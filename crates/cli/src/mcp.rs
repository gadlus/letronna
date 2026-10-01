//! `letronna mcp`: add, list, get and remove the MCP servers in `mcp.json`,
//! and `serve`, which offers Letronna's own tools to other MCP clients.
//!
//! The flags are Claude Code's own, so a server's README that says `claude
//! mcp add ...` works with `letronna` in its place. Letronna keeps one list
//! for every folder, so `user` is the only scope it takes. An app's secret
//! is refused as a flag, where it would sit in the shell's history; it goes
//! in mcp.json as a `${VAR}`.

use crate::bar::Bar;
use crate::render;
use crate::session::Runner;
use letronna_core::approvals::{self, Decision};
use letronna_core::chat::{self, Message, ToolCall};
use letronna_core::driver::{self, Allow, Outcome, Spec};
use letronna_core::gate::{self, Gate, Grants, Mode};
use letronna_core::mcp::{self, ServerStatus, State};
use letronna_core::mcp_serve;
use letronna_core::permissions::{self, Rule};
use letronna_core::store::Transcript;
use letronna_core::tools::ToolContext;
use serde_json::{Map, Value, json};
use std::time::Duration;

/// How long `list` and `get` wait for the servers to answer before calling
/// one still starting. An npx server's first run downloads it first.
const HEALTH_WAIT: Duration = Duration::from_secs(30);

const HELP: &str = "\
Manage the MCP servers in mcp.json

  letronna mcp add [options] <name> <command-or-url> [args...]
  letronna mcp list              every server, started to see it answer
  letronna mcp get <name>        one server in full
  letronna mcp remove <name>     take one out
  letronna mcp login <name>      sign in to one reached by URL, in the browser
  letronna mcp logout <name>     forget that sign-in
  letronna mcp serve [options]   offer the browser, media and search tools
                                 to another agent over stdio

Options for serve
      --yes                run tools that need a yes without one
      --allow-turn         also offer letronna_turn, a whole agent turn

Options for add
  -t, --transport <kind>   stdio (a command run here) or http; a URL means http
  -e, --env <KEY=value>    an environment variable for the command
  -H, --header <\"K: v\">    a header sent to a server reached by URL
  -s, --scope user         the only scope: one list for every folder
  --client-id <id>         the app id a server that registers no apps gave you
  --callback-port <port>   the port that app id was registered with

Examples
  letronna mcp add files -- npx -y @modelcontextprotocol/server-filesystem ~/notes
  letronna mcp add github -e GITHUB_TOKEN=... -- npx -y @modelcontextprotocol/server-github
  letronna mcp add --transport http sentry https://mcp.sentry.dev/mcp
";

/// Run `letronna mcp <words>` and give the exit code.
pub fn run(words: &[String]) -> i32 {
    let (verb, rest) = match words.split_first() {
        Some((verb, rest)) => (verb.as_str(), rest),
        None => ("", words),
    };
    let wants_help = rest
        .iter()
        .take_while(|word| *word != "--")
        .any(|word| word == "-h" || word == "--help");
    if wants_help || matches!(verb, "" | "help" | "-h" | "--help") {
        print!("{HELP}");
        return 0;
    }
    // Its stdout is the protocol's, so its one failure goes to stderr.
    if verb == "serve" {
        return match serve(rest) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("{error}");
                1
            }
        };
    }
    let done = match verb {
        "add" => add(rest),
        "list" => list(),
        "get" => named(rest).and_then(get),
        "remove" => named(rest).and_then(remove),
        "login" => named(rest).and_then(|name| {
            login(name, &mut |question| {
                crate::input::secret(&format!("{question}: "))
            })?;
            render::line(&format!("Signed in to {}.", render::bold(name)));
            Ok(())
        }),
        "logout" => named(rest).and_then(logout),
        other => Err(format!(
            "`letronna mcp {other}` is not a command; `letronna mcp` lists them."
        )),
    };
    match done {
        Ok(()) => 0,
        Err(error) => {
            render::line(&render::red(&error));
            1
        }
    }
}

