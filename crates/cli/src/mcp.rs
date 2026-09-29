//! `letronna mcp`: add, list, get and remove the MCP servers in `mcp.json`.
//!
//! The flags are Claude Code's own, so a server's README that says `claude
//! mcp add ...` works with `letronna` in its place. Letronna keeps one list
//! for every folder, so `user` is the only scope it takes, and it signs in to
//! no server yet, so the OAuth flags are refused rather than dropped.

use crate::render;
use letronna_core::mcp::{self, ServerStatus, State};
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

Options for add
  -t, --transport <kind>   stdio (a command run here) or http; a URL means http
  -e, --env <KEY=value>    an environment variable for the command
  -H, --header <\"K: v\">    a header sent to a server reached by URL
  -s, --scope user         the only scope: one list for every folder

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
    let done = match verb {
        "add" => add(rest),
        "list" => list(),
        "get" => named(rest).and_then(get),
        "remove" => named(rest).and_then(remove),
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
            render::dim(&format!("· {} tools", server.tools))
        ),
        State::Starting => render::dim("… still starting"),
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
    render::line(&render::dim(&format!(
        "New sessions start it; one already open does with /mcp reconnect {name}."
    )));
    Ok(())
}

fn remove(name: &str) -> Result<(), String> {
    mcp::remove_server(name)?;
    render::line(&format!(
        "Removed the MCP server {} from {}",
        render::bold(name),
        mcp::config_path().display()
    ));
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
            "--callback-port" | "--client-id" | "--client-secret" => {
                return Err(format!(
                    "{word}: Letronna does not sign in to MCP servers yet; give the server \
                     a key with --header instead."
                ));
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

    fn read(words: &[&str]) -> Result<(String, Value), String> {
        let words: Vec<String> = words.iter().map(|w| w.to_string()).collect();
        entry(&words)
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
        refused(&["--client-id", "abc", "s", "https://x/mcp"], "sign in");
        refused(&["only-a-name"], "Give a name");
        refused(&["my server", "npx"], "letters, digits");
        refused(&["s", "-H", "A: b", "--", "npx"], "--header is for");
        refused(&["s", "-e", "--", "npx"], "KEY=value");
        refused(&["-t", "http", "s", "npx"], "not an address");
        refused(&["-t", "ws", "s", "npx"], "stdio or http");
        refused(&["-x", "s", "npx"], "not an option");
    }
}
