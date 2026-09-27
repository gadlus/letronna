//! Tools from MCP servers.
//!
//! The Model Context Protocol is how agents now take on tools they were not
//! built with: a browser, GitHub, a database, a company's own services.
//! Servers listed in `mcp.json` in the data folder -- the shape Claude Desktop
//! and Cursor read, so a config can be copied across -- run as child
//! processes, spoken to over their stdin and stdout one JSON-RPC message per
//! line. Their tools join Letronna's own as `mcp__<server>__<tool>`.
//!
//! Starting a server can take a while (npx may be downloading it first), so
//! the servers start on threads of their own the first time the tools are
//! listed, and their tools appear once they answer; only a question asked
//! once waits for them, as it goes out at once.
//! Every call asks the user first unless the config's `autoApprove` names the
//! tool: a server is somebody else's code, and it alone says what its tools
//! do. They are never offered where other people can read the replies.

use crate::tools::{Tool, ToolContext};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Once, OnceLock, mpsc};
use std::time::Duration;

/// How long a server gets to start and list its tools. The first run of an
/// npx server downloads it, which is most of this.
const START_TIME: Duration = Duration::from_secs(60);
/// How long one tool call may take, the same as a command.
const CALL_TIME: Duration = Duration::from_secs(120);
/// The protocol revision asked for; a server answers with one it speaks.
const PROTOCOL: &str = "2025-06-18";

/// `mcp.json`, as Claude Desktop writes it.
#[derive(Deserialize, Default)]
struct Config {
    #[serde(default, rename = "mcpServers")]
    servers: BTreeMap<String, ServerConfig>,
}

#[derive(Deserialize, Clone, Default)]
struct ServerConfig {
    /// Empty for a server reached by URL, which is not supported yet.
    #[serde(default)]
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    disabled: bool,
    /// Tools that run without asking, by their name on the server.
    #[serde(default, rename = "autoApprove")]
    auto_approve: Vec<String>,
}

/// The tools of every server that has answered so far.
pub fn tools() -> Vec<Arc<dyn Tool>> {
    start();
    registry()
        .lock()
        .map(|known| known.clone())
        .unwrap_or_default()
}

/// Start the configured servers, the first time only, without waiting for
/// them. The frontends call this as they open, so that servers which take
/// seconds to start have their tools ready by the first message.
pub fn start() {
    // Tests list the tools all the time; they must not start whatever servers
    // happen to be configured on the machine running them.
    if cfg!(test) {
        return;
    }
    static STARTED: Once = Once::new();
    STARTED.call_once(|| {
        let path = crate::chat::data_dir().join("mcp.json");
        let config: Config = std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        for (name, server) in config.servers {
            if server.disabled || server.command.trim().is_empty() {
                continue;
            }
            begin_start();
            std::thread::spawn(move || {
                if let Ok(tools) = launch(&name, &server)
                    && let Ok(mut known) = registry().lock()
                {
                    known.extend(tools);
                }
                end_start();
            });
        }
    });
}

/// Wait until every server has answered or given up, for at most `limit`:
/// for a question asked once, whose first request would otherwise go out
/// before any server's tools are there.
pub fn ready(limit: Duration) {
    start();
    wait_started(limit);
}

/// How many servers are still starting, and a signal as each is done.
fn starting() -> &'static (Mutex<usize>, Condvar) {
    static STARTING: OnceLock<(Mutex<usize>, Condvar)> = OnceLock::new();
    STARTING.get_or_init(Default::default)
}

fn begin_start() {
    if let Ok(mut left) = starting().0.lock() {
        *left += 1;
    }
}

fn end_start() {
    let (left, done) = starting();
    if let Ok(mut left) = left.lock() {
        *left = left.saturating_sub(1);
    }
    done.notify_all();
}

fn wait_started(limit: Duration) {
    let (left, done) = starting();
    if let Ok(left) = left.lock() {
        let _ = done.wait_timeout_while(left, limit, |left| *left > 0);
    }
}