/// A server's state in a few words, coloured: how `list`, `get` and `/mcp`
/// show it.
pub fn health(server: &ServerStatus) -> String {
    match server.state {
        State::Connected => format!(
            "{} {}",
            render::green("✓ connected"),
            render::dim(&format!(
                "· {} tools{}",
                server.tools,
                if server.signed_in {
                    " · signed in"
                } else {
                    ""
                }
            ))
        ),
        // Why, as a sign-in the model started and that failed says there.
        State::NeedsAuth => {
            let why = server.error.as_deref().unwrap_or_default();
            let why = why.lines().next().unwrap_or(why);
            format!("{} {}", render::amber("needs sign-in"), render::dim(why))
        }
        State::Starting => render::dim("… still starting"),
        State::Retrying => {
            let why = server.error.as_deref().unwrap_or("stopped");
            let next = server.retry.map_or_else(String::new, |retry| {
                format!(
                    " in {} s ({} of {})",
                    retry.wait.as_secs_f64().ceil(),
                    retry.attempt,
                    retry.attempts
                )
            });
            let why = why.lines().next().unwrap_or(why);
            render::amber(&format!("↻ retrying{next}: {why}"))
        }
        State::Disabled => render::dim("switched off"),
        State::Failed => {
            let why = server.error.as_deref().unwrap_or("failed");
            render::red(&format!("✗ {}", why.lines().next().unwrap_or(why)))
        }
    }
}

fn add(words: &[String]) -> Result<(), String> {
    let (name, entry) = entry(words)?;
    let kind = if entry.get("url").is_some() {
        "HTTP"
    } else {
        "stdio"
    };
    mcp::add_server(&name, entry)?;
    render::line(&format!(
        "Added the {kind} MCP server {} to {}",
        render::bold(&name),
        mcp::config_path().display()
    ));
    render::line(&render::dim(
        "New sessions start it; one already open does with /mcp reload.",
    ));
    Ok(())
}

fn remove(name: &str) -> Result<(), String> {
    mcp::remove_server(name)?;
    render::line(&format!(
        "Removed the MCP server {} from {}",
        render::bold(name),
        mcp::config_path().display()
    ));
    render::line(&render::dim(
        "A session already open stops it with /mcp reload.",
    ));
    Ok(())
}

/// Sign in to the server `name` in the browser. `ask` puts a question to
/// the user and reads a line: Enter once the browser says so, or the address
/// it ended on, pasted where the browser cannot reach this computer.
pub fn login(name: &str, ask: &mut dyn FnMut(&str) -> Option<String>) -> Result<(), String> {
    use letronna_core::mcp_auth;
    render::line(&render::dim(&format!("Finding where {name} signs in…")));
    let sign_in = mcp_auth::begin(name, &|| false)?;
    render::said(
        &render::blue("●"),
        &render::bold(&format!("Sign in to {name}")),
    );
    render::line(&sign_in.url);
    if !mcp_auth::open_browser(&sign_in.url) {
        render::line(&render::dim(
            "Could not open a browser; open the address above.",
        ));
    }
    let (done, finished) = std::sync::mpsc::channel();
    let waiting = sign_in.clone();
    std::thread::spawn(move || done.send(waiting.finish()));
    let question =
        "Press Enter once the browser says you are signed in, or paste the address it ended on";
    loop {
        if let Ok(outcome) = finished.try_recv() {
            return outcome;
        }
        let Some(line) = ask(question) else {
            sign_in.cancel();
            return Err(mcp_auth::CANCELLED.into());
        };
        if !line.trim().is_empty()
            && let Err(error) = sign_in.paste(&line)
        {
            render::line(&render::red(&error));
            continue;
        }
        if sign_in.answered() {
            return finished
                .recv()
                .unwrap_or_else(|_| Err("The sign-in stopped.".into()));
        }
        render::line(&render::dim(
            "Not signed in yet: finish in the browser, or paste the address it ended on.",
        ));
    }
}

/// Forget the sign-in to the server `name`.
pub fn logout(name: &str) -> Result<(), String> {
    letronna_core::mcp_auth::sign_out(name)?;
    render::line(&format!("Signed out of {}.", render::bold(name)));
    Ok(())
}

