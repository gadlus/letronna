//! Searching the web with a search service of the user's own.
//!
//! The gateway's search (the Web toggle) comes with only some gateways, and a
//! browser searching Google is heavy and runs into its bot checks. A search
//! service answers in a moment with a list the model can read: Brave, Tavily
//! or Exa with the user's key, or a SearXNG of their own at its address, no
//! key needed. The service set up last is the one used; its key stays in
//! keys.json beside the gateways', under the service's name, and which one is
//! used in search.json.
//!
//! Each service is reached at its official address unless the user gives
//! another -- a proxy, or a server that answers the same way -- and a SearXNG
//! only at the address it is given. Emptied, an address goes back to the
//! official one.
//!
//! While no service is chosen, searches go to DuckDuckGo's page for browsers
//! without JavaScript, which needs no key, so a fresh install can search from
//! its first turn. DuckDuckGo has no public search API, so `keyless_search`
//! in the settings, on unless the user switches it off, says whether it is
//! used. It is never a stand-in: a chosen service that is not set up gives
//! its own error, and switched off with no other service chosen, the tool
//! says how to switch it back on or add a key rather than searching. It is
//! asked once per search with Letronna's own user agent; when it limits
//! searches, the model is told so rather than given an empty list, and it is
//! left alone for a while.
//!
//! The tool is only offered while a service is usable, or DuckDuckGo is
//! switched off with nothing else chosen, and asks before each search: the
//! query leaves for somebody else's server, and a page read a moment ago may
//! be what is asking for it.

use crate::tools::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// How long a search service gets to answer.
const SEARCH_TIME: Duration = Duration::from_secs(20);
/// How many results a search returns unless asked for more, and the most.
const RESULTS: u64 = 5;
const MOST_RESULTS: u64 = 10;
/// A result's snippet is cut to this many characters: enough to tell what the
/// page is, and a list of ten stays short.
const SNIPPET: usize = 300;
/// How many results a service that cannot narrow by site is asked for while
/// the list is narrowed here, so a few are still left afterwards: Brave's
/// most.
const WIDER: u64 = 20;
/// How long a search's answer is given again for the very same search.
const MEMO_TIME: Duration = Duration::from_secs(10 * 60);
/// How long DuckDuckGo is not asked again once it limits a search: asking at
/// once only keeps the block in place, and a few minutes usually lift it.
const HOLD_OFF: Duration = Duration::from_secs(5 * 60);
/// What the model is told while DuckDuckGo limits searches. Not an empty
/// list, which it would take for a subject nobody has written about.
const LIMITED: &str = "DuckDuckGo is limiting searches from this connection for now. Read a page you know with web_fetch, or ask the user to add a search service in Settings › Search (/search services).";
/// What a search is answered while DuckDuckGo is switched off and no other
/// service is chosen: nothing is sent, and the user is told how to search.
const SWITCHED_OFF: &str = "Searching with DuckDuckGo, which needs no key, is switched off and no other search service is chosen, so nothing was searched. The user can switch it back on with /search keyless in the terminal or \"keyless_search\": true in Letronna's settings.json, or add a search service's key in Settings › Search (/search services).";

/// A search service Letronna can use.
pub struct Service {
    pub name: &'static str,
    pub blurb: &'static str,
    /// Where it answers unless the user says otherwise. Empty for SearXNG,
    /// which is the user's own and needs no key.
    pub official: &'static str,
    /// Needs neither a key nor an address of the user's: it is used while no
    /// other is chosen, unless the settings switch it off.
    pub keyless: bool,
}

impl Service {
    /// Reached at an address the user gives, with no key.
    pub fn self_hosted(&self) -> bool {
        self.official.is_empty()
    }
}

pub const SERVICES: &[Service] = &[
    Service {
        name: "Brave",
        blurb: "Brave's own index of the web. A free plan, then paid by the search.",
        official: "https://api.search.brave.com/res/v1/web/search",
        keyless: false,
    },
    Service {
        name: "Tavily",
        blurb: "Search made for agents, each result with the text that matters.",
        official: "https://api.tavily.com/search",
        keyless: false,
    },
    Service {
        name: "Exa",
        blurb: "Search by meaning, with the passages of each page that match.",
        official: "https://api.exa.ai/search",
        keyless: false,
    },
    Service {
        name: "SearXNG",
        blurb: "Your own metasearch server, at its address. No key, nothing paid.",
        official: "",
        keyless: false,
    },
    Service {
        name: "DuckDuckGo",
        blurb: "No key; unofficial, may be rate-limited; a key gives steadier results.",
        // Its page without JavaScript, the one meant for any client; with no
        // slash on the end, as `web_address` leaves an address.
        official: "https://html.duckduckgo.com/html",
        keyless: true,
    },
];

/// Which service is used, and the addresses the user gave: search.json.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Debug)]
pub struct Choice {
    #[serde(default)]
    pub service: String,
    /// By service: a SearXNG's address, or somewhere else to reach one of
    /// the others. A service with none here is reached at its official one.
    #[serde(default)]
    pub addresses: BTreeMap<String, String>,
}

impl Choice {
    /// Where a service is reached: the address given for it, or its official one.
    pub fn address(&self, service: &Service) -> String {
        let given = self
            .addresses
            .get(service.name)
            .map(|a| a.trim())
            .unwrap_or_default();
        match given.is_empty() {
            true => service.official.to_owned(),
            false => given.to_owned(),
        }
    }

    /// Give a service an address, or with an empty one or its official one,
    /// send it back to the official one.
    pub fn set_address(&mut self, service: &Service, address: &str) {
        let address = address.trim();
        if address.is_empty() || address == service.official {
            self.addresses.remove(service.name);
        } else {
            self.addresses
                .insert(service.name.to_owned(), address.to_owned());
        }
    }
}

/// The service the user picked and where to reach it, as saved.
pub fn choice() -> Choice {
    std::fs::read(choice_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Save which service is used; with none, DuckDuckGo is, unless the
/// settings switch it off.
pub fn choose(choice: &Choice) -> Result<(), String> {
    let _ = std::fs::create_dir_all(crate::chat::data_dir());
    let bytes = serde_json::to_vec_pretty(choice).map_err(|e| e.to_string())?;
    std::fs::write(choice_path(), bytes).map_err(|e| format!("Cannot save the search service: {e}"))
}

fn choice_path() -> std::path::PathBuf {
    crate::chat::data_dir().join("search.json")
}

/// Whether the search in use is the keyless one, which a service with a key
/// would make steadier.
pub fn keyless() -> bool {
    let now = standing();
    now.ready && now.keyless
}

/// Whether a search service is usable, so searches go out.
pub fn ready() -> bool {
    standing().ready
}

/// Whether the tool is offered: while a service is usable, and while
/// DuckDuckGo is switched off with no other chosen, so that a call can say
/// how to search.
pub fn offered() -> bool {
    let now = standing();
    now.ready || now.keyless
}

/// Why a search cannot go out though the tool is offered: DuckDuckGo is
/// switched off and no other service is chosen.
pub(crate) fn switched_off() -> Option<&'static str> {
    let now = standing();
    (now.keyless && !now.ready).then_some(SWITCHED_OFF)
}

/// Where searching stands, as the files say now.
#[derive(Clone, Copy)]
struct Standing {
    /// A service is usable.
    ready: bool,
    /// The service searches go to, or would but for the settings, is the
    /// keyless one.
    keyless: bool,
}

fn standing() -> Standing {
    // Asked on every tool lookup -- several for each call, and one each
    // frame the window's approval card is up -- so the files are only read
    // again once one has changed: the terminal and the window each save
    // them, and each has to see the other's change.
    static SEEN: Mutex<Option<([Stamp; 4], Standing)>> = Mutex::new(None);
    let (choice_file, keys, vault, settings) = (
        choice_path(),
        crate::store::keys_path(),
        crate::vault::index_path(),
        crate::store::settings_path(),
    );
    remembered(&SEEN, [&choice_file, &keys, &vault, &settings], || {
        let choice = choice();
        let keys = crate::store::load_keys();
        Standing {
            ready: usable(&choice, &keys, crate::store::keyless_search()).is_ok(),
            keyless: picked(&choice).is_some_and(|s| s.keyless),
        }
    })
}

/// When a file last changed and how long it was then, or `None` while it is
/// missing.
// The length catches a second save within the clock's tick.
type Stamp = Option<(SystemTime, u64)>;

fn stamp(path: &Path) -> Stamp {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// The answer kept in `seen` while the files are as they were when it was
/// read, or a fresh one from `read` once one has changed.
fn remembered<T: Copy, const N: usize>(
    seen: &Mutex<Option<([Stamp; N], T)>>,
    files: [&Path; N],
    read: impl FnOnce() -> T,
) -> T {
    // Stamped before reading, so a save that lands during the read leaves
    // a stamp that no longer matches, and the next call reads again.
    let stamps = files.map(stamp);
    let Ok(mut seen) = seen.lock() else {
        return read();
    };
    if let Some((was, ready)) = *seen
        && was == stamps
    {
        return ready;
    }
    let ready = read();
    *seen = Some((stamps, ready));
    ready
}

/// Where a service stands: a short status, whether it works, and whether it
/// is set up at all.
pub fn status(
    name: &str,
    choice: &Choice,
    keys: &HashMap<String, String>,
) -> (&'static str, bool, bool) {
    // Only the keyless one's stands on the settings, so only it reads them.
    let keyless = service(name).is_some_and(|s| s.keyless) && crate::store::keyless_search();
    status_with(name, choice, keys, keyless)
}

/// `status`, with `keyless` saying whether the settings let DuckDuckGo
/// search.
fn status_with(
    name: &str,
    choice: &Choice,
    keys: &HashMap<String, String>,
    keyless: bool,
) -> (&'static str, bool, bool) {
    let found = service(name);
    let set_up = match found {
        Some(s) if s.keyless => true,
        Some(s) if s.self_hosted() => !choice.address(s).is_empty(),
        Some(_) => keys.get(name).is_some_and(|key| !key.trim().is_empty()),
        None => false,
    };
    let in_use = picked(choice).is_some_and(|s| s.name == name);
    match (set_up, in_use) {
        // Nothing of it was saved: it needs nothing, and only the settings
        // say whether it is used.
        _ if found.is_some_and(|s| s.keyless) && !keyless => ("Switched off", false, true),
        (true, true) => ("In use", true, true),
        (true, false) if found.is_some_and(|s| s.keyless) => ("Not in use", false, true),
        (true, false) => ("Saved, not in use", false, true),
        (false, _) => ("Not set up", false, false),
    }
}

pub fn service(name: &str) -> Option<&'static Service> {
    SERVICES.iter().find(|s| s.name == name)
}

/// The service searches go to: the one chosen, or with none chosen the
/// keyless one, which the settings may switch off.
fn picked(choice: &Choice) -> Option<&'static Service> {
    match choice.service.as_str() {
        "" => SERVICES.iter().find(|s| s.keyless),
        name => service(name),
    }
}