fn registry() -> &'static Mutex<Vec<Arc<dyn Tool>>> {
    static KNOWN: OnceLock<Mutex<Vec<Arc<dyn Tool>>>> = OnceLock::new();
    KNOWN.get_or_init(Default::default)
}

/// Start one server and list its tools.
fn launch(name: &str, config: &ServerConfig) -> Result<Vec<Arc<dyn Tool>>, String> {
    let mut child = spawn(config)?;
    let (Some(stdin), Some(stdout), Some(mut stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err(format!("{name} has no pipes to talk over."));
    };
    // A server that fills its stderr pipe stops until someone reads it.
    std::thread::spawn(move || std::io::copy(&mut stderr, &mut std::io::sink()));
    connect(name, config, stdout, stdin)
}

/// The server's process, with its pipes.
fn spawn(config: &ServerConfig) -> Result<std::process::Child, String> {
    use std::process::{Command, Stdio};
    let build = |program: &str, args: &[String]| {
        let mut command = Command::new(program);
        command
            .args(args)
            .envs(&config.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Letronna owns no console, so without this every server would open
        // a console window of its own.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        command
    };
    let failed = |e: std::io::Error| format!("Cannot start `{}`: {e}", config.command);
    match build(&config.command, &config.args).spawn() {
        Ok(child) => Ok(child),
        // `npx`, `uvx` and friends are .cmd scripts on Windows, which only
        // the shell finds.
        Err(e) if cfg!(windows) && e.kind() == std::io::ErrorKind::NotFound => {
            let mut args = vec!["/C".to_owned(), config.command.clone()];
            args.extend(config.args.iter().cloned());
            build("cmd", &args).spawn().map_err(failed)
        }
        Err(e) => Err(failed(e)),
    }
}

/// Greet a server over its pipes and wrap each tool it lists.
fn connect(
    name: &str,
    config: &ServerConfig,
    from: impl Read + Send + 'static,
    to: impl Write + Send + 'static,
) -> Result<Vec<Arc<dyn Tool>>, String> {
    let server = Arc::new(Server {
        name: name.to_owned(),
        to: Mutex::new(Box::new(to)),
        waiting: Mutex::default(),
        next: AtomicU64::new(1),
    });
    let reader = server.clone();
    std::thread::spawn(move || reader.listen(from));
    server.request(
        "initialize",
        json!({
            "protocolVersion": PROTOCOL,
            "capabilities": {},
            "clientInfo": {"name": "letronna", "version": env!("CARGO_PKG_VERSION")},
        }),
        START_TIME,
    )?;
    server.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let params = match &cursor {
            Some(cursor) => json!({ "cursor": cursor }),
            None => json!({}),
        };
        let page = server.request("tools/list", params, START_TIME)?;
        for tool in page["tools"].as_array().into_iter().flatten() {
            let Some(own) = tool["name"].as_str() else {
                continue;
            };
            tools.push(Arc::new(McpTool {
                server: server.clone(),
                own: own.to_owned(),
                name: keep(tool_name(name, own)),
                description: keep(format!(
                    "{} (from the {name} MCP server)",
                    tool["description"].as_str().unwrap_or(own)
                )),
                schema: schema(&tool["inputSchema"]),
                ask: !config.auto_approve.iter().any(|t| t == own),
                read_only: tool["annotations"]["readOnlyHint"].as_bool() == Some(true),
            }));
        }
        match page["nextCursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
    }
    Ok(tools)
}

/// One running server, as seen from this side of its pipes.
struct Server {
    name: String,
    to: Mutex<Box<dyn Write + Send>>,
    /// Requests sent and not yet answered, by id.
    waiting: Mutex<HashMap<u64, mpsc::Sender<Result<Value, String>>>>,
    next: AtomicU64,
}

impl Server {
    fn send(&self, message: &Value) -> Result<(), String> {
        let mut line = message.to_string();
        line.push('\n');
        let gone = || format!("{} has stopped.", self.name);
        let mut to = self.to.lock().map_err(|_| gone())?;
        to.write_all(line.as_bytes())
            .and_then(|_| to.flush())
            .map_err(|_| gone())
    }

    /// Send a request and wait for its answer.
    fn request(&self, method: &str, params: Value, wait: Duration) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (answer, answered) = mpsc::channel();
        if let Ok(mut waiting) = self.waiting.lock() {
            waiting.insert(id, answer);
        }
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        answered.recv_timeout(wait).unwrap_or_else(|_| {
            if let Ok(mut waiting) = self.waiting.lock() {
                waiting.remove(&id);
            }
            Err(format!(
                "{} did not answer {method} within {} seconds.",
                self.name,
                wait.as_secs()
            ))
        })
    }

    /// Read what the server says until it goes, handing each answer to
    /// whoever asked for it.
    fn listen(&self, from: impl Read) {
        for line in BufReader::new(from).lines() {
            let Ok(line) = line else {
                break;
            };
            // Some servers print other things on stdout; they are not ours.
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            match (message.get("id"), message.get("method")) {
                // The server asking something of us: a ping is answered, and
                // the rest -- sampling, roots -- are turned down.
                (Some(id), Some(method)) => {
                    let reply = if method == "ping" {
                        json!({"jsonrpc": "2.0", "id": id, "result": {}})
                    } else {
                        json!({"jsonrpc": "2.0", "id": id,
                            "error": {"code": -32601, "message": "Letronna does not offer this."}})
                    };
                    let _ = self.send(&reply);
                }
                (Some(id), None) => {
                    let Some(id) = id.as_u64() else {
                        continue;
                    };
                    let asker = self.waiting.lock().ok().and_then(|mut w| w.remove(&id));
                    let outcome = match message.get("error") {
                        Some(error) => Err(error["message"]
                            .as_str()
                            .unwrap_or("The server reported an error.")
                            .to_owned()),
                        None => Ok(message["result"].clone()),
                    };
                    if let Some(asker) = asker {
                        let _ = asker.send(outcome);
                    }
                }
                // Notifications: progress, logging, changed lists.
                _ => {}
            }
        }
        // Gone: anyone still waiting hears it now rather than at a timeout.
        if let Ok(mut waiting) = self.waiting.lock() {
            for (_, asker) in waiting.drain() {
                let _ = asker.send(Err(format!("{} stopped.", self.name)));
            }
        }
    }
}

/// One of a server's tools, offered to the model under Letronna's name for it.
struct McpTool {
    server: Arc<Server>,
    /// The tool's name on its server.
    own: String,
    name: &'static str,
    description: &'static str,
    schema: Value,
    ask: bool,
    read_only: bool,
}

impl Tool for McpTool {
    fn name(&self) -> &'static str {
        self.name
    }
    fn description(&self) -> &'static str {
        self.description
    }
    fn parameters(&self) -> Value {
        self.schema.clone()
    }
    fn run(&self, args: &Value, _: &ToolContext) -> Result<String, String> {
        let result = self.server.request(
            "tools/call",
            json!({"name": self.own, "arguments": args}),
            CALL_TIME,
        )?;
        let text = text_of(&result);
        if result["isError"].as_bool() == Some(true) {
            Err(text)
        } else {
            Ok(text)
        }
    }
    fn needs_approval(&self) -> bool {
        self.ask
    }
    fn mutates(&self) -> bool {
        !self.read_only
    }
    fn preview(&self, args: &Value, _: &ToolContext) -> String {
        let args = serde_json::to_string_pretty(args).unwrap_or_default();
        format!(
            "{} on the {} MCP server\n{args}",
            self.own, self.server.name
        )
    }
}

