//! Letronna as an MCP server: `letronna mcp serve`.
//!
//! Other agents and editors already speak MCP, so the browser, the media
//! tools and search are offered to them the way `mcp.rs` takes tools from
//! other servers: JSON-RPC 2.0 over stdin and stdout, one message a line,
//! written by that module's own framing so the two ends cannot drift apart.
//! Only the tools that look at or make things elsewhere are offered. The
//! file, command and messaging tools are left out, since whoever connects
//! is a program that can already reach this computer in its own ways, and
//! handing it a second way in would only skip Letronna's cards.
//!
//! Nobody is watching a card here, so a tool that needs a yes is refused
//! with a result saying so, unless the server was started with `--yes`.
//! Even then every call is put to the gate as a session with nobody to ask
//! would put it, in Bypass: `--yes` answers the cards, not the user's deny
//! and ask rules, which still refuse, as a rules or hooks file that cannot
//! be read does.
//! Calls are answered one at a time, in order: the browser tools must never
//! run beside each other, and a client that needs more at once can start a
//! second server. A tool that fails answers with `isError`, so the model on
//! the other side reads why; a name that is not offered is the protocol's
//! own error, as the spec has it.
//!
//! With `--allow-turn`, one more tool, `letronna_turn`, runs a whole turn of
//! Letronna's own agent on the prompt it is given and returns its answer.
//! The frontend supplies the turn, since only it knows the session, the
//! model and how approvals inside the turn are answered.

use crate::chat::ToolCall;
use crate::gate::{self, Gate, Grants, Mode, Outcome};
use crate::mcp::{PROTOCOL, write_message};
use crate::permissions;
use crate::tools::{self, Tool, ToolContext};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::sync::Arc;

/// The tools offered by name, from `tools::all()`.
pub const EXPOSED: &[&str] = &[
    "web_fetch",
    "browse",
    "use_page",
    "make_image",
    "speak",
    "transcribe",
    "make_video",
    "search_web",
];

/// The tool that runs a whole turn, offered only with `--allow-turn`.
pub const TURN: &str = "letronna_turn";

/// Revisions a client may ask for that the answers here also fit; any other
/// is answered with `PROTOCOL`, and the client decides whether to go on.
const SPOKEN: &[&str] = &[PROTOCOL, "2025-03-26", "2024-11-05"];

/// The conversation served calls are made in. Conversation 0 is the one
/// that always takes the browser's shared session, sign-ins and all; this
/// id, which no session (counting up from 1) nor helper (from 2^40) meets,
/// gets a private browser of its own unless the user shares one browser
/// between every conversation.
const CALLER: u64 = u64::MAX;

/// Runs one turn on a prompt and gives its final text.
pub type Turn = Box<dyn FnMut(&str) -> Result<String, String>>;

/// What one server offers and how it answers.
pub struct Server {
    tools: Vec<Arc<dyn Tool>>,
    /// `--yes`: tools that need a yes run without one.
    yes: bool,
    turn: Option<Turn>,
    ctx: ToolContext,
}

/// Letronna's own tools that are offered, among those this session can use.
pub fn exposed() -> Vec<Arc<dyn Tool>> {
    let connection = crate::chat::Connection {
        workspace: std::env::current_dir().ok(),
        ..Default::default()
    };
    let disabled = crate::store::disabled_tools();
    tools::all()
        .into_iter()
        .filter(|tool| EXPOSED.contains(&tool.name()) && tool.offered(&connection))
        .filter(|tool| !tools::switched_off(tool.name(), &disabled))
        .collect()
}

impl Server {
    /// A server offering `tools`, and the turn when there is one.
    pub fn new(tools: Vec<Arc<dyn Tool>>, yes: bool, turn: Option<Turn>) -> Self {
        Self {
            tools,
            yes,
            turn,
            ctx: ToolContext {
                workspace: std::env::current_dir().ok(),
                conversation: CALLER,
                ..Default::default()
            },
        }
    }