/// What searching needs: the service, its key (none for SearXNG) and the
/// address to reach it at.
struct Reach {
    service: &'static Service,
    key: String,
    address: String,
}

/// The service to search with, and how to reach it; `keyless` says whether
/// the settings let DuckDuckGo search.
fn usable(choice: &Choice, keys: &HashMap<String, String>, keyless: bool) -> Result<Reach, String> {
    let Some(service) = picked(choice) else {
        return Err(
            "No search service is set up: add one in Settings, Search, or with /search services."
                .into(),
        );
    };
    if service.keyless && !keyless {
        return Err(SWITCHED_OFF.into());
    }
    let key = keys
        .get(service.name)
        .map(|key| key.trim().to_owned())
        .unwrap_or_default();
    if !service.self_hosted() && !service.keyless && key.is_empty() {
        return Err(format!("{} has no key saved.", service.name));
    }
    let address = web_address(&choice.address(service), service.self_hosted());
    if address.is_empty() {
        return Err(format!("{} has no address saved.", service.name));
    }
    Ok(Reach {
        service,
        key,
        address,
    })
}

/// An address as the user pasted it, made one to send a request to. A
/// SearXNG's is its home page, with or without its search page on the end.
fn web_address(pasted: &str, self_hosted: bool) -> String {
    let mut address = pasted.trim().trim_end_matches('/').to_owned();
    if self_hosted && let Some(home) = address.strip_suffix("/search") {
        address = home.to_owned();
    }
    if !address.is_empty() && !address.contains("://") {
        // A server of the user's own is seldom https; a service's always is.
        let scheme = if self_hosted { "http" } else { "https" };
        address = format!("{scheme}://{address}");
    }
    address
}

/// The model's way to search the web.
pub struct SearchWeb;

impl Tool for SearchWeb {
    fn hides_key_shapes(&self) -> bool {
        true
    }
    fn name(&self) -> &'static str {
        "search_web"
    }
    fn description(&self) -> &'static str {
        "Search the web. Returns a numbered list of results, each with its title, address and a snippet; read a page in full with web_fetch. allowed_domains keeps only results from those sites and blocked_domains drops them, subdomains included; a site in both is blocked. Some services apply site filters after answering, so fewer results may come back. freshness keeps to pages from the last day, week, month or year. The same search again within 10 minutes gives the earlier answer."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "query": {"type": "string", "description": "What to search for, as you would type it into a search engine."},
            "count": {"type": "integer", "description": "How many results, 1 to 10. Left out, 5."},
            "allowed_domains": {"type": "array", "items": {"type": "string"}, "description": "Only results from these sites and their subdomains, such as [\"docs.rs\"]."},
            "blocked_domains": {"type": "array", "items": {"type": "string"}, "description": "No results from these sites or their subdomains."},
            "freshness": {"type": "string", "enum": ["day", "week", "month", "year"], "description": "Only pages from this last stretch of time."}
        }, "required": ["query"]})
    }
    fn run(&self, args: &Value, _: &ToolContext) -> Result<String, String> {
        let query = args["query"].as_str().unwrap_or_default().trim();
        if query.is_empty() {
            return Err("Say what to search for.".into());
        }
        let count = args["count"]
            .as_u64()
            .unwrap_or(RESULTS)
            .clamp(1, MOST_RESULTS);
        let options = options(args)?;
        let keyless = crate::store::keyless_search();
        let reach = usable(&choice(), &crate::store::load_keys(), keyless)?;
        let asked = Asked {
            service: reach.service.name,
            address: reach.address.clone(),
            query: query.to_owned(),
            count,
            options,
        };
        searched_once(memo(), asked, Instant::now(), |asked| {
            search(&reach, query, count, &asked.options)
        })
    }
    fn needs_approval(&self) -> bool {
        true
    }
    fn concurrent(&self) -> bool {
        true
    }

    fn untrusted(&self) -> bool {
        true
    }
    fn preview(&self, args: &Value, _: &ToolContext) -> String {
        let query = args["query"].as_str().unwrap_or_default();
        // Tavily and Exa are sent the sites too, and DuckDuckGo a single
        // allowed one as site: in the query, so the card and the judge
        // see every word that leaves, not just the query. Filters that do
        // not parse stop the call before anything is sent.
        let filters = options(args)
            .map(|options| filters(&options))
            .unwrap_or_default();
        let keyless = crate::store::keyless_search();
        let search = match usable(&choice(), &crate::store::load_keys(), keyless) {
            Ok(reach) if reach.address == reach.service.official => {
                format!("Search {} for \"{query}\"", reach.service.name)
            }
            Ok(reach) => format!(
                "Search {} at {} for \"{query}\"",
                reach.service.name, reach.address
            ),
            Err(_) => format!("Search for \"{query}\""),
        };
        format!("{search}{filters}")
    }
}

/// The filters as the approval card lists them, each on a line of its own,
/// or nothing when there are none.
fn filters(options: &Options) -> String {
    let mut shown = String::new();
    if !options.allowed.is_empty() {
        shown.push_str(&format!("\nonly from {}", options.allowed.join(", ")));
    }
    if !options.blocked.is_empty() {
        shown.push_str(&format!("\nnot from {}", options.blocked.join(", ")));
    }
    if let Some(freshness) = options.freshness {
        shown.push_str(&format!("\nfrom the last {}", freshness.word()));
    }
    shown
}

/// One result, as every service's answer comes down to.
#[derive(PartialEq, Debug)]
struct Hit {
    title: String,
    url: String,
    snippet: String,
}

/// How far back a search reaches.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Freshness {
    Day,
    Week,
    Month,
    Year,
}

impl Freshness {
    /// The word Tavily and SearXNG take, and the model gives.
    fn word(self) -> &'static str {
        match self {
            Freshness::Day => "day",
            Freshness::Week => "week",
            Freshness::Month => "month",
            Freshness::Year => "year",
        }
    }

    /// Brave's code for it.
    fn brave(self) -> &'static str {
        match self {
            Freshness::Day => "pd",
            Freshness::Week => "pw",
            Freshness::Month => "pm",
            Freshness::Year => "py",
        }
    }

    /// DuckDuckGo's code for it.
    fn duckduckgo(self) -> &'static str {
        match self {
            Freshness::Day => "d",
            Freshness::Week => "w",
            Freshness::Month => "m",
            Freshness::Year => "y",
        }
    }

    /// The first day it reaches back to, which is how Exa takes it.
    fn since(self, today: chrono::NaiveDate) -> chrono::NaiveDate {
        let days = match self {
            Freshness::Day => 1,
            Freshness::Week => 7,
            Freshness::Month => 30,
            Freshness::Year => 365,
        };
        today - chrono::Days::new(days)
    }
}

/// Which sites and how recent, as the model asked.
#[derive(Clone, Default, PartialEq, Eq, Hash, Debug)]
struct Options {
    allowed: Vec<String>,
    blocked: Vec<String>,
    freshness: Option<Freshness>,
}