fn list() -> Result<(), String> {
    let servers = checked()?;
    if servers.is_empty() {
        render::line("No MCP servers yet. Add one with `letronna mcp add`.");
    }
    for server in &servers {
        let target = match server.transport {
            "http" => format!("{} (HTTP)", server.target),
            _ => server.target.clone(),
        };
        render::line(&format!(
            "{}: {target} - {}",
            render::bold(&server.name),
            health(server)
        ));
    }
    Ok(())
}

fn get(name: &str) -> Result<(), String> {
    let Some(server) = checked()?.into_iter().find(|server| server.name == name) else {
        return Err(format!("There is no {name} in mcp.json."));
    };
    let row = |what: &str, text: &str| {
        render::line(&format!("  {} {text}", render::dim(&format!("{what:<12}"))))
    };
    render::line(&render::bold(&format!("{name}:")));
    row("Status", &health(&server));
    row("Type", server.transport);
    match server.transport {
        "http" => row("URL", &server.target),
        _ => row("Command", &server.target),
    }
    // Only the names: the values are keys as often as not, and this can be
    // run where others see the screen, or by the model through a command.
    if !server.env.is_empty() {
        row("Environment", &server.env.join(", "));
    }
    if !server.headers.is_empty() {
        row("Headers", &server.headers.join(", "));
    }
    render::blank();
    render::line(&render::dim(&format!(
        "To remove it: letronna mcp remove {name}"
    )));
    Ok(())
}

/// Serve Letronna's tools over stdin and stdout until the client hangs up.
fn serve(words: &[String]) -> Result<(), String> {
    let (mut yes, mut allow_turn) = (false, false);
    for word in words {
        match word.as_str() {
            "--yes" => yes = true,
            "--allow-turn" => allow_turn = true,
            other => {
                return Err(format!(
                    "`letronna mcp serve` takes --yes and --allow-turn, not {other}."
                ));
            }
        }
    }
    letronna_core::diag::init("cli");
    let settings = letronna_core::store::load_listing().settings;
    letronna_core::browser::share_browser(settings.browser_shared);
    std::thread::spawn(letronna_core::browser::close_left_over);
    let turn = allow_turn.then(|| {
        // Served once, before any turn: a request with nobody serving is
        // declined at once, and there is no keyboard behind this stdin.
        answer_from_gate(yes);
        let turn: mcp_serve::Turn = Box::new(move |prompt| turn(prompt, yes));
        turn
    });
    let mut server = mcp_serve::Server::new(mcp_serve::exposed(), yes, turn);
    let served = mcp_serve::serve(&mut server, std::io::stdin().lock(), std::io::stdout());
    // A hidden browser, a job or an MCP server of our own would outlive the
    // client unseen.
    letronna_core::browser::close_hidden();
    letronna_core::jobs::stop_all();
    mcp::stop_all();
    letronna_core::diag::stop();
    served
}

/// Answers the calls a turn asks a yes for as the gate settles a helper's
/// with no terminal to ask on, saying why one was refused on stderr, since
/// stdout is the protocol's.
fn answer_from_gate(yes: bool) {
    let requests = approvals::serve();
    std::thread::spawn(move || {
        for request in requests {
            // Read afresh each time, since the window may have added one.
            let rules = permissions::load(&permissions::user_file());
            let decision = decided(&request.call, request.root, yes, &rules);
            if let Decision::Deny(why) = &decision {
                eprintln!("`{}`: {why}", request.call.name);
            }
            request.answer(decision);
        }
    });
}

/// What the gate makes of `call` in session `root` with nobody to ask, as
/// the terminal settles a helper's: in Bypass under --yes, so a yes to every
/// card still leaves the floor and the user's deny and ask rules to refuse.
fn decided(call: &ToolCall, root: u64, yes: bool, rules: &Result<Vec<Rule>, String>) -> Decision {
    let mut ctx = ToolContext::default();
    ctx.conversation = root;
    ctx.workspace = std::env::current_dir().ok();
    let mode = if yes { Mode::Bypass } else { Mode::Default };
    let none = Grants::new();
    match gate::decide(call, &Gate::for_terminal(&ctx, &none, rules, mode, false)) {
        gate::Outcome::Run => Decision::Once,
        gate::Outcome::Refuse(why) => Decision::Deny(why),
        // With nobody to answer the gate refuses rather than asks; a card
        // here would be a yes nobody gave.
        gate::Outcome::Ask => Decision::Deny(String::new()),
    }
}

