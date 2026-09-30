//! Adding an MCP server from what its README says.
//!
//! A server's README gives one of three things to copy: a JSON block for
//! Claude Desktop or VS Code, a `claude mcp add …` line, or the bare line
//! that starts it, such as `npx -y @modelcontextprotocol/server-github`.
//! `read` takes any of them, pasted as it is, fences and `$` prompts
//! included, and fills in the fields of the window's add form: a name, the
//! command line or address, and the variables or headers it is given. The
//! name is only a guess from the package, for the user to correct before
//! `add` writes the entry to `mcp.json` the same way `letronna mcp add` does.

use serde_json::{Map, Value, json};

/// Words in a command line that say how to run a package rather than which
/// package, passed over when guessing a server's name.
const RUNNERS: &[&str] = &["run", "x", "exec", "dlx", "tool", "-m"];

/// Parts of a package's name that every server has, taken off its name.
const PREFIXES: &[&str] = &["mcp-server-", "server-", "mcp-"];
const SUFFIXES: &[&str] = &["-mcp-server", "-server-mcp", "-mcp", "-server"];

/// A script's or folder's name that says nothing of the server.
const GENERIC: &[&str] = &[
    "index", "main", "server", "dist", "build", "src", "lib", "bin",
];

/// A server as the add form holds it before it is added.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    pub name: String,
    /// The command and its arguments as one line, or the server's address.
    pub target: String,
    /// One a line: `KEY=value` for a command's environment, `Name: value`
    /// for a header sent to an address.
    pub extras: String,
}

/// The form's fields filled in from a README snippet or command line.
pub fn read(pasted: &str) -> Result<Draft, String> {
    let text: String = pasted
        .lines()
        .filter(|line| !line.trim_start().starts_with("```"))
        .map(|line| line.trim().strip_prefix("$ ").unwrap_or(line.trim()))
        .collect::<Vec<_>>()
        .join("\n");
    let text = text.trim();
    if text.is_empty() {
        return Err("Paste a server's JSON, or the line that starts it.".into());
    }
    if text.starts_with('{') || text.starts_with('"') {
        // A README often shows only the entry, `"github": {…}`, for the
        // reader to put inside their own braces.
        let json = serde_json::from_str(text)
            .or_else(|_| serde_json::from_str(&format!("{{{text}}}")))
            .map_err(|e| format!("That JSON will not read: {e}"))?;
        return from_json(&json);
    }
    // Lines ended by a backslash go on, and of several commands the last is
    // the one that starts the server, after any that install it.
    let joined = text.replace("\\\n", " ");
    let line = joined
        .lines()
        .rfind(|line| !line.trim().is_empty())
        .unwrap_or_default();
    from_line(&split(line)?)
}