/// What a tool call returned, as text: the text parts joined, anything else
/// named by its type so the model knows it was there.
fn text_of(result: &Value) -> String {
    let parts: Vec<String> = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|part| match part["type"].as_str().unwrap_or_default() {
            "text" => part["text"].as_str().unwrap_or_default().to_owned(),
            "resource" => part["resource"]["text"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    let uri = part["resource"]["uri"].as_str().unwrap_or("?");
                    format!("[resource {uri}]")
                }),
            kind => {
                let mime = part["mimeType"].as_str().unwrap_or(kind);
                let bytes = part["data"].as_str().map_or(0, |d| d.len() * 3 / 4);
                format!("[{mime}, {bytes} bytes]")
            }
        })
        .filter(|part| !part.is_empty())
        .collect();
    if !parts.is_empty() {
        return parts.join("\n");
    }
    match result.get("structuredContent") {
        Some(structured) => structured.to_string(),
        None => "(no output)".into(),
    }
}

/// A tool's input schema as providers take it: an object, and without the
/// `$schema` key some of them refuse.
fn schema(input: &Value) -> Value {
    let mut schema = match input {
        Value::Object(_) => input.clone(),
        _ => json!({}),
    };
    if let Value::Object(map) = &mut schema {
        map.remove("$schema");
        map.entry("type").or_insert_with(|| json!("object"));
        map.entry("properties").or_insert_with(|| json!({}));
    }
    schema
}