impl Options {
    fn by_site(&self) -> bool {
        !self.allowed.is_empty() || !self.blocked.is_empty()
    }
}

/// The filters in a search_web call, checked.
fn options(args: &Value) -> Result<Options, String> {
    let freshness = match args["freshness"]
        .as_str()
        .map(|word| word.trim().to_lowercase())
    {
        None if args["freshness"].is_null() => None,
        Some(word) if word.is_empty() => None,
        Some(word) if word == "day" => Some(Freshness::Day),
        Some(word) if word == "week" => Some(Freshness::Week),
        Some(word) if word == "month" => Some(Freshness::Month),
        Some(word) if word == "year" => Some(Freshness::Year),
        _ => return Err("freshness is one of day, week, month or year.".into()),
    };
    Ok(Options {
        allowed: domains(&args["allowed_domains"], "allowed_domains")?,
        blocked: domains(&args["blocked_domains"], "blocked_domains")?,
        freshness,
    })
}

/// A list of sites, each made a bare domain; sorted, so the same sites in
/// another order are the same search to the memo.
fn domains(value: &Value, field: &str) -> Result<Vec<String>, String> {
    // A model asked for a list sometimes sends the one site on its own.
    let listed: Vec<&str> = match value {
        Value::Null => return Ok(Vec::new()),
        Value::String(one) => vec![one],
        Value::Array(items) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .ok_or_else(|| format!("{field} is a list of domains, each a string."))
            })
            .collect::<Result<_, _>>()?,
        _ => {
            return Err(format!(
                "{field} is a list of domains, such as [\"example.com\"]."
            ));
        }
    };
    let mut found = listed
        .into_iter()
        .map(domain)
        .collect::<Result<Vec<_>, _>>()?;
    found.sort();
    found.dedup();
    Ok(found)
}

/// A site as the model wrote it -- perhaps with its scheme, a path or a
/// leading `*.` -- as the bare domain it names.
fn domain(written: &str) -> Result<String, String> {
    let lower = written.trim().to_lowercase();
    let bare = lower
        .split_once("://")
        .map_or(lower.as_str(), |(_, rest)| rest);
    let bare = bare.split(['/', '?', '#']).next().unwrap_or_default();
    let bare = bare
        .trim_start_matches("*.")
        .trim_start_matches('.')
        .trim_end_matches('.');
    let fits = bare.contains('.')
        && bare.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
    if !fits {
        return Err(format!(
            "\"{}\" is not a domain: give one such as example.com.",
            written.trim()
        ));
    }
    Ok(bare.to_owned())
}

/// Whether a result at `url` is one the filters keep: from an allowed site
/// if any are given, and never from a blocked one, subdomains included.
fn wanted(url: &str, options: &Options) -> bool {
    if !options.by_site() {
        return true;
    }
    let Some(host) = reqwest::Url::parse(url).ok().and_then(|url| {
        url.host_str()
            .map(|host| host.trim_end_matches('.').to_lowercase())
    }) else {
        return false;
    };
    let under = |domain: &String| {
        host == *domain
            || host
                .strip_suffix(domain.as_str())
                .is_some_and(|rest| rest.ends_with('.'))
    };
    if options.blocked.iter().any(under) {
        return false;
    }
    options.allowed.is_empty() || options.allowed.iter().any(under)
}

/// Whether a service narrows a search by site itself; the others are
/// narrowed here, once they answer.
fn narrows_by_site(service: &str) -> bool {
    matches!(service, "Tavily" | "Exa")
}

/// What the services asked with a GET are sent: Brave, SearXNG and
/// DuckDuckGo.
fn parameters(
    service: &str,
    query: &str,
    count: u64,
    options: &Options,
) -> Vec<(&'static str, String)> {
    let mut sent = vec![("q", query.to_owned())];
    if service == "DuckDuckGo" {
        // It takes no list of sites, but one site is narrowed to as a person
        // would type it; the rest are filtered here. It answers a page of
        // results whatever the count.
        if let [site] = options.allowed.as_slice() {
            sent[0].1 = format!("{query} site:{site}");
        }
        if let Some(freshness) = options.freshness {
            sent.push(("df", freshness.duckduckgo().to_owned()));
        }
    } else if service == "Brave" {
        let count = match options.by_site() {
            true => WIDER,
            false => count,
        };
        sent.push(("count", count.to_string()));
        if let Some(freshness) = options.freshness {
            sent.push(("freshness", freshness.brave().to_owned()));
        }
    } else {
        sent.push(("format", "json".to_owned()));
        if let Some(freshness) = options.freshness {
            sent.push(("time_range", freshness.word().to_owned()));
        }
    }
    sent
}

/// What the services asked with a POST are sent: Tavily and Exa.
fn payload(
    service: &str,
    query: &str,
    count: u64,
    options: &Options,
    today: chrono::NaiveDate,
) -> Value {
    if service == "Tavily" {
        let mut sent = json!({"query": query, "max_results": count});
        if !options.allowed.is_empty() {
            sent["include_domains"] = json!(options.allowed);
        }
        if !options.blocked.is_empty() {
            sent["exclude_domains"] = json!(options.blocked);
        }
        if let Some(freshness) = options.freshness {
            sent["time_range"] = freshness.word().into();
        }
        return sent;
    }
    let mut sent = json!({"query": query, "numResults": count, "contents": {"highlights": {"maxCharacters": SNIPPET}}});
    // Exa turns down both lists at once; the allowed one narrows the most,
    // and a blocked subdomain of it is still dropped here.
    if !options.allowed.is_empty() {
        sent["includeDomains"] = json!(options.allowed);
    } else if !options.blocked.is_empty() {
        sent["excludeDomains"] = json!(options.blocked);
    }
    if let Some(freshness) = options.freshness {
        sent["startPublishedDate"] = format!(
            "{}T00:00:00.000Z",
            freshness.since(today).format("%Y-%m-%d")
        )
        .into();
    }
    sent
}

/// A search as the memo knows it: the same service at the same address,
/// asked the same thing.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Asked {
    service: &'static str,
    address: String,
    query: String,
    count: u64,
    options: Options,
}

/// Searches answered in the last `MEMO_TIME`, kept only in memory: a model
/// that asks again a moment later gets the same list without spending
/// another search of the user's plan.
#[derive(Default)]
struct Memo {
    kept: HashMap<Asked, (Instant, String)>,
}

impl Memo {
    fn recall(&self, asked: &Asked, now: Instant) -> Option<String> {
        let (at, listing) = self.kept.get(asked)?;
        (now.saturating_duration_since(*at) < MEMO_TIME).then(|| listing.clone())
    }

    fn keep(&mut self, asked: Asked, listing: String, now: Instant) {
        self.kept
            .retain(|_, (at, _)| now.saturating_duration_since(*at) < MEMO_TIME);
        self.kept.insert(asked, (now, listing));
    }
}

fn memo() -> &'static Mutex<Memo> {
    static MEMO: OnceLock<Mutex<Memo>> = OnceLock::new();
    MEMO.get_or_init(Default::default)
}

/// The answer to `asked` from the memo, with a note that it is, or from
/// `search` when there is none still fresh. A failed search is not kept, so
/// asking again tries again.
fn searched_once(
    memo: &Mutex<Memo>,
    asked: Asked,
    now: Instant,
    search: impl FnOnce(&Asked) -> Result<String, String>,
) -> Result<String, String> {
    // Not held across the search, so searches running side by side wait
    // only for each other's lookups.
    let recalled = memo.lock().ok().and_then(|memo| memo.recall(&asked, now));
    if let Some(listing) = recalled {
        return Ok(format!(
            "(The same search was made a few minutes ago; this is its answer again.)\n\n{listing}"
        ));
    }
    let listing = search(&asked)?;
    if let Ok(mut memo) = memo.lock() {
        memo.keep(asked, listing.clone(), now);
    }
    Ok(listing)
}

/// Ask the service for `query`, and list what it found that the filters keep;
/// DuckDuckGo one search at a time, and not while it is held off.
fn search(reach: &Reach, query: &str, count: u64, options: &Options) -> Result<String, String> {
    if !reach.service.keyless {
        return ask(reach, query, count, options);
    }
    static LIMITED_AT: Mutex<Option<Instant>> = Mutex::new(None);
    unless_held_off(&LIMITED_AT, Instant::now(), || {
        ask(reach, query, count, options)
    })
}

/// The answer from `ask`, unless DuckDuckGo limited a search less than
/// `HOLD_OFF` before `now`, when nothing is sent. A limit in the answer
/// starts the wait.
fn unless_held_off(
    limited_at: &Mutex<Option<Instant>>,
    now: Instant,
    ask: impl FnOnce() -> Result<String, String>,
) -> Result<String, String> {
    // Held across the request, so searches the model sends side by side
    // reach it one at a time, and none goes out once one was limited.
    let mut at = limited_at
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if at.is_some_and(|at| now.saturating_duration_since(at) < HOLD_OFF) {
        return Err(LIMITED.into());
    }
    let answer = ask();
    if answer.as_ref().is_err_and(|error| error == LIMITED) {
        *at = Some(now);
    }
    answer
}