/// The draft a JSON snippet describes: a whole config, one entry, or one
/// server's fields.
fn from_json(json: &Value) -> Result<Draft, String> {
    let servers = ["mcpServers", "servers"]
        .iter()
        .find_map(|key| json[key].as_object())
        .or_else(|| json["mcp"]["servers"].as_object());
    let (name, server) = match servers {
        Some(servers) => first(servers)?,
        None if is_server(json) => (String::new(), json),
        None => match json.as_object() {
            Some(entries) => first(entries)?,
            None => return Err("That JSON names no server.".into()),
        },
    };
    if server["type"].as_str() == Some("sse") {
        return Err(sse());
    }
    let url = server["url"].as_str().or(server["serverUrl"].as_str());
    let (words, extras, pairs) = match url {
        Some(url) => (vec![url.to_owned()], &server["headers"], ": "),
        None => {
            let Some(command) = server["command"].as_str() else {
                return Err(format!("{name} has neither a command nor a url."));
            };
            let args = server["args"].as_array().into_iter().flatten();
            let words = std::iter::once(command.to_owned())
                .chain(args.map(|arg| arg.as_str().map_or(arg.to_string(), str::to_owned)))
                .collect();
            (words, &server["env"], "=")
        }
    };
    let extras = extras
        .as_object()
        .into_iter()
        .flatten()
        .map(|(key, value)| {
            let value = value.as_str().map_or(value.to_string(), str::to_owned);
            format!("{key}{pairs}{value}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Draft {
        name: if name.is_empty() {
            name_for(&words)
        } else {
            name
        },
        target: join(&words),
        extras,
    })
}

/// The first server in `servers`, by name, or why there is none.
fn first(servers: &Map<String, Value>) -> Result<(String, &Value), String> {
    servers
        .iter()
        .find(|(_, server)| is_server(server))
        .map(|(name, server)| (name.clone(), server))
        .ok_or_else(|| "That JSON names no server.".into())
}

/// Whether `value` is one server's fields.
fn is_server(value: &Value) -> bool {
    ["command", "url", "serverUrl"]
        .iter()
        .any(|key| value[key].is_string())
}

/// The draft a command line describes: a `claude mcp add` line, or the
/// command itself after any `KEY=value` it is given.
fn from_line(words: &[String]) -> Result<Draft, String> {
    let adds = words.len() > 3
        && matches!(words[0].as_str(), "claude" | "letronna")
        && words[1] == "mcp"
        && words[2] == "add";
    if adds {
        return added(&words[3..]);
    }
    let set = words.iter().take_while(|word| is_env(word)).count();
    drafted(None, &words[set..], &words[..set])
}

/// The draft of `claude mcp add`'s words after `add`, read as `letronna mcp
/// add` reads them.
fn added(words: &[String]) -> Result<Draft, String> {
    let mut words = words.iter().peekable();
    let (mut plain, mut extras) = (Vec::new(), Vec::new());
    while let Some(word) = words.next() {
        match word.as_str() {
            "--" => plain.extend(words.by_ref().cloned()),
            "-t" | "--transport" if words.peek().is_some_and(|kind| *kind == "sse") => {
                return Err(sse());
            }
            "-t" | "--transport" | "-s" | "--scope" => {
                words.next();
            }
            "-e" | "--env" => {
                while let Some(pair) = words.next_if(|word| is_env(word)) {
                    extras.push(pair.clone());
                }
            }
            "-H" | "--header" => {
                while let Some(header) = words.next_if(|word| is_header(word)) {
                    extras.push(header.clone());
                }
            }
            _ => plain.push(word.clone()),
        }
    }
    let name = (!plain.is_empty()).then(|| plain.remove(0));
    drafted(name, &plain, &extras)
}

/// The draft of the command `words` given `extras`, named `name` or after
/// its package.
fn drafted(name: Option<String>, words: &[String], extras: &[String]) -> Result<Draft, String> {
    if words.is_empty() {
        return Err("That line has no command in it.".into());
    }
    Ok(Draft {
        name: name.unwrap_or_else(|| name_for(words)),
        target: join(words),
        extras: extras.join("\n"),
    })
}

/// The refusal for a server that speaks only the older SSE.
fn sse() -> String {
    "Letronna speaks MCP over HTTP, not the older SSE. Most servers offer both: \
     give the address ending in /mcp."
        .into()
}

/// The name, and the `mcp.json` entry, that `draft` describes.
pub fn entry(draft: &Draft) -> Result<(String, Value), String> {
    let name = draft.name.trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("A name takes only letters, digits, - and _.".into());
    }
    let words = split(draft.target.trim())?;
    let Some((first, rest)) = words.split_first() else {
        return Err("Give the command that starts the server, or its address.".into());
    };
    let extras = draft
        .extras
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty());
    let by_url = first.starts_with("http://") || first.starts_with("https://");
    let mut entry = if by_url {
        if !rest.is_empty() {
            return Err("An address takes nothing after it; give headers below.".into());
        }
        json!({"type": "http", "url": first})
    } else {
        json!({"command": first, "args": rest})
    };
    let mut given = Map::new();
    for line in extras {
        let pair = match by_url {
            true => line.split_once(':').filter(|_| is_header(line)),
            false => line.split_once('=').filter(|_| is_env(line)),
        };
        let Some((key, value)) = pair else {
            return Err(match by_url {
                true => format!("`{line}` is not a header, like `Authorization: Bearer …`."),
                false => format!("`{line}` is not a variable, like `API_KEY=…`."),
            });
        };
        given.insert(key.trim().to_owned(), json!(value.trim()));
    }
    if !given.is_empty() {
        entry[if by_url { "headers" } else { "env" }] = Value::Object(given);
    }
    Ok((name.to_owned(), entry))
}

/// Add the server `draft` describes to `mcp.json` and start it, giving its
/// name.
pub fn add(draft: &Draft) -> Result<String, String> {
    let (name, entry) = entry(draft)?;
    crate::mcp::add_server(&name, entry)?;
    crate::mcp::reload()?;
    Ok(name)
}