/// Runs `prompt` as `letronna -p` would, in a session of its own that is
/// listed with the rest, and gives the last thing the agent said.
fn turn(prompt: &str, yes: bool) -> Result<String, String> {
    let mut runner = Runner {
        store: letronna_core::store::load_listing(),
        allowed: Default::default(),
        mode: if yes { Mode::Bypass } else { Mode::Default },
        human: false,
        trouble: None,
        bar: Bar::new(String::new()),
        stopped: false,
        images: Vec::new(),
        notices: letronna_core::notify::Notices::new(Box::new(crate::notify::Silent)),
        ended: None,
        voice: None,
        review: Default::default(),
    };
    let session = runner.start(std::env::current_dir().ok());
    let connection = runner.connection(session);
    chat::validate(&connection)?;
    let mut conversation = runner.store.conversations.remove(session);
    conversation.title = prompt.chars().take(40).collect();
    conversation.messages.push(Message {
        role: "user".into(),
        content: prompt.to_owned(),
        ..Default::default()
    });
    let id = conversation.id;
    // The run is the only writer of the session's file, which lists it.
    let report = driver::run(Spec {
        connection,
        transcript: Transcript {
            path: letronna_core::store::sessions_dir().join(format!("{id}.jsonl")),
            conversation,
        },
        id,
        owner: id,
        root: id,
        run: id,
        label: "Letronna".into(),
        allow: Allow::All,
        rounds: letronna_core::agent::MAX_ROUNDS,
        cancel: Default::default(),
        budget: None,
        status: Default::default(),
        depth: 0,
        fork: false,
        from: 0,
        schema: None,
        decide: None,
        watch: None,
    });
    for problem in letronna_core::hooks::problems() {
        eprintln!("{problem}");
    }
    match report.status {
        Outcome::Failed(why) => Err(why),
        _ => Ok(report.text),
    }
}

/// Every server, once each has answered or had `HEALTH_WAIT` to.
fn checked() -> Result<Vec<ServerStatus>, String> {
    render::line(&render::dim("Starting the MCP servers to see them answer…"));
    mcp::ready(HEALTH_WAIT);
    match mcp::unreadable() {
        Some(error) => Err(error),
        None => Ok(mcp::status()),
    }
}

/// The one name `get` and `remove` take, past a `--scope` flag.
fn named(words: &[String]) -> Result<&str, String> {
    let mut words = words.iter();
    let mut name = None;
    while let Some(word) = words.next() {
        match word.as_str() {
            "-s" | "--scope" => scope(words.next())?,
            flag if flag.starts_with('-') => return Err(format!("{flag} is not an option here.")),
            _ if name.is_some() => return Err("Give one server name.".into()),
            _ => name = Some(word.as_str()),
        }
    }
    name.ok_or_else(|| "Which server? Give its name.".into())
}

fn scope(value: Option<&String>) -> Result<(), String> {
    match value.map(String::as_str) {
        Some("user") => Ok(()),
        Some("local" | "project") => Err(
            "Letronna keeps one list of MCP servers for every folder; use --scope user or \
             leave it out."
                .into(),
        ),
        _ => Err("--scope takes user.".into()),
    }
}