/// Send the one request a search takes, and list what came back that the
/// filters keep.
fn ask(reach: &Reach, query: &str, count: u64, options: &Options) -> Result<String, String> {
    let Reach {
        service,
        key,
        address,
    } = reach;
    let mut client = reqwest::blocking::Client::builder()
        .timeout(SEARCH_TIME)
        .user_agent(concat!("Letronna/", env!("CARGO_PKG_VERSION")));
    if service.keyless {
        // A bang or a leading \ in the query makes it answer with a redirect
        // to some other site, perhaps one on this machine or its network:
        // followed, the query would go where the approval card never said.
        client = client.redirect(reqwest::redirect::Policy::none());
    }
    let client = client.build().map_err(|e| e.to_string())?;
    let today = chrono::Utc::now().date_naive();
    let request = match service.name {
        "Brave" => client
            .get(address)
            .query(&parameters(service.name, query, count, options))
            .header("Accept", "application/json")
            .header("X-Subscription-Token", key),
        "Tavily" => client.post(address).bearer_auth(key).json(&payload(
            service.name,
            query,
            count,
            options,
            today,
        )),
        "Exa" => client.post(address).header("x-api-key", key).json(&payload(
            service.name,
            query,
            count,
            options,
            today,
        )),
        "DuckDuckGo" => client.get(format!("{address}/")).query(&parameters(
            service.name,
            query,
            count,
            options,
        )),
        _ => client.get(format!("{address}/search")).query(&parameters(
            service.name,
            query,
            count,
            options,
        )),
    };
    let reply = request.send().map_err(|e| match e.is_timeout() {
        true => format!(
            "{} did not answer within {} seconds.",
            service.name,
            SEARCH_TIME.as_secs()
        ),
        false => format!("Could not reach {} at {address}: {e}", service.name),
    })?;
    let status = reply.status();
    // The site a redirect points to, which only DuckDuckGo's are left unfollowed.
    let elsewhere = reply
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|to| to.to_str().ok())
        .and_then(|to| reply.url().join(to).ok())
        .and_then(|to| to.host_str().map(str::to_owned));
    let text = reply.text().unwrap_or_default();
    // Every answer goes through the filters, a service that narrowed by
    // site too: it may not count subdomains the same way, and only Tavily
    // takes both lists at once.
    let wider = match options.by_site() && !narrows_by_site(service.name) {
        true => usize::MAX,
        false => count as usize,
    };
    if service.keyless {
        // Its challenge to prove a person is searching comes back as HTTP
        // 202, and a block as 403 or 429; with no key to refuse, a 401 is a
        // block too.
        // Sent on rather than answered is the query's doing, not a limit, so
        // it neither reads as one nor holds the next search off.
        if status.is_redirection() {
            let to = elsewhere
                .map(|host| format!(" to {host}"))
                .unwrap_or_default();
            // It may redirect for reasons of its own too, where dropping a
            // bang the query does not have would be no help.
            let next = if sends_on(query) {
                ", as it does for a word starting with ! or a query starting with \\. It was \
                 not followed: search again without them."
            } else {
                ". It was not followed: read a page you know with web_fetch, or search again \
                 later."
            };
            return Err(format!(
                "DuckDuckGo answered by sending this search on{to} rather than listing results{next}"
            ));
        }
        let found = match status.as_u16() {
            202 | 401 | 403 | 429 => None,
            _ if !status.is_success() => {
                return Err(refusal(service, status.as_u16(), &text));
            }
            _ => duckduckgo_hits(&text, wider),
        };
        let Some(found) = found else {
            return Err(LIMITED.into());
        };
        return Ok(kept(service.name, query, found, count, options));
    }
    if !status.is_success() {
        return Err(refusal(service, status.as_u16(), &text));
    }
    let body: Value = serde_json::from_str(&text).map_err(|_| {
        format!(
            "{} answered with something other than search results.",
            service.name
        )
    })?;
    Ok(kept(
        service.name,
        query,
        hits(service.name, &body, wider),
        count,
        options,
    ))
}

/// The listing of the hits the filters keep, up to `count`. When the filters
/// drop every one, it says so, so the model does not take a narrowed search
/// for a subject nobody has written about.
fn kept(service: &str, query: &str, found: Vec<Hit>, count: u64, options: &Options) -> String {
    let any = !found.is_empty();
    let hits: Vec<Hit> = found
        .into_iter()
        .filter(|hit| wanted(&hit.url, options))
        .take(count as usize)
        .collect();
    if any && hits.is_empty() {
        return format!(
            "{service} found results for \"{query}\", but none from the sites allowed and not blocked."
        );
    }
    listing(service, query, &hits)
}

/// Why a service turned a search down, in words the user can act on.
fn refusal(service: &Service, status: u16, body: &str) -> String {
    let name = service.name;
    match status {
        401 | 403 if service.self_hosted() => format!(
            "{name} refused to answer in JSON (HTTP {status}): turn on the json format in its \
             settings.yml, under search: formats."
        ),
        401 | 403 => {
            format!("{name} refused the key (HTTP {status}): check it in Settings, Search.")
        }
        429 => format!(
            "{name} says there have been too many searches (HTTP 429): the plan's limit is reached for now."
        ),
        _ => {
            let said: String = body.chars().take(200).collect();
            crate::untrusted::quoted(&format!("{name} answered HTTP {status}:"), said.trim())
        }
    }
}

/// The results in a service's answer, in its own shape.
fn hits(service: &str, body: &Value, count: usize) -> Vec<Hit> {
    let (list, snippet): (&Value, fn(&Value) -> String) = match service {
        "Brave" => (&body["web"]["results"], |r| text(&r["description"])),
        "Exa" => (&body["results"], |r| match r["highlights"].as_array() {
            Some(parts) if !parts.is_empty() => {
                parts.iter().map(text).collect::<Vec<_>>().join(" … ")
            }
            _ => text(&r["text"]),
        }),
        // Tavily and SearXNG both call it the content.
        _ => (&body["results"], |r| text(&r["content"])),
    };
    list.as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|r| r["url"].as_str().is_some_and(|url| !url.is_empty()))
        .take(count)
        .map(|r| Hit {
            title: text(&r["title"]),
            url: r["url"].as_str().unwrap_or_default().to_owned(),
            snippet: snippet(r),
        })
        .collect()
}

/// A field's text with its markup gone -- Brave marks the words that matched
/// -- and its spacing made plain.
fn text(value: &Value) -> String {
    let raw = value.as_str().unwrap_or_default();
    let mut plain = String::with_capacity(raw.len());
    let mut in_tag = false;
    for c in raw.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => plain.push(c),
            _ => {}
        }
    }
    let plain = plain
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&");
    plain.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether `query` has what makes DuckDuckGo send a search on to another
/// site: a word starting with ! or a \ before it all.
fn sends_on(query: &str) -> bool {
    query.trim_start().starts_with('\\') || query.split_whitespace().any(|w| w.starts_with('!'))
}

/// The results on a DuckDuckGo page, up to `count`, its ads left out; `None`
/// when the page is no list of results -- its challenge to prove a person is
/// searching, or a page with neither results nor its note that there are
/// none -- which is DuckDuckGo limiting searches from here.
fn duckduckgo_hits(page: &str, count: usize) -> Option<Vec<Hit>> {
    // Read by the class of each tag, never by text the page holds: the
    // query is written back into it, and could hold any marker.
    let mut hits = Vec::new();
    // The result whose snippet comes next; none after an ad, so the ad's
    // snippet goes with it.
    let mut open: Option<Hit> = None;
    let (mut listed, mut none_found) = (false, false);
    let mut at = 0;
    while let Some(start) = page[at..].find('<').map(|i| at + i) {
        let Some(end) = page[start..].find('>').map(|i| start + i) else {
            break;
        };
        let tag = &page[start + 1..end];
        at = end + 1;
        // Its form fields hold the query in their values, where a name is
        // read without minding quotes and could be any marker.
        let name = tag
            .split(|c: char| c.is_ascii_whitespace() || c == '/')
            .next()
            .unwrap_or_default();
        if name.eq_ignore_ascii_case("input") {
            continue;
        }
        let class = crate::web::attribute(tag, "class").unwrap_or_default();
        let classes: Vec<&str> = class.split_whitespace().collect();
        if classes.iter().any(|c| c.starts_with("anomaly-modal"))
            || crate::web::attribute(tag, "id").as_deref() == Some("challenge-form")
        {
            return None;
        }
        none_found |= classes
            .iter()
            .any(|c| matches!(*c, "no-results" | "result--no-result"));
        let title = classes.contains(&"result__a");
        if !title && !classes.contains(&"result__snippet") {
            continue;
        }
        let close = page[at..].find("</a>").map_or(page.len(), |i| at + i);
        let words = words(&page[at..close]);
        at = close;
        if title {
            listed = true;
            hits.extend(open.take());
            open = crate::web::attribute(tag, "href")
                .as_deref()
                .and_then(landing)
                .map(|url| Hit {
                    title: words,
                    url,
                    snippet: String::new(),
                });
        } else if let Some(hit) = open.as_mut() {
            hit.snippet = words;
        }
    }
    hits.extend(open);
    if !listed && !none_found {
        return None;
    }
    hits.truncate(count);
    Some(hits)
}