/// A guess at a server's name from the command that starts it or its
/// address: `github` for `npx -y @modelcontextprotocol/server-github`.
fn name_for(words: &[String]) -> String {
    let Some(first) = words.first() else {
        return "server".into();
    };
    if let Some((_, rest)) = first.split_once("://") {
        let host = rest.split(['/', ':']).next().unwrap_or_default();
        let labels: Vec<&str> = host.split('.').collect();
        let named = labels[..labels.len().saturating_sub(1)]
            .iter()
            .find(|label| !matches!(**label, "mcp" | "www" | "api" | "app"));
        return clean(named.unwrap_or(&labels[0]));
    }
    let named: Vec<&String> = words[1..]
        .iter()
        .filter(|word| !word.starts_with('-') && !RUNNERS.contains(&word.as_str()))
        .collect();
    // The package, which a scope, a registry or `mcp` in its name gives away,
    // before any folder or value handed to it.
    let package = named
        .iter()
        .find(|word| word.contains(['/', '@']) || word.contains("mcp"))
        .or(named.first())
        .map_or(first.as_str(), |word| word.as_str());
    let mut parts = package.rsplit(['/', '\\']);
    let name = parts.next().unwrap_or(package);
    let mut name = name.split('.').next().unwrap_or(name);
    // A script such as build/index.js is named by its folders.
    while GENERIC.contains(&name.to_ascii_lowercase().as_str()) {
        let Some(folder) = parts.next().filter(|folder| !folder.is_empty()) else {
            break;
        };
        name = folder;
    }
    if name.is_empty() {
        name = "server";
    }
    clean(name)
}

/// `name` as a server's name: without its version, tag or the `mcp` and
/// `server` every package says, in letters, digits and dashes.
fn clean(name: &str) -> String {
    let name = match name.rsplit_once('@') {
        Some((head, _)) if !head.is_empty() => head,
        _ => name,
    };
    let name = name.split(':').next().unwrap_or(name);
    let mut name: String = name
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    if let Some(rest) = PREFIXES.iter().find_map(|p| name.strip_prefix(p)) {
        name = rest.to_owned();
    }
    if let Some(rest) = SUFFIXES.iter().find_map(|s| name.strip_suffix(s)) {
        name = rest.to_owned();
    }
    let name = name.trim_matches('-');
    match name {
        "" => "server".into(),
        name => name.to_owned(),
    }
}