/// The name and `mcp.json` entry that `add`'s words describe.
fn entry(words: &[String]) -> Result<(String, Value), String> {
    let usage = "Give a name and a command or URL, like `letronna mcp add files -- npx -y \
                 @modelcontextprotocol/server-filesystem ~/notes`.";
    let mut words = words.iter().peekable();
    let (mut transport, mut env, mut headers) = (None, Map::new(), Map::new());
    let mut oauth = Map::new();
    let mut plain: Vec<String> = Vec::new();
    while let Some(word) = words.next() {
        match word.as_str() {
            // The rest is the command and its arguments, dashes and all.
            "--" => plain.extend(words.by_ref().cloned()),
            "-t" | "--transport" => {
                let kind = words.next().ok_or("--transport takes stdio or http.")?;
                transport = Some(kind.as_str());
            }
            // As in Claude Code, one flag takes every value that follows it.
            "-e" | "--env" => {
                let before = env.len();
                while let Some(pair) = words.next_if(|word| is_env(word)) {
                    if let Some((key, value)) = pair.split_once('=') {
                        env.insert(key.to_owned(), json!(value));
                    }
                }
                if env.len() == before {
                    return Err("-e takes KEY=value, like -e API_KEY=abc.".into());
                }
            }
            "-H" | "--header" => {
                let before = headers.len();
                while let Some(header) = words.next_if(|word| is_header(word)) {
                    if let Some((key, value)) = header.split_once(':') {
                        headers.insert(key.trim().to_owned(), json!(value.trim()));
                    }
                }
                if headers.len() == before {
                    return Err("-H takes a header, like -H \"X-Api-Key: abc\".".into());
                }
            }
            "-s" | "--scope" => scope(words.next())?,
            "--client-id" => {
                let id = words
                    .next()
                    .ok_or("--client-id takes the id the server gave you.")?;
                oauth.insert("clientId".into(), json!(id));
            }
            "--callback-port" => {
                let port = words.next().and_then(|port| port.parse::<u16>().ok());
                let port = port.ok_or("--callback-port takes a port number, like 8080.")?;
                oauth.insert("callbackPort".into(), json!(port));
            }
            "--client-secret" => {
                return Err(
                    "--client-secret: a secret here stays in the shell's history. Put it in \
                     mcp.json under oauth as \"clientSecret\": \"${VAR}\" instead."
                        .into(),
                );
            }
            // Past the name and the command, a dash is the command's own.
            flag if flag.starts_with('-') && plain.len() < 2 => {
                return Err(format!("{flag} is not an option of `letronna mcp add`."));
            }
            _ => plain.push(word.clone()),
        }
    }
    let mut plain = plain.into_iter();
    let (Some(name), Some(first)) = (plain.next(), plain.next()) else {
        return Err(usage.into());
    };
    let rest: Vec<String> = plain.collect();
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "{name}: a name takes only letters, digits, - and _."
        ));
    }
    let by_url = first.starts_with("http://") || first.starts_with("https://");
    let entry = match transport.unwrap_or(if by_url { "http" } else { "stdio" }) {
        "stdio" if !headers.is_empty() => {
            return Err("--header is for a server reached by URL; a command takes -e.".into());
        }
        "stdio" if !oauth.is_empty() => {
            return Err("Only a server reached by URL signs in.".into());
        }
        "stdio" => {
            let mut entry = json!({"command": first, "args": rest});
            if !env.is_empty() {
                entry["env"] = Value::Object(env);
            }
            entry
        }
        "http" if !by_url => {
            return Err(format!("{first} is not an address; give the https:// one."));
        }
        "http" if !rest.is_empty() || !env.is_empty() => {
            return Err(
                "A server reached by URL takes no arguments or -e; give it --header.".into(),
            );
        }
        "http" => {
            let mut entry = json!({"type": "http", "url": first});
            if !headers.is_empty() {
                entry["headers"] = Value::Object(headers);
            }
            if !oauth.is_empty() {
                entry["oauth"] = Value::Object(oauth);
            }
            entry
        }
        "sse" => {
            return Err(
                "Letronna speaks MCP over HTTP, not the older SSE. Most servers offer both: \
                 try --transport http with the address ending in /mcp."
                    .into(),
            );
        }
        other => return Err(format!("--transport takes stdio or http, not {other}.")),
    };
    Ok((name, entry))
}