/// Where a DuckDuckGo result leads: it links each through an address of its
/// own with the real one in `uddg`. `None` for its ads, which lead through
/// duckduckgo.com/y.js, and for anything that is not a web page.
fn landing(href: &str) -> Option<String> {
    let link = reqwest::Url::parse("https://duckduckgo.com/")
        .ok()?
        .join(href)
        .ok()?;
    let real = link
        .query_pairs()
        .find(|(name, _)| name == "uddg")
        .map_or_else(|| link.to_string(), |(_, real)| real.into_owned());
    let url = reqwest::Url::parse(&real).ok()?;
    let ad = url.host_str() == Some("duckduckgo.com") && url.path() == "/y.js";
    (matches!(url.scheme(), "http" | "https") && !ad).then_some(real)
}

/// A piece of a page as the words a reader sees: its tags gone, entities
/// written out and spacing made plain.
fn words(html: &str) -> String {
    let mut plain = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        plain.push_str(&rest[..open]);
        rest = rest[open..]
            .find('>')
            .map_or("", |close| &rest[open + close + 1..]);
    }
    plain.push_str(rest);
    let plain = crate::web::decode(&plain);
    plain.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The numbered list the model reads.
fn listing(service: &str, query: &str, hits: &[Hit]) -> String {
    if hits.is_empty() {
        return format!("{service} found nothing for \"{query}\".");
    }
    let mut out = format!("{service} results for \"{query}\":\n");
    for (i, hit) in hits.iter().enumerate() {
        let title = if hit.title.is_empty() {
            &hit.url
        } else {
            &hit.title
        };
        out.push_str(&format!("\n{}. {title}\n   {}\n", i + 1, hit.url));
        if !hit.snippet.is_empty() {
            let snippet: String = hit.snippet.chars().take(SNIPPET).collect();
            let cut = if hit.snippet.chars().count() > SNIPPET {
                "…"
            } else {
                ""
            };
            out.push_str(&format!("   {snippet}{cut}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_service_answers_in_its_own_shape() {
        let brave = json!({"web": {"results": [
            {"title": "Rust <strong>language</strong>", "url": "https://rust-lang.org/", "description": "A <strong>language</strong> empowering everyone &amp; more"},
            {"title": "No address", "url": "", "description": "skipped"}
        ]}});
        assert_eq!(
            hits("Brave", &brave, 5),
            vec![Hit {
                title: "Rust language".into(),
                url: "https://rust-lang.org/".into(),
                snippet: "A language empowering everyone & more".into(),
            }]
        );
        let tavily = json!({"query": "q", "results": [{"title": "T", "url": "https://t.example/", "content": "the  text\nthat matters", "score": 0.9}]});
        assert_eq!(
            hits("Tavily", &tavily, 5)[0].snippet,
            "the text that matters"
        );
        let exa = json!({"results": [
            {"title": "E", "url": "https://e.example/", "highlights": ["one part", "another"]},
            {"title": "F", "url": "https://f.example/", "text": "whole text"}
        ]});
        let found = hits("Exa", &exa, 5);
        assert_eq!(found[0].snippet, "one part … another");
        assert_eq!(found[1].snippet, "whole text");
        let searxng = json!({"results": [
            {"title": "S1", "url": "https://s1.example/", "content": "first"},
            {"title": "S2", "url": "https://s2.example/", "content": "second"}
        ]});
        assert_eq!(
            hits("SearXNG", &searxng, 1).len(),
            1,
            "no more than asked for"
        );
        assert!(hits("Brave", &json!({"error": "nope"}), 5).is_empty());
    }

    #[test]
    fn a_listing_is_numbered_and_its_snippets_kept_short() {
        let hits = vec![
            Hit {
                title: "One".into(),
                url: "https://one.example/".into(),
                snippet: "x".repeat(SNIPPET + 50),
            },
            Hit {
                title: String::new(),
                url: "https://two.example/".into(),
                snippet: String::new(),
            },
        ];
        let shown = listing("Brave", "cats", &hits);
        assert!(shown.starts_with("Brave results for \"cats\""), "{shown}");
        assert!(
            shown.contains("\n1. One\n   https://one.example/\n"),
            "{shown}"
        );
        assert!(
            shown.contains(&format!("   {}…\n", "x".repeat(SNIPPET))),
            "{shown}"
        );
        assert!(
            shown.contains("\n2. https://two.example/\n   https://two.example/\n"),
            "{shown}"
        );
        assert_eq!(
            listing("Exa", "cats", &[]),
            "Exa found nothing for \"cats\"."
        );
    }

    #[test]
    fn an_address_is_taken_as_pasted() {
        assert_eq!(web_address("http://mini:8888/", true), "http://mini:8888");
        assert_eq!(
            web_address("https://search.example.org/search", true),
            "https://search.example.org"
        );
        assert_eq!(web_address(" mini:8888 ", true), "http://mini:8888");
        // A service's own address keeps its path, and is https unless it says.
        assert_eq!(
            web_address("proxy.example/brave/search", false),
            "https://proxy.example/brave/search"
        );
        assert_eq!(web_address("", true), "");
    }

    #[test]
    fn a_service_is_reached_at_its_official_address_until_given_another() {
        let brave = service("Brave").unwrap();
        let mut choice = Choice::default();
        assert_eq!(choice.address(brave), brave.official);
        choice.set_address(brave, "http://localhost:9000/search");
        assert_eq!(choice.address(brave), "http://localhost:9000/search");
        // Reset: emptied, or given the official one, it is not kept at all.
        choice.set_address(brave, "  ");
        assert_eq!(choice.address(brave), brave.official);
        choice.set_address(brave, brave.official);
        assert!(choice.addresses.is_empty());
        // SearXNG has no official address to go back to.
        let searxng = service("SearXNG").unwrap();
        assert_eq!(choice.address(searxng), "");
    }

    #[test]
    fn the_service_used_is_the_one_chosen_and_set_up() {
        let keys = HashMap::from([("Brave".to_owned(), "key".to_owned())]);
        let brave = Choice {
            service: "Brave".into(),
            ..Choice::default()
        };
        let reach = usable(&brave, &keys, true).unwrap();
        assert_eq!((reach.service.name, reach.key.as_str()), ("Brave", "key"));
        assert_eq!(reach.address, service("Brave").unwrap().official);
        assert_eq!(status("Brave", &brave, &keys), ("In use", true, true));
        assert_eq!(
            status("Tavily", &brave, &keys),
            ("Not set up", false, false)
        );

        let tavily = Choice {
            service: "Tavily".into(),
            ..Choice::default()
        };
        assert!(usable(&tavily, &keys, true).is_err(), "chosen, but no key");
        assert_eq!(
            status("Brave", &tavily, &keys),
            ("Saved, not in use", false, true)
        );

        let mut searxng = Choice {
            service: "SearXNG".into(),
            ..Choice::default()
        };
        assert!(usable(&searxng, &keys, true).is_err(), "no address yet");
        searxng.set_address(service("SearXNG").unwrap(), "mini:8888");
        assert_eq!(
            usable(&searxng, &keys, true).unwrap().address,
            "http://mini:8888"
        );
        // Its address is kept while another service is used.
        let back_to_brave = Choice {
            service: "Brave".into(),
            ..searxng
        };
        assert_eq!(
            status("SearXNG", &back_to_brave, &keys),
            ("Saved, not in use", false, true)
        );
        assert!(
            usable(&Choice::default(), &keys, false).is_err(),
            "nothing chosen, DuckDuckGo switched off"
        );
    }

    #[test]
    fn ready_is_read_from_disk_only_when_a_file_changed() {
        let dir = std::env::temp_dir().join(format!("letronna-ready-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (search, keys) = (dir.join("search.json"), dir.join("keys.json"));
        // Each save at a second of its own: two within one tick of a coarse
        // clock would carry the same time.
        let save = |path: &Path, text: &str, second: u64| {
            std::fs::write(path, text).unwrap();
            let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000 + second);
            let file = std::fs::File::options().write(true).open(path).unwrap();
            file.set_modified(at).unwrap();
        };
        let seen = Mutex::new(None);
        let reads = std::cell::Cell::new(0);
        let ready = || {
            remembered(&seen, [&search, &keys], || {
                reads.set(reads.get() + 1);
                std::fs::read_to_string(&keys).is_ok()
            })
        };

        save(&search, r#"{"service": "Brave"}"#, 0);
        assert!(!ready());
        assert!(!ready());
        assert_eq!(reads.get(), 1, "nothing changed in between");
        // The other process saves the key.
        save(&keys, r#"{"Brave": "key-1"}"#, 1);
        assert!(ready());
        assert!(ready());
        assert_eq!(reads.get(), 2);
        // Saved again at the same length, its time still tells.
        save(&keys, r#"{"Brave": "key-2"}"#, 2);
        assert!(ready());
        assert_eq!(reads.get(), 3);
        // Saved within the same second, its length tells.
        save(&keys, r#"{"Brave": "a-longer-key"}"#, 2);
        assert!(ready());
        assert_eq!(reads.get(), 4);
        // And a file taken away is a change too.
        std::fs::remove_file(&search).unwrap();
        assert!(ready());
        assert!(ready());
        assert_eq!(reads.get(), 5);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_search_goes_to_the_address_given() {
        // A stand-in for Brave that answers one search and says what it was asked.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let asked = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let body = r#"{"web": {"results": [{"title": "Here", "url": "https://here.example/", "description": "found"}]}}"#;
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).unwrap();
            String::from_utf8_lossy(&request[..read]).into_owned()
        });
        let reach = Reach {
            service: service("Brave").unwrap(),
            key: "secret".into(),
            address: format!("http://127.0.0.1:{port}/brave/search"),
        };
        let found = search(&reach, "cats", 3, &Options::default()).unwrap();
        assert!(
            found.contains("1. Here\n   https://here.example/\n   found"),
            "{found}"
        );
        let request = asked.join().unwrap().to_lowercase();
        assert!(
            request.starts_with("get /brave/search?q=cats&count=3 "),
            "{request}"
        );
        assert!(
            request.contains("x-subscription-token: secret"),
            "{request}"
        );
    }

    #[test]
    fn each_service_is_asked_to_filter_in_its_own_words() {
        let options = Options {
            allowed: vec!["docs.rs".into()],
            blocked: vec!["spam.docs.rs".into()],
            freshness: Some(Freshness::Week),
        };
        let today = chrono::NaiveDate::from_ymd_opt(2026, 3, 4).unwrap();
        let brave = parameters("Brave", "rust", 5, &options);
        // Brave cannot narrow by site, so it is asked for more to filter here.
        assert_eq!(
            brave,
            vec![
                ("q", "rust".to_owned()),
                ("count", WIDER.to_string()),
                ("freshness", "pw".to_owned())
            ]
        );
        assert_eq!(
            parameters("Brave", "rust", 5, &Options::default()),
            vec![("q", "rust".to_owned()), ("count", "5".to_owned())]
        );
        assert_eq!(
            parameters("SearXNG", "rust", 5, &options),
            vec![
                ("q", "rust".to_owned()),
                ("format", "json".to_owned()),
                ("time_range", "week".to_owned())
            ]
        );
        assert_eq!(
            payload("Tavily", "rust", 5, &options, today),
            json!({"query": "rust", "max_results": 5, "include_domains": ["docs.rs"],
                   "exclude_domains": ["spam.docs.rs"], "time_range": "week"})
        );
        let exa = payload("Exa", "rust", 5, &options, today);
        assert_eq!(exa["includeDomains"], json!(["docs.rs"]));
        assert!(exa.get("excludeDomains").is_none(), "Exa takes one list");
        assert_eq!(exa["startPublishedDate"], "2026-02-25T00:00:00.000Z");
        let blocked_only = Options {
            blocked: vec!["spam.example".into()],
            ..Options::default()
        };
        assert_eq!(
            payload("Exa", "rust", 5, &blocked_only, today)["excludeDomains"],
            json!(["spam.example"])
        );
        assert_eq!(
            payload("Tavily", "rust", 5, &Options::default(), today),
            json!({"query": "rust", "max_results": 5})
        );
        // DuckDuckGo takes one site in the query, and more only here.
        assert_eq!(
            parameters("DuckDuckGo", "rust", 5, &options),
            vec![
                ("q", "rust site:docs.rs".to_owned()),
                ("df", "w".to_owned())
            ]
        );
        let two_sites = Options {
            allowed: vec!["docs.rs".into(), "rust-lang.org".into()],
            ..Options::default()
        };
        assert_eq!(
            parameters("DuckDuckGo", "rust", 5, &two_sites),
            vec![("q", "rust".to_owned())]
        );
    }

    #[test]
    fn the_card_and_the_judge_see_the_sites_that_are_sent() {
        let call = json!({
            "query": "rust",
            "allowed_domains": ["docs.rs", "secret-word.example.org"],
            "blocked_domains": ["spam.example"],
            "freshness": "week"
        });
        let ctx = ToolContext::default();
        let card = SearchWeb.preview(&call, &ctx);
        assert!(
            card.ends_with(
                "for \"rust\"\nonly from docs.rs, secret-word.example.org\nnot from spam.example\nfrom the last week"
            ),
            "{card}"
        );
        assert_eq!(SearchWeb.judge_input(&call, &ctx), card);
        let plain = SearchWeb.preview(&json!({"query": "rust"}), &ctx);
        assert!(plain.ends_with("for \"rust\""), "{plain}");
    }

    #[test]
    fn results_are_kept_by_site_with_subdomains_and_blocked_wins() {
        let options = Options {
            allowed: vec!["example.com".into()],
            blocked: vec!["ads.example.com".into()],
            freshness: None,
        };
        assert!(wanted("https://example.com/a", &options));
        assert!(wanted("https://docs.EXAMPLE.com./b", &options));
        assert!(!wanted("https://ads.example.com/c", &options));
        assert!(!wanted("https://x.ads.example.com/c", &options));
        // A name that only ends the same way is another site.
        assert!(!wanted("https://notexample.com/", &options));
        assert!(!wanted("https://example.com.evil.net/", &options));
        assert!(!wanted("not an address", &options));
        // A site in both lists is blocked.
        let both = Options {
            allowed: vec!["example.com".into()],
            blocked: vec!["example.com".into()],
            freshness: None,
        };
        assert!(!wanted("https://example.com/", &both));
        let blocked_only = Options {
            blocked: vec!["example.com".into()],
            ..Options::default()
        };
        assert!(wanted("https://other.org/", &blocked_only));
        assert!(!wanted("https://www.example.com/", &blocked_only));
        assert!(wanted("anything at all", &Options::default()));
    }

    #[test]
    fn a_search_the_filters_empty_does_not_read_as_finding_nothing() {
        let found = || {
            vec![Hit {
                title: "Elsewhere".into(),
                url: "https://other.example/".into(),
                snippet: String::new(),
            }]
        };
        let docs = Options {
            allowed: vec!["docs.rs".into()],
            ..Options::default()
        };
        assert_eq!(
            kept("Brave", "rust", found(), 5, &docs),
            "Brave found results for \"rust\", but none from the sites allowed and not blocked."
        );
        assert_eq!(
            kept("Brave", "rust", Vec::new(), 5, &docs),
            "Brave found nothing for \"rust\"."
        );
        assert!(kept("Brave", "rust", found(), 5, &Options::default()).contains("1. Elsewhere"));
    }

    #[test]
    fn filters_are_read_from_the_call_and_bad_ones_refused() {
        let read = options(&json!({
            "allowed_domains": ["https://Docs.rs/std/", "*.example.org", "docs.rs"],
            "blocked_domains": "spam.example.org",
            "freshness": "Month"
        }))
        .unwrap();
        assert_eq!(read.allowed, vec!["docs.rs", "example.org"]);
        assert_eq!(read.blocked, vec!["spam.example.org"]);
        assert_eq!(read.freshness, Some(Freshness::Month));
        assert_eq!(
            options(&json!({"freshness": ""})).unwrap(),
            Options::default()
        );
        assert_eq!(options(&json!({})).unwrap(), Options::default());

        for bad in [
            json!({"allowed_domains": ["localhost"]}),
            json!({"allowed_domains": ["exa mple.com"]}),
            json!({"allowed_domains": [""]}),
            json!({"blocked_domains": ["example.com:8080"]}),
            json!({"blocked_domains": ["-bad.example.com"]}),
            json!({"blocked_domains": [42]}),
            json!({"blocked_domains": {"site": "example.com"}}),
            json!({"freshness": "fortnight"}),
            json!({"freshness": 7}),
        ] {
            assert!(options(&bad).is_err(), "{bad}");
        }
        assert_eq!(
            options(&json!({"allowed_domains": ["localhost"]})).unwrap_err(),
            "\"localhost\" is not a domain: give one such as example.com."
        );
    }

    #[test]
    fn the_same_search_again_is_answered_from_the_memo_until_it_expires() {
        let memo = Mutex::new(Memo::default());
        let asked = Asked {
            service: "Brave",
            address: "https://brave.example/".into(),
            query: "cats".into(),
            count: 5,
            options: Options::default(),
        };
        let searches = std::cell::Cell::new(0);
        let search = |_: &Asked| {
            searches.set(searches.get() + 1);
            Ok(format!("listing {}", searches.get()))
        };
        let start = Instant::now();
        assert_eq!(
            searched_once(&memo, asked.clone(), start, search).unwrap(),
            "listing 1"
        );
        let again = searched_once(
            &memo,
            asked.clone(),
            start + Duration::from_secs(60),
            search,
        )
        .unwrap();
        assert!(again.ends_with("\n\nlisting 1"), "{again}");
        assert!(again.starts_with("(The same search"), "{again}");
        assert_eq!(searches.get(), 1);
        // Other filters are another search.
        let recent = Asked {
            options: Options {
                freshness: Some(Freshness::Day),
                ..Options::default()
            },
            ..asked.clone()
        };
        assert_eq!(
            searched_once(&memo, recent, start, search).unwrap(),
            "listing 2"
        );
        // Past its time, it is searched again.
        let later = start + MEMO_TIME + Duration::from_secs(1);
        assert_eq!(
            searched_once(&memo, asked.clone(), later, search).unwrap(),
            "listing 3"
        );
        // A failure is not kept.
        let failed = Asked {
            query: "dogs".into(),
            ..asked
        };
        assert!(searched_once(&memo, failed.clone(), later, |_| Err("down".into())).is_err());
        assert_eq!(
            searched_once(&memo, failed, later, search).unwrap(),
            "listing 4"
        );
    }

    /// A DuckDuckGo results page cut down from a real one: an instant
    /// answer, an ad, two results with marked words and entities, and its
    /// Next form, whose copy of the query names the markers.
    const RESULTS_PAGE: &str = r#"<html><head><title>rust at DuckDuckGo</title></head><body>
<input name="q" class="search__input" type="text" value="rust" />
<div class="zci"><h1 class="zci__heading"><a rel="nofollow" href="https://en.wikipedia.org/wiki/Rust">Rust</a></h1></div>
<div class="result results_links results_links_deep result--ad ">
  <h2 class="result__title"><a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fduckduckgo.com%2Fy.js%3Fad_domain%3Dcourses.example&amp;rut=1">Learn Rust Fast</a></h2>
  <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fduckduckgo.com%2Fy.js%3Fad_domain%3Dcourses.example&amp;rut=1">An <b>ad</b>.</a>
</div>
<div class="result results_links results_links_deep web-result ">
  <div class="links_main links_deep result__body"> <!-- This is the visible part -->
  <h2 class="result__title"><a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust%2Dlang.org%2F&amp;rut=98">Rust Programming Language</a></h2>
  <a class="result__url" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust%2Dlang.org%2F&amp;rut=98">rust-lang.org</a>
  <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust%2Dlang.org%2F&amp;rut=98">A <b>language</b> empowering
     everyone.</a>
  </div>
</div>
<div class="result results_links results_links_deep web-result ">
  <h2 class="result__title"><a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdoc.rust%2Dlang.org%2Fbook%2F&amp;rut=dd">The Rust Book &amp; more</a></h2>
  <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdoc.rust%2Dlang.org%2Fbook%2F&amp;rut=dd">It assumes you&#x27;re using <b>Rust</b> with edition = &quot;2024&quot;.</a>
</div>
<div class="nav-link"><form action="/html/" method="post">
  <input type="submit" class='btn btn--alt' value="Next" />
  <input type="hidden" name="q" value="x class=anomaly-modal id=challenge-form class=result__a href=https://evil.example/ y" />
</form></div>
</body></html>"#;

    /// Its page when nothing is found, for a query that names its markers.
    const NO_RESULTS_PAGE: &str = r#"<html><head><title>"result__a" anomaly-modal at DuckDuckGo</title></head><body>
<input name="q" class="search__input" type="text" value="&quot;result__a&quot; anomaly-modal" />
<div class="result results_links results_links_deep web-result result--no-result">
  <div class="no-results__container result__title"><span class='no-results'>
    <div class="no-results__message"><h1>No results found for <strong>&quot;result__a&quot; anomaly-modal</strong></h1></div>
  </span></div>
</div>
</body></html>"#;

    /// Its challenge to prove a person is searching.
    const CHALLENGE_PAGE: &str = r#"<html><body>
<div class="anomaly-modal__mask"><div class="anomaly-modal__modal" data-testid="anomaly-modal">
<div class="anomaly-modal__title">Unfortunately, bots use DuckDuckGo too.</div>
<form id="challenge-form" action="//duckduckgo.com/anomaly.js?sv=html&amp;q=rust" method="POST"></form>
</div></div>
</body></html>"#;

    /// A stand-in server that answers one request with `status`, any headers
    /// after it, and `body`, and gives back the request.
    fn answer_once(status: &str, body: &'static str) -> (u16, std::thread::JoinHandle<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let status = status.to_owned();
        let asked = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=UTF-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).unwrap();
            String::from_utf8_lossy(&request[..read]).into_owned()
        });
        (port, asked)
    }

    fn duckduckgo_at(port: u16) -> Reach {
        Reach {
            service: service("DuckDuckGo").unwrap(),
            key: String::new(),
            address: format!("http://127.0.0.1:{port}/html"),
        }
    }

    #[test]
    fn fixture_parses_results_ads_entities_and_no_results() {
        // The query in the Next form, though it names the markers, neither
        // adds a result nor reads as a challenge.
        assert_eq!(
            duckduckgo_hits(RESULTS_PAGE, 10),
            Some(vec![
                Hit {
                    title: "Rust Programming Language".into(),
                    url: "https://rust-lang.org/".into(),
                    snippet: "A language empowering everyone.".into(),
                },
                Hit {
                    title: "The Rust Book & more".into(),
                    url: "https://doc.rust-lang.org/book/".into(),
                    snippet: "It assumes you're using Rust with edition = \"2024\".".into(),
                },
            ])
        );
        assert_eq!(
            duckduckgo_hits(RESULTS_PAGE, 1).map(|hits| hits.len()),
            Some(1)
        );
        // Nothing found is an empty list, though the query names the markers.
        assert_eq!(duckduckgo_hits(NO_RESULTS_PAGE, 10), Some(Vec::new()));
        assert_eq!(
            landing("https://example.com/direct"),
            Some("https://example.com/direct".into())
        );
        assert_eq!(
            landing("//duckduckgo.com/l/?uddg=javascript%3Aalert(1)"),
            None
        );
    }

    #[test]
    fn the_anomaly_page_reads_as_limited_not_empty() {
        assert_eq!(duckduckgo_hits(CHALLENGE_PAGE, 10), None);
        // A page of some other kind, with neither results nor the note that
        // there are none, is not taken for nothing found either.
        assert_eq!(
            duckduckgo_hits("<html><body><p>Something new</p></body></html>", 10),
            None
        );
        assert_eq!(duckduckgo_hits("", 10), None);
        // Its challenge comes back as HTTP 202.
        let (port, asked) = answer_once("202 Accepted", CHALLENGE_PAGE);
        let error = ask(&duckduckgo_at(port), "rust", 5, &Options::default()).unwrap_err();
        assert_eq!(error, LIMITED);
        asked.join().unwrap();
        let (port, asked) = answer_once("200 OK", "<html><body></body></html>");
        let error = ask(&duckduckgo_at(port), "rust", 5, &Options::default()).unwrap_err();
        assert_eq!(error, LIMITED);
        asked.join().unwrap();
        // It has no key, so a 401 is a block, never a key refused.
        let (port, asked) = answer_once("401 Unauthorized", "");
        let error = ask(&duckduckgo_at(port), "rust", 5, &Options::default()).unwrap_err();
        assert_eq!(error, LIMITED);
        asked.join().unwrap();
    }

    #[test]
    fn a_duckduckgo_search_is_one_get_that_says_who_is_asking() {
        let (port, asked) = answer_once("200 OK", RESULTS_PAGE);
        let options = Options {
            allowed: vec!["rust-lang.org".into()],
            freshness: Some(Freshness::Week),
            ..Options::default()
        };
        let found = ask(&duckduckgo_at(port), "rust", 5, &options).unwrap();
        assert!(
            found.starts_with("DuckDuckGo results for \"rust\":\n"),
            "{found}"
        );
        assert!(
            found.contains("\n1. Rust Programming Language\n   https://rust-lang.org/\n   A language empowering everyone.\n"),
            "{found}"
        );
        assert!(found.contains("\n2. The Rust Book & more\n"), "{found}");
        let request = asked.join().unwrap().to_lowercase();
        assert!(
            request.starts_with("get /html/?q=rust+site%3arust-lang.org&df=w "),
            "{request}"
        );
        let agent = format!("user-agent: letronna/{}\r\n", env!("CARGO_PKG_VERSION"));
        assert!(request.contains(&agent.to_lowercase()), "{request}");
    }

    #[test]
    fn a_duckduckgo_redirect_is_neither_followed_nor_taken_for_a_limit() {
        // A bang answers with a redirect to the site it names, here one on
        // this machine that would hear the query if it were followed.
        let elsewhere = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        elsewhere.set_nonblocking(true).unwrap();
        let to = elsewhere.local_addr().unwrap().port();
        let (port, asked) = answer_once(
            &format!("303 See Other\r\nLocation: http://127.0.0.1:{to}/wiki?search=rust"),
            "",
        );
        let limited_at = Mutex::new(None);
        let error = unless_held_off(&limited_at, Instant::now(), || {
            ask(&duckduckgo_at(port), "!w rust", 5, &Options::default())
        })
        .unwrap_err();
        asked.join().unwrap();
        assert!(
            error.starts_with("DuckDuckGo answered by sending this search on to 127.0.0.1 "),
            "{error}"
        );
        assert!(error.ends_with("search again without them."), "{error}");
        assert!(
            elsewhere
                .accept()
                .is_err_and(|e| e.kind() == std::io::ErrorKind::WouldBlock),
            "the redirect was followed"
        );
        assert_eq!(*limited_at.lock().unwrap(), None, "held off");

        // One it makes for reasons of its own, to a page of its own or
        // nowhere it says, is not put down to a bang the query lacks.
        for (status, said) in [
            (
                "302 Found\r\nLocation: /html/",
                "search on to 127.0.0.1 rather",
            ),
            ("301 Moved Permanently", "search on rather"),
        ] {
            let (port, asked) = answer_once(status, "");
            let error = unless_held_off(&limited_at, Instant::now(), || {
                ask(&duckduckgo_at(port), "rust", 5, &Options::default())
            })
            .unwrap_err();
            asked.join().unwrap();
            assert!(error.contains(said), "{error}");
            assert!(error.ends_with("search again later."), "{error}");
            assert_eq!(*limited_at.lock().unwrap(), None, "held off");
        }
        assert!(sends_on("\\rust book") && sends_on("rust !w") && !sends_on("rust! book"));
    }

    #[test]
    fn a_chosen_brave_without_a_key_errors_rather_than_using_duckduckgo() {
        let none = HashMap::new();
        let brave = Choice {
            service: "Brave".into(),
            ..Choice::default()
        };
        assert_eq!(
            usable(&brave, &none, true).err().as_deref(),
            Some("Brave has no key saved.")
        );
        let searxng = Choice {
            service: "SearXNG".into(),
            ..Choice::default()
        };
        assert_eq!(
            usable(&searxng, &none, true).err().as_deref(),
            Some("SearXNG has no address saved.")
        );
        assert_eq!(
            status_with("DuckDuckGo", &brave, &none, true),
            ("Not in use", false, true)
        );
    }

    #[test]
    fn duckduckgo_searches_while_nothing_is_chosen_unless_switched_off() {
        let none = HashMap::new();
        let nothing = Choice::default();
        // On, as it is unless switched off: DuckDuckGo, which needs no key.
        let reach = usable(&nothing, &none, true).unwrap();
        assert_eq!(
            (reach.service.name, reach.address.as_str()),
            ("DuckDuckGo", "https://html.duckduckgo.com/html")
        );
        assert_eq!(
            status_with("DuckDuckGo", &nothing, &none, true),
            ("In use", true, true)
        );
        // Switched off, nothing is searched, and the answer says how to
        // switch it back on or add a key.
        let off = usable(&nothing, &none, false).err().unwrap_or_default();
        assert_eq!(off, SWITCHED_OFF);
        for how in [
            "/search keyless",
            r#""keyless_search": true"#,
            "/search services",
        ] {
            assert!(off.contains(how), "{how}: {off}");
        }
        assert_eq!(
            status_with("DuckDuckGo", &nothing, &none, false),
            ("Switched off", false, true)
        );
        // So is it when chosen by name.
        let named = Choice {
            service: "DuckDuckGo".into(),
            ..Choice::default()
        };
        assert_eq!(
            usable(&named, &none, false).err().as_deref(),
            Some(SWITCHED_OFF)
        );
        // A service with a key searches either way.
        let keys = HashMap::from([("Brave".to_owned(), "key".to_owned())]);
        let brave = Choice {
            service: "Brave".into(),
            ..Choice::default()
        };
        assert_eq!(usable(&brave, &keys, false).unwrap().service.name, "Brave");
    }

    #[test]
    fn a_block_fails_fast_for_five_minutes() {
        let limited_at = Mutex::new(None);
        let asked = std::cell::Cell::new(0);
        let limited = || {
            asked.set(asked.get() + 1);
            Err(LIMITED.to_owned())
        };
        let answered = || {
            asked.set(asked.get() + 1);
            Ok("listing".to_owned())
        };
        let start = Instant::now();
        assert_eq!(
            unless_held_off(&limited_at, start, limited),
            Err(LIMITED.to_owned())
        );
        // Nothing is sent for five minutes.
        let soon = start + Duration::from_secs(4 * 60 + 59);
        assert_eq!(
            unless_held_off(&limited_at, soon, answered),
            Err(LIMITED.to_owned())
        );
        assert_eq!(asked.get(), 1);
        let later = start + Duration::from_secs(5 * 60);
        assert_eq!(
            unless_held_off(&limited_at, later, answered),
            Ok("listing".to_owned())
        );
        assert_eq!(asked.get(), 2);
        // Any other failure, such as no connection, is not held against it.
        let down = || Err("Could not reach DuckDuckGo".to_owned());
        assert!(unless_held_off(&limited_at, later, down).is_err());
        assert_eq!(
            unless_held_off(&limited_at, later, answered),
            Ok("listing".to_owned())
        );
        assert_eq!(asked.get(), 3);
    }

    #[test]
    fn searches_side_by_side_go_one_at_a_time_and_none_after_a_limit() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let limited_at = Mutex::new(None);
        let (asking, most, asked) = (
            AtomicUsize::new(0),
            AtomicUsize::new(0),
            AtomicUsize::new(0),
        );
        // Each request takes a moment, so two not kept apart would overlap.
        let ask = |answer: Result<String, String>| {
            asked.fetch_add(1, SeqCst);
            most.fetch_max(asking.fetch_add(1, SeqCst) + 1, SeqCst);
            std::thread::sleep(Duration::from_millis(100));
            asking.fetch_sub(1, SeqCst);
            answer
        };
        let side_by_side = |answer: Result<String, String>| -> Vec<Result<String, String>> {
            std::thread::scope(|scope| {
                let searches: Vec<_> = (0..2)
                    .map(|_| {
                        let answer = answer.clone();
                        scope.spawn(|| unless_held_off(&limited_at, Instant::now(), || ask(answer)))
                    })
                    .collect();
                searches.into_iter().map(|s| s.join().unwrap()).collect()
            })
        };
        let listed = Ok("listing".to_owned());
        assert_eq!(side_by_side(listed.clone()), vec![listed; 2]);
        assert_eq!((asked.load(SeqCst), most.load(SeqCst)), (2, 1));
        // The one that waited is not sent once the first was limited.
        let limited = Err(LIMITED.to_owned());
        assert_eq!(side_by_side(limited.clone()), vec![limited; 2]);
        assert_eq!(asked.load(SeqCst), 3);
    }

    /// Sends one search to DuckDuckGo from this connection.
    #[test]
    #[ignore = "searches the real DuckDuckGo, which may limit this connection"]
    fn searches_the_real_duckduckgo() {
        let service = service("DuckDuckGo").unwrap();
        let reach = Reach {
            service,
            key: String::new(),
            address: service.official.to_owned(),
        };
        let found = ask(&reach, "rust programming language", 3, &Options::default()).unwrap();
        assert!(found.contains("1. "), "{found}");
    }

    /// Needs a SearXNG with the json format on, at the address in
    /// `LETRONNA_SEARXNG`.
    #[test]
    #[ignore = "needs a SearXNG server: set LETRONNA_SEARXNG to its address"]
    fn searches_a_real_searxng() {
        let reach = Reach {
            service: service("SearXNG").unwrap(),
            key: String::new(),
            address: web_address(&std::env::var("LETRONNA_SEARXNG").unwrap(), true),
        };
        let found = search(&reach, "rust programming language", 3, &Options::default()).unwrap();
        assert!(found.contains("1. "), "{found}");
    }

    /// Spends a search of each service whose key is in the environment:
    /// `BRAVE_API_KEY`, `TAVILY_API_KEY`, `EXA_API_KEY`.
    #[test]
    #[ignore = "spends paid searches: set BRAVE_API_KEY, TAVILY_API_KEY or EXA_API_KEY"]
    fn searches_each_paid_service_with_a_key() {
        for (name, var) in [
            ("Brave", "BRAVE_API_KEY"),
            ("Tavily", "TAVILY_API_KEY"),
            ("Exa", "EXA_API_KEY"),
        ] {
            let Ok(key) = std::env::var(var) else {
                continue;
            };
            let service = service(name).unwrap();
            let reach = Reach {
                service,
                key,
                address: service.official.to_owned(),
            };
            let found =
                search(&reach, "rust programming language", 3, &Options::default()).unwrap();
            assert!(found.contains("1. "), "{name}: {found}");
        }
    }
}