/// `mcp__<server>__<tool>`, in the letters providers accept in a tool name
/// and no longer than the 64 they allow.
fn tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

/// A string kept for the rest of the run. Tool names and descriptions must
/// be `'static`, and an MCP server's arrive while Letronna runs; each
/// different one is leaked once and then reused, so it costs one copy.
fn keep(text: String) -> &'static str {
    static KEPT: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let Ok(mut kept) = KEPT.get_or_init(Default::default).lock() else {
        return Box::leak(text.into_boxed_str());
    };
    if let Some(&known) = kept.get(text.as_str()) {
        return known;
    }
    let leaked: &'static str = Box::leak(text.into_boxed_str());
    kept.insert(leaked);
    leaked
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server that answers the handshake, lists its tools over two pages,
    /// pings the client once, and echoes calls back.
    fn scripted_server(from_client: std::io::PipeReader, mut to_client: std::io::PipeWriter) {
        std::thread::spawn(move || {
            let reply = |to: &mut std::io::PipeWriter, message: Value| {
                writeln!(to, "{message}").unwrap();
            };
            for line in BufReader::new(from_client).lines() {
                let message: Value = serde_json::from_str(&line.unwrap()).unwrap();
                let id = message["id"].clone();
                match message["method"].as_str() {
                    Some("initialize") => {
                        writeln!(to_client, "server log line, not JSON").unwrap();
                        reply(
                            &mut to_client,
                            json!({"jsonrpc": "2.0", "method": "ping", "id": "p1"}),
                        );
                        reply(
                            &mut to_client,
                            json!({"jsonrpc": "2.0", "id": id, "result": {
                                "protocolVersion": PROTOCOL, "capabilities": {"tools": {}},
                                "serverInfo": {"name": "demo", "version": "1"}}}),
                        );
                    }
                    Some("tools/list") if message["params"]["cursor"].is_null() => reply(
                        &mut to_client,
                        json!({"jsonrpc": "2.0", "id": id, "result": {"nextCursor": "2", "tools": [
                            {"name": "echo", "description": "Say it back.",
                             "inputSchema": {"$schema": "https://json-schema.org/draft/2020-12/schema",
                                 "type": "object", "properties": {"text": {"type": "string"}}}}]}}),
                    ),
                    Some("tools/list") => reply(
                        &mut to_client,
                        json!({"jsonrpc": "2.0", "id": id, "result": {"tools": [
                            {"name": "look up", "annotations": {"readOnlyHint": true}}]}}),
                    ),
                    Some("tools/call") => {
                        let text = message["params"]["arguments"]["text"]
                            .as_str()
                            .unwrap_or("");
                        let failed = text == "fail";
                        reply(
                            &mut to_client,
                            json!({"jsonrpc": "2.0", "id": id, "result": {
                                "content": [{"type": "text", "text": format!("echo: {text}")},
                                            {"type": "image", "mimeType": "image/png", "data": "AAAA"}],
                                "isError": failed}}),
                        );
                    }
                    // The client's answer to our ping, and its notification.
                    _ => {}
                }
            }
        });
    }

    #[test]
    fn a_question_asked_once_waits_for_servers_still_starting() {
        begin_start();
        let asked = std::time::Instant::now();
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(100));
            end_start();
        });
        wait_started(Duration::from_secs(5));
        let waited = asked.elapsed();
        assert!(waited >= Duration::from_millis(100), "{waited:?}");
        assert!(waited < Duration::from_secs(2), "{waited:?}");

        // One that never answers is waited for no longer than asked.
        begin_start();
        let asked = std::time::Instant::now();
        wait_started(Duration::from_millis(200));
        assert!(asked.elapsed() < Duration::from_secs(2));
        end_start();
    }

    #[test]
    fn a_server_is_greeted_listed_and_called() {
        let (from_client, to_server) = std::io::pipe().unwrap();
        let (from_server, to_client) = std::io::pipe().unwrap();
        scripted_server(from_client, to_client);
        let config = ServerConfig {
            auto_approve: vec!["look up".into()],
            ..Default::default()
        };
        let tools = connect("demo", &config, from_server, to_server).unwrap();

        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(names, ["mcp__demo__echo", "mcp__demo__look_up"]);
        let (echo, look_up) = (&tools[0], &tools[1]);
        assert_eq!(
            echo.description(),
            "Say it back. (from the demo MCP server)"
        );
        assert!(echo.parameters().get("$schema").is_none());
        assert!(echo.needs_approval() && echo.mutates());
        assert!(!look_up.needs_approval() && !look_up.mutates());
        assert_eq!(look_up.parameters()["type"], "object");

        let ctx = ToolContext::default();
        assert_eq!(
            echo.run(&json!({"text": "hi"}), &ctx).unwrap(),
            "echo: hi\n[image/png, 3 bytes]"
        );
        assert_eq!(
            echo.run(&json!({"text": "fail"}), &ctx).unwrap_err(),
            "echo: fail\n[image/png, 3 bytes]"
        );
    }

    #[test]
    fn a_server_that_stops_fails_its_callers_at_once() {
        let (from_client, to_server) = std::io::pipe().unwrap();
        let (from_server, to_client) = std::io::pipe().unwrap();
        // Answers nothing, and hangs up as soon as it hears anything.
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = BufReader::new(from_client).read_line(&mut line);
            drop(to_client);
        });
        let started = std::time::Instant::now();
        let Err(err) = connect("quiet", &ServerConfig::default(), from_server, to_server) else {
            panic!("a server that hung up counted as connected");
        };
        assert_eq!(err, "quiet stopped.");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn names_are_ones_providers_accept() {
        assert_eq!(
            tool_name("git hub", "list.repos"),
            "mcp__git_hub__list_repos"
        );
        assert_eq!(tool_name("s", &"x".repeat(100)).len(), 64);
        let config: Config = serde_json::from_str(
            r#"{"mcpServers": {"fs": {"command": "npx", "args": ["-y", "pkg"], "autoApprove": ["read"]},
                               "web": {"url": "https://example.com/mcp"}}}"#,
        )
        .unwrap();
        assert_eq!(config.servers["fs"].args, ["-y", "pkg"]);
        assert_eq!(config.servers["fs"].auto_approve, ["read"]);
        // A server reached by URL has no command, and is passed over.
        assert!(config.servers["web"].command.is_empty());
    }

    #[test]
    #[ignore = "Starts @modelcontextprotocol/server-everything with npx: needs Node and the network"]
    fn talks_to_a_real_server() {
        let config = ServerConfig {
            command: "npx".into(),
            args: vec![
                "-y".into(),
                "@modelcontextprotocol/server-everything".into(),
            ],
            ..Default::default()
        };
        let tools = launch("everything", &config).unwrap();
        let echo = tools
            .iter()
            .find(|t| t.name() == "mcp__everything__echo")
            .expect("the everything server has an echo tool");
        let said = echo
            .run(&json!({"message": "hello"}), &ToolContext::default())
            .unwrap();
        assert!(said.contains("hello"), "{said}");
    }
}