    /// Answers one message, or `None` for a notification or a reply.
    pub fn answer(&mut self, message: &Value) -> Option<Value> {
        // Replies to nothing we asked, and notifications, `initialized`
        // among them, need no answer.
        // A batch, which one revision allowed, or anything else that is not
        // one message would otherwise leave the client waiting.
        if !message.is_object() {
            let why = "Letronna answers one JSON-RPC object a line, not a batch.";
            return Some(failure(Value::Null, -32600, why));
        }
        let method = message.get("method").and_then(Value::as_str)?;
        let id = message.get("id").cloned()?;
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(self.greeting(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": self.listing()})),
            "tools/call" => self.call(&params),
            other => Err((-32601, format!("Letronna does not serve `{other}`."))),
        };
        Some(match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, why)) => failure(id, code, &why),
        })
    }

    fn greeting(&self, params: &Value) -> Value {
        let asked = params["protocolVersion"].as_str().unwrap_or_default();
        let version = SPOKEN
            .iter()
            .find(|spoken| **spoken == asked)
            .unwrap_or(&PROTOCOL);
        json!({
            "protocolVersion": version,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "letronna", "version": env!("CARGO_PKG_VERSION")},
        })
    }

    fn listing(&self) -> Vec<Value> {
        let mut listed: Vec<Value> = self
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name(),
                    "description": tool.description(),
                    "inputSchema": tool.parameters(),
                })
            })
            .collect();
        if self.turn.is_some() {
            listed.push(json!({
                "name": TURN,
                "description": "Run a whole turn of the Letronna agent on a prompt, with its own tools, and get its final answer.",
                "inputSchema": {"type": "object", "properties": {
                    "prompt": {"type": "string", "description": "What to ask Letronna to do."}
                }, "required": ["prompt"]},
            }));
        }
        listed
    }

    /// Runs the named tool, its failures as results the caller reads.
    fn call(&mut self, params: &Value) -> Result<Value, (i64, String)> {
        let Some(name) = params["name"].as_str() else {
            return Err((-32602, "tools/call needs the tool's name.".into()));
        };
        let args = match params.get("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(args) => args.clone(),
        };
        if name == TURN
            && let Some(turn) = self.turn.as_mut()
        {
            let Some(prompt) = args["prompt"].as_str().filter(|p| !p.trim().is_empty()) else {
                return Ok(outcome(Err("letronna_turn needs a prompt.".into())));
            };
            // Every result inside the turn was scrubbed, but its answer is
            // the model's own words, which reach another program here.
            let said = turn(prompt)
                .map(|text| crate::redact::scrub(&text, false))
                .map_err(|why| crate::redact::scrub(&why, false));
            return Ok(outcome(said));
        }
        let Some(tool) = self.tools.iter().find(|tool| tool.name() == name) else {
            return Err((
                -32602,
                format!("Letronna does not offer a tool called `{name}`."),
            ));
        };
        if tool.needs_approval() && !self.yes {
            return Ok(outcome(Err(format!(
                "`{name}` needs a yes, and `letronna mcp serve` has nobody to ask. \
                 Start it with --yes to let its tools run unasked."
            ))));
        }
        if let Some(why) = self.refused(name, &args) {
            return Ok(outcome(Err(why)));
        }
        let hooks = crate::hooks::current();
        let ran = tools::run_found(tool.as_ref(), &args.to_string(), &self.ctx, &hooks);
        Ok(outcome(ran))
    }

    /// Why the gate would not run the call, as it decides for a session with
    /// nobody to ask: in Bypass under `--yes`, with the user's rules read
    /// afresh, since the window may have added one while this server ran.
    fn refused(&self, name: &str, args: &Value) -> Option<String> {
        let rules = permissions::load(&permissions::user_file());
        let mode = if self.yes {
            Mode::Bypass
        } else {
            Mode::Default
        };
        let none = Grants::new();
        let gate = Gate::for_terminal(&self.ctx, &none, &rules, mode, false);
        let call = ToolCall {
            name: name.to_owned(),
            arguments: args.to_string(),
            ..Default::default()
        };
        match gate::decide(&call, &gate) {
            Outcome::Run => None,
            Outcome::Refuse(why) => Some(why),
            // With nobody to answer the gate refuses rather than asks; a
            // card here would be a yes nobody gave.
            Outcome::Ask => Some(format!(
                "`{name}` needs a yes, and nobody is here to give it."
            )),
        }
    }
}