/// Whether a word is `KEY=value`.
fn is_env(word: &str) -> bool {
    word.split_once('=').is_some_and(|(key, _)| {
        !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Whether a word is `Name: value`, and not an address such as https://.
fn is_header(word: &str) -> bool {
    word.split_once(':').is_some_and(|(key, value)| {
        !key.is_empty()
            && !value.starts_with("//")
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use letronna_core::permissions::Effect;

    fn read(words: &[&str]) -> Result<(String, Value), String> {
        let words: Vec<String> = words.iter().map(|w| w.to_string()).collect();
        entry(&words)
    }

    /// A turn's `run_command` call of `line`.
    fn command(line: &str) -> ToolCall {
        ToolCall {
            name: "run_command".into(),
            arguments: json!({"command": line}).to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn a_turns_calls_under_yes_still_meet_the_floor_and_the_deny_rules() {
        let none = Ok(Vec::new());
        assert_eq!(decided(&command("ls"), 1, true, &none), Decision::Once);
        let Decision::Deny(why) = decided(&command("rm -rf victim"), 1, true, &none) else {
            panic!("a folder was deleted with nobody to ask");
        };
        assert!(why.contains("nobody"), "{why}");
        let denied = Ok(vec![Rule {
            tool: "run_command".into(),
            pattern: "ls".into(),
            effect: Effect::Deny,
            project: None,
        }]);
        let Decision::Deny(why) = decided(&command("ls"), 1, true, &denied) else {
            panic!("a denied command ran");
        };
        assert!(why.contains("permission rule denies"), "{why}");
    }

    #[test]
    fn a_turns_calls_without_yes_are_refused_unless_they_only_read() {
        let none = Ok(Vec::new());
        assert!(matches!(
            decided(&command("touch made"), 1, false, &none),
            Decision::Deny(_)
        ));
        assert_eq!(decided(&command("ls"), 1, false, &none), Decision::Once);
    }

    #[test]
    fn the_forms_claude_codes_docs_give_are_read_the_same() {
        let (name, files) = read(&["files", "--", "npx", "-y", "pkg", "-e"]).unwrap();
        assert_eq!(name, "files");
        assert_eq!(
            files,
            json!({"command": "npx", "args": ["-y", "pkg", "-e"]})
        );
        // Without `--`, a dash after the command is still the command's.
        let (_, bare) = read(&["files", "npx", "-y", "pkg"]).unwrap();
        assert_eq!(bare, json!({"command": "npx", "args": ["-y", "pkg"]}));
        let (_, keyed) = read(&["gh", "-e", "A=1", "B=x=y", "--", "npx", "gh"]).unwrap();
        assert_eq!(keyed["env"], json!({"A": "1", "B": "x=y"}));
        let (_, remote) = read(&[
            "--transport",
            "http",
            "sentry",
            "https://mcp.example.com/mcp",
            "--header",
            "Authorization: Bearer t",
            "X-Key:k",
        ])
        .unwrap();
        assert_eq!(
            remote,
            json!({"type": "http", "url": "https://mcp.example.com/mcp",
                   "headers": {"Authorization": "Bearer t", "X-Key": "k"}})
        );
        let (_, app) = read(&[
            "--client-id",
            "abc",
            "--callback-port",
            "8080",
            "linear",
            "https://mcp.example.com/mcp",
        ])
        .unwrap();
        assert_eq!(
            app["oauth"],
            json!({"clientId": "abc", "callbackPort": 8080})
        );
        // An address alone is taken as one to reach, and --scope user is fine.
        let (_, inferred) = read(&["-s", "user", "s", "https://mcp.example.com/mcp"]).unwrap();
        assert_eq!(inferred["type"], "http");
    }

    #[test]
    fn what_letronna_cannot_honour_is_refused_with_why() {
        let refused = |words: &[&str], says: &str| {
            let error = read(words).unwrap_err();
            assert!(error.contains(says), "{words:?}: {error}");
        };
        refused(&["-t", "sse", "s", "https://x/sse"], "SSE");
        refused(&["-s", "project", "s", "--", "npx"], "one list");
        refused(
            &["--client-secret", "abc", "s", "https://x/mcp"],
            "clientSecret",
        );
        refused(
            &["--client-id", "abc", "s", "--", "npx"],
            "Only a server reached by URL",
        );
        refused(
            &["--callback-port", "port", "s", "https://x/mcp"],
            "port number",
        );
        refused(&["only-a-name"], "Give a name");
        refused(&["my server", "npx"], "letters, digits");
        refused(&["s", "-H", "A: b", "--", "npx"], "--header is for");
        refused(&["s", "-e", "--", "npx"], "KEY=value");
        refused(&["-t", "http", "s", "npx"], "not an address");
        refused(&["-t", "ws", "s", "npx"], "stdio or http");
        refused(&["-x", "s", "npx"], "not an option");
    }
}