/// Whether `word` is `KEY=value`.
fn is_env(word: &str) -> bool {
    word.split_once('=').is_some_and(|(key, _)| {
        !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Whether `word` is `Name: value`, and not an address such as https://.
fn is_header(word: &str) -> bool {
    word.split_once(':').is_some_and(|(key, value)| {
        !key.is_empty()
            && !value.starts_with("//")
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    })
}

/// A command line's words, split as a shell would split them: quotes keep
/// spaces, and a backslash escapes only a quote, a space or itself, so a
/// Windows path keeps its backslashes.
fn split(line: &str) -> Result<Vec<String>, String> {
    let (mut words, mut word, mut quote) = (Vec::new(), None::<String>, None::<char>);
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        let escaped = c == '\\'
            && quote != Some('\'')
            && chars
                .peek()
                .is_some_and(|next| matches!(next, '"' | '\'' | '\\' | ' '));
        match (c, quote) {
            _ if escaped => {
                let next = chars.next().unwrap_or(c);
                word.get_or_insert_default().push(next);
            }
            ('"' | '\'', None) => {
                quote = Some(c);
                word.get_or_insert_default();
            }
            (c, Some(open)) if c == open => quote = None,
            (c, None) if c.is_whitespace() => words.extend(word.take()),
            (c, _) => word.get_or_insert_default().push(c),
        }
    }
    if quote.is_some() {
        return Err("A quote in that line is never closed.".into());
    }
    words.extend(word);
    Ok(words)
}

/// `words` as one line that `split` gives back.
fn join(words: &[String]) -> String {
    words
        .iter()
        .map(|word| {
            // Two backslashes in a row are one to `split`, so a UNC path
            // keeps them only quoted.
            let plain = !word.is_empty()
                && !word.contains("\\\\")
                && !word.contains(|c: char| c.is_whitespace() || matches!(c, '"' | '\''));
            match plain {
                true => word.clone(),
                false => format!("\"{}\"", word.replace('\\', "\\\\").replace('"', "\\\"")),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(name: &str, target: &str, extras: &str) -> Draft {
        Draft {
            name: name.into(),
            target: target.into(),
            extras: extras.into(),
        }
    }

    #[test]
    fn a_claude_desktop_block_fills_in_its_first_server() {
        let pasted = r#"```json
            {
              "mcpServers": {
                "github": {
                  "command": "npx",
                  "args": ["-y", "@modelcontextprotocol/server-github"],
                  "env": {"GITHUB_PERSONAL_ACCESS_TOKEN": "<YOUR_TOKEN>"}
                },
                "other": {"command": "uvx", "args": ["other"]}
              }
            }
            ```"#;
        assert_eq!(
            read(pasted).unwrap(),
            draft(
                "github",
                "npx -y @modelcontextprotocol/server-github",
                "GITHUB_PERSONAL_ACCESS_TOKEN=<YOUR_TOKEN>"
            )
        );
    }

    #[test]
    fn an_entry_without_its_braces_or_a_vs_code_block_reads_the_same() {
        let entry = r#""fetch": {"command": "uvx", "args": ["mcp-server-fetch"]}"#;
        assert_eq!(
            read(entry).unwrap(),
            draft("fetch", "uvx mcp-server-fetch", "")
        );
        let vs_code = r#"{"servers": {"sentry": {"type": "http", "url": "https://mcp.sentry.dev/mcp",
            "headers": {"Authorization": "Bearer x"}}}}"#;
        assert_eq!(
            read(vs_code).unwrap(),
            draft(
                "sentry",
                "https://mcp.sentry.dev/mcp",
                "Authorization: Bearer x"
            )
        );
        let sse = r#"{"mcpServers": {"old": {"type": "sse", "url": "https://x.dev/sse"}}}"#;
        assert!(read(sse).unwrap_err().contains("SSE"));
    }

    #[test]
    fn a_command_line_is_named_after_its_package() {
        for (line, name) in [
            (
                "npx -y @modelcontextprotocol/server-filesystem ~/notes",
                "filesystem",
            ),
            ("$ npx -y @upstash/context7-mcp@latest", "context7"),
            ("uvx mcp-server-fetch", "fetch"),
            ("python -m mcp_server_time", "time"),
            (
                "docker run -i --rm -e GITHUB_TOKEN ghcr.io/github/github-mcp-server",
                "github",
            ),
            ("node /home/me/weather/build/index.js", "weather"),
            ("https://api.githubcopilot.com/mcp/", "githubcopilot"),
        ] {
            assert_eq!(read(line).unwrap().name, name, "{line}");
        }
    }

    #[test]
    fn variables_before_a_command_and_a_claude_mcp_add_line_go_to_the_extras() {
        let line = "API_KEY=abc npx -y \\\n  @acme/server-notes \"My Notes\"";
        assert_eq!(
            read(line).unwrap(),
            draft(
                "notes",
                "npx -y @acme/server-notes \"My Notes\"",
                "API_KEY=abc"
            )
        );
        let added = "claude mcp add github -e GITHUB_TOKEN=x -s user -- npx -y \
                     @modelcontextprotocol/server-github";
        assert_eq!(
            read(added).unwrap(),
            draft(
                "github",
                "npx -y @modelcontextprotocol/server-github",
                "GITHUB_TOKEN=x"
            )
        );
        let remote = "claude mcp add --transport http linear https://mcp.linear.app/mcp \
                      -H \"Authorization: Bearer k\"";
        assert_eq!(
            read(remote).unwrap(),
            draft(
                "linear",
                "https://mcp.linear.app/mcp",
                "Authorization: Bearer k"
            )
        );
    }

    #[test]
    fn a_draft_becomes_the_entry_letronna_mcp_add_would_write() {
        let (name, entry) = entry(&draft(
            "notes",
            "npx -y \"C:\\\\My Notes\\\\server.js\"",
            "API_KEY=abc\n\n",
        ))
        .unwrap();
        assert_eq!(name, "notes");
        assert_eq!(
            entry,
            json!({"command": "npx", "args": ["-y", "C:\\My Notes\\server.js"],
                   "env": {"API_KEY": "abc"}})
        );
        let (_, remote) = super::entry(&draft("s", "https://x.dev/mcp", "X-Key: 1")).unwrap();
        assert_eq!(
            remote,
            json!({"type": "http", "url": "https://x.dev/mcp", "headers": {"X-Key": "1"}})
        );
        for (name, target, extras) in [
            ("has space", "npx x", ""),
            ("s", "", ""),
            ("s", "npx \"x", ""),
            ("s", "npx x", "not a pair"),
            ("s", "https://x.dev/mcp", "KEY=1"),
            ("s", "https://x.dev/mcp extra", ""),
        ] {
            assert!(
                super::entry(&draft(name, target, extras)).is_err(),
                "{name} {target}"
            );
        }
    }

    #[test]
    fn a_split_line_joins_back_to_the_same_words() {
        let words: Vec<String> = ["npx", "a b", "C:\\x", "say \"hi\"", "", "\\\\host\\share"]
            .map(str::to_owned)
            .into();
        assert_eq!(split(&join(&words)).unwrap(), words);
        assert_eq!(split("a\\ b 'c d' C:\\x").unwrap(), ["a b", "c d", "C:\\x"]);
    }
}