/// A call's result as MCP gives it: its text, and whether it failed.
fn outcome(ran: Result<String, String>) -> Value {
    let (text, failed) = match ran {
        Ok(text) => (text, false),
        Err(why) => (why, true),
    };
    json!({"content": [{"type": "text", "text": text}], "isError": failed})
}

fn failure(id: Value, code: i64, why: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": why}})
}

/// Answers every message read from `from` on `to` until `from` ends.
pub fn serve(server: &mut Server, from: impl BufRead, mut to: impl Write) -> Result<(), String> {
    // Bytes rather than lines of text, so one line that is not UTF-8 is
    // answered as a parse error instead of ending the server.
    for line in from.split(b'\n') {
        let line = line.map_err(|e| format!("Could not read the client: {e}"))?;
        let reply = match std::str::from_utf8(&line) {
            Err(_) => Some(failure(Value::Null, -32700, "Not UTF-8.")),
            Ok(text) if text.trim().is_empty() => continue,
            Ok(text) => match serde_json::from_str::<Value>(text) {
                Ok(message) => server.answer(&message),
                Err(e) => Some(failure(Value::Null, -32700, &format!("Not JSON: {e}"))),
            },
        };
        if let Some(reply) = reply {
            write_message(&mut to, &reply)
                .map_err(|e| format!("Could not answer the client: {e}"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tool that says back what it was given, and needs no yes.
    struct Echo;
    impl Tool for Echo {
        fn name(&self) -> &'static str {
            "echo"
        }
        fn description(&self) -> &'static str {
            "Say it back."
        }
        fn parameters(&self) -> Value {
            json!({"type": "object", "properties": {"text": {"type": "string"}}})
        }
        fn run(&self, args: &Value, _: &ToolContext) -> Result<String, String> {
            Ok(args["text"].as_str().unwrap_or_default().to_owned())
        }
    }

    /// A tool that would do something, and so needs a yes.
    struct Risky;
    impl Tool for Risky {
        fn name(&self) -> &'static str {
            "risky"
        }
        fn description(&self) -> &'static str {
            "Do something."
        }
        fn parameters(&self) -> Value {
            json!({"type": "object"})
        }
        fn run(&self, _: &Value, _: &ToolContext) -> Result<String, String> {
            Ok("did it".into())
        }
        fn needs_approval(&self) -> bool {
            true
        }
    }

    /// A server offering the two tools above, which the gate finds by name
    /// on this test's thread as it finds an MCP server's.
    fn server(yes: bool, turn: Option<Turn>) -> Server {
        let offered: Vec<Arc<dyn Tool>> = vec![Arc::new(Echo), Arc::new(Risky)];
        crate::mcp::serve(offered.clone());
        Server::new(offered, yes, turn)
    }

    /// An empty folder for one test, which removes it when done.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("letronna-mcp-serve-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".letronna")).unwrap();
        dir
    }

    /// Sends `lines` through `serve` and reads back each answer.
    fn round_trip(server: &mut Server, lines: &[Value]) -> Vec<Value> {
        let input: String = lines.iter().map(|line| format!("{line}\n")).collect();
        let mut output = Vec::new();
        assert_eq!(serve(server, input.as_bytes(), &mut output), Ok(()));
        String::from_utf8_lossy(&output)
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn call(id: u64, name: &str, arguments: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}})
    }

    #[test]
    fn a_client_is_greeted_listed_and_called_one_line_each() {
        let answers = round_trip(
            &mut server(false, None),
            &[
                json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                    "params": {"protocolVersion": PROTOCOL, "capabilities": {},
                        "clientInfo": {"name": "test", "version": "0"}}}),
                json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
                call(3, "echo", json!({"text": "hello"})),
            ],
        );
        // The notification gets no answer, so three lines for three requests.
        assert_eq!(answers.len(), 3);
        assert_eq!(answers[0]["id"], 1);
        assert_eq!(answers[0]["result"]["protocolVersion"], PROTOCOL);
        assert_eq!(answers[0]["result"]["serverInfo"]["name"], "letronna");
        let names: Vec<&str> = answers[1]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["echo", "risky"]);
        assert_eq!(
            answers[1]["result"]["tools"][0]["inputSchema"]["type"],
            "object"
        );
        assert_eq!(answers[2]["result"]["isError"], false);
        assert_eq!(answers[2]["result"]["content"][0]["text"], "hello");
    }

    #[test]
    fn served_calls_are_made_in_a_conversation_of_their_own() {
        // Not 0, which takes the browser's shared session whatever the
        // setting, nor any session's or helper's.
        let served = server(true, None);
        assert_eq!(served.ctx.conversation, CALLER);
        assert_ne!(served.ctx.conversation, 0);
    }

    #[test]
    fn a_client_asking_an_older_revision_is_answered_in_it() {
        let greeting = server(false, None)
            .answer(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"protocolVersion": "2024-11-05"}}))
            .unwrap();
        assert_eq!(greeting["result"]["protocolVersion"], "2024-11-05");
        let greeting = server(false, None)
            .answer(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"protocolVersion": "1999-01-01"}}))
            .unwrap();
        assert_eq!(greeting["result"]["protocolVersion"], PROTOCOL);
    }

    #[test]
    fn a_tool_that_is_not_offered_is_the_protocols_error() {
        let answer = server(true, None)
            .answer(&call(4, "run_command", json!({"command": "ls"})))
            .unwrap();
        assert_eq!(answer["id"], 4);
        assert_eq!(answer["error"]["code"], -32602);
        assert!(
            answer["error"]["message"]
                .as_str()
                .unwrap()
                .contains("run_command")
        );
    }

    #[test]
    fn a_tool_that_needs_a_yes_is_refused_as_a_result_unless_started_with_yes() {
        let refused = server(false, None)
            .answer(&call(5, "risky", json!({})))
            .unwrap();
        assert!(refused.get("error").is_none());
        assert_eq!(refused["result"]["isError"], true);
        let why = refused["result"]["content"][0]["text"].as_str().unwrap();
        assert!(why.contains("--yes"), "{why}");
        let ran = server(true, None)
            .answer(&call(6, "risky", json!({})))
            .unwrap();
        assert_eq!(ran["result"]["isError"], false);
        assert_eq!(ran["result"]["content"][0]["text"], "did it");
    }

    #[test]
    fn the_users_deny_and_ask_rules_refuse_a_served_call_even_with_yes() {
        let project = scratch("rules");
        std::fs::write(
            project.join(permissions::PROJECT_FILE),
            r#"[{"tool": "risky", "effect": "deny"}, {"tool": "echo", "effect": "ask"}]"#,
        )
        .unwrap();
        let mut served = server(true, None);
        served.ctx.workspace = Some(project.clone());
        let denied = served.answer(&call(7, "risky", json!({}))).unwrap();
        let asked = served
            .answer(&call(8, "echo", json!({"text": "hi"})))
            .unwrap();
        let _ = std::fs::remove_dir_all(&project);
        assert_eq!(denied["result"]["isError"], true);
        let why = denied["result"]["content"][0]["text"].as_str().unwrap();
        assert!(why.contains("permission rule denies"), "{why}");
        // An ask rule has nobody to ask, so it is a no, not a quiet yes.
        assert_eq!(asked["result"]["isError"], true);
        let why = asked["result"]["content"][0]["text"].as_str().unwrap();
        assert!(why.contains("nobody"), "{why}");
    }

    #[test]
    fn the_turn_is_offered_only_when_given_and_answers_with_its_text() {
        let listed = |server: &mut Server| {
            let answer = server
                .answer(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
                .unwrap();
            answer["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == TURN)
        };
        assert!(!listed(&mut server(false, None)));
        let turn: Turn = Box::new(|prompt| match prompt {
            "fail" => Err("The model is not set up.".into()),
            _ => Ok(format!("done: {prompt}")),
        });
        let mut with_turn = server(false, Some(turn));
        assert!(listed(&mut with_turn));
        let answered = with_turn
            .answer(&call(2, TURN, json!({"prompt": "tidy"})))
            .unwrap();
        assert_eq!(answered["result"]["content"][0]["text"], "done: tidy");
        let failed = with_turn
            .answer(&call(3, TURN, json!({"prompt": "fail"})))
            .unwrap();
        assert_eq!(failed["result"]["isError"], true);
        let empty = with_turn.answer(&call(4, TURN, json!({}))).unwrap();
        assert_eq!(empty["result"]["isError"], true);
        // Without the turn the name is as unknown as any other.
        let unknown = server(false, None)
            .answer(&call(5, TURN, json!({"prompt": "x"})))
            .unwrap();
        assert_eq!(unknown["error"]["code"], -32602);
    }

    #[test]
    fn a_line_that_is_not_json_and_an_unknown_method_are_answered_with_errors() {
        let mut output = Vec::new();
        let input = "not json\n{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"resources/list\"}\n";
        assert_eq!(
            serve(&mut server(false, None), input.as_bytes(), &mut output),
            Ok(())
        );
        let answers: Vec<Value> = String::from_utf8_lossy(&output)
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(answers[0]["error"]["code"], -32700);
        assert_eq!(answers[0]["id"], Value::Null);
        assert_eq!(answers[1]["error"]["code"], -32601);
        assert_eq!(answers[1]["id"], 9);
    }

    #[test]
    fn a_line_that_is_not_utf8_or_not_one_object_is_answered_and_reading_goes_on() {
        let mut output = Vec::new();
        let input: &[u8] = b"\xff\xfe\n[{\"jsonrpc\":\"2.0\",\"id\":8,\"method\":\"ping\"}]\n\
            {\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"ping\"}\n";
        assert_eq!(serve(&mut server(false, None), input, &mut output), Ok(()));
        let answers: Vec<Value> = String::from_utf8_lossy(&output)
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(answers.len(), 3);
        assert_eq!(answers[0]["error"]["code"], -32700);
        assert_eq!(answers[1]["error"]["code"], -32600);
        assert_eq!(answers[2]["id"], 9);
        assert_eq!(answers[2]["result"], json!({}));
    }

    #[test]
    fn the_turns_answer_has_the_users_keys_hidden() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let data = scratch("keys");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &data) };
        let keys = r#"{"OpenAI":"a-saved-key-of-no-known-shape"}"#;
        std::fs::write(data.join("keys.json"), keys).unwrap();
        let turn: Turn = Box::new(|_| Ok("It is a-saved-key-of-no-known-shape.".into()));
        let answered = server(false, Some(turn))
            .answer(&call(2, TURN, json!({"prompt": "which key?"})))
            .unwrap();
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
        let _ = std::fs::remove_dir_all(&data);
        let text = answered["result"]["content"][0]["text"].as_str().unwrap();
        assert!(!text.contains("a-saved-key"), "{text}");
        assert!(text.contains(crate::redact::MARKER), "{text}");
    }

    #[test]
    fn only_the_web_browser_media_and_search_tools_are_offered() {
        let offered: Vec<&str> = exposed().iter().map(|tool| tool.name()).collect();
        assert!(
            offered.iter().all(|name| EXPOSED.contains(name)),
            "{offered:?}"
        );
        for kept_out in [
            "read_file",
            "write_file",
            "run_command",
            "read_chat",
            "send_file",
        ] {
            assert!(!offered.contains(&kept_out), "{kept_out}");
            assert!(
                tools::find(kept_out).is_some(),
                "{kept_out} is still a tool"
            );
        }
        assert!(offered.contains(&"web_fetch") && offered.contains(&"browse"));
    }
}
