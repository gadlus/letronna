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
//! The tool is only offered once a service is set up, and asks before each
//! search: the query leaves for somebody else's server, and a page read a
//! moment ago may be what is asking for it.

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

/// A search service Letronna can use.
pub struct Service {
    pub name: &'static str,
    pub blurb: &'static str,
    /// Where it answers unless the user says otherwise. Empty for SearXNG,
    /// which is the user's own and needs no key.
    pub official: &'static str,
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
    },
    Service {
        name: "Tavily",
        blurb: "Search made for agents, each result with the text that matters.",
        official: "https://api.tavily.com/search",
    },
    Service {
        name: "Exa",
        blurb: "Search by meaning, with the passages of each page that match.",
        official: "https://api.exa.ai/search",
    },
    Service {
        name: "SearXNG",
        blurb: "Your own metasearch server, at its address. No key, nothing paid.",
        official: "",
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

/// Save which service is used; an empty one turns searching off.
pub fn choose(choice: &Choice) -> Result<(), String> {
    let _ = std::fs::create_dir_all(crate::chat::data_dir());
    let bytes = serde_json::to_vec_pretty(choice).map_err(|e| e.to_string())?;
    std::fs::write(choice_path(), bytes).map_err(|e| format!("Cannot save the search service: {e}"))
}

fn choice_path() -> std::path::PathBuf {
    crate::chat::data_dir().join("search.json")
}

/// Whether a search service is set up, so the tool is offered.
pub fn ready() -> bool {
    // Asked on every tool lookup -- several for each call, and one each
    // frame the window's approval card is up -- so the files are only read
    // again once one has changed: the terminal and the window each save
    // them, and each has to see the other's change.
    static SEEN: Mutex<Option<([Stamp; 2], bool)>> = Mutex::new(None);
    remembered(&SEEN, [&choice_path(), &crate::store::keys_path()], || {
        usable(&choice(), &crate::store::load_keys()).is_ok()
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

/// The answer kept in `seen` while both files are as they were when it was
/// read, or a fresh one from `read` once either has changed.
fn remembered(
    seen: &Mutex<Option<([Stamp; 2], bool)>>,
    files: [&Path; 2],
    read: impl FnOnce() -> bool,
) -> bool {
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
    let set_up = match service(name) {
        Some(s) if s.self_hosted() => !choice.address(s).is_empty(),
        Some(_) => keys.get(name).is_some_and(|key| !key.trim().is_empty()),
        None => false,
    };
    match (set_up, choice.service == name) {
        (true, true) => ("In use", true, true),
        (true, false) => ("Saved, not in use", false, true),
        (false, _) => ("Not set up", false, false),
    }
}

pub fn service(name: &str) -> Option<&'static Service> {
    SERVICES.iter().find(|s| s.name == name)
}

/// What searching needs: the service, its key (none for SearXNG) and the
/// address to reach it at.
struct Reach {
    service: &'static Service,
    key: String,
    address: String,
}

/// The service to search with, and how to reach it.
fn usable(choice: &Choice, keys: &HashMap<String, String>) -> Result<Reach, String> {
    let Some(service) = service(&choice.service) else {
        return Err(
            "No search service is set up: add one in Settings, Search, or with /search services."
                .into(),
        );
    };
    let key = keys
        .get(service.name)
        .map(|key| key.trim().to_owned())
        .unwrap_or_default();
    if !service.self_hosted() && key.is_empty() {
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
        "Search the web with the user's search service. Returns a numbered list of results, each with its title, address and a snippet; read a page in full with web_fetch. allowed_domains keeps only results from those sites and blocked_domains drops them, subdomains included; a site in both is blocked. Tavily and Exa narrow the search itself, while Brave and SearXNG are filtered here after they answer, so fewer results may come back. freshness keeps to pages from the last day, week, month or year. The same search again within 10 minutes gives the earlier answer."
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
        let reach = usable(&choice(), &crate::store::load_keys())?;
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
        // Tavily and Exa are sent the sites too, so the card and the judge
        // see every word that leaves, not just the query. Filters that do
        // not parse stop the call before anything is sent.
        let filters = options(args)
            .map(|options| filters(&options))
            .unwrap_or_default();
        let search = match usable(&choice(), &crate::store::load_keys()) {
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

/// What the services asked with a GET are sent: Brave and SearXNG.
fn parameters(
    service: &str,
    query: &str,
    count: u64,
    options: &Options,
) -> Vec<(&'static str, String)> {
    let mut sent = vec![("q", query.to_owned())];
    if service == "Brave" {
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

/// Ask the service for `query`, and list what it found that the filters keep.
fn search(reach: &Reach, query: &str, count: u64, options: &Options) -> Result<String, String> {
    let Reach {
        service,
        key,
        address,
    } = reach;
    let client = reqwest::blocking::Client::builder()
        .timeout(SEARCH_TIME)
        .user_agent(concat!("Letronna/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
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
    let text = reply.text().unwrap_or_default();
    if !status.is_success() {
        return Err(refusal(service, status.as_u16(), &text));
    }
    let body: Value = serde_json::from_str(&text).map_err(|_| {
        format!(
            "{} answered with something other than search results.",
            service.name
        )
    })?;
    // Every answer goes through the filters, a service that narrowed by
    // site too: it may not count subdomains the same way, and only Tavily
    // takes both lists at once.
    let wider = match options.by_site() && !narrows_by_site(service.name) {
        true => usize::MAX,
        false => count as usize,
    };
    let hits: Vec<Hit> = hits(service.name, &body, wider)
        .into_iter()
        .filter(|hit| wanted(&hit.url, options))
        .take(count as usize)
        .collect();
    Ok(listing(service.name, query, &hits))
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
        let reach = usable(&brave, &keys).unwrap();
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
        assert!(usable(&tavily, &keys).is_err(), "chosen, but no key");
        assert_eq!(
            status("Brave", &tavily, &keys),
            ("Saved, not in use", false, true)
        );

        let mut searxng = Choice {
            service: "SearXNG".into(),
            ..Choice::default()
        };
        assert!(usable(&searxng, &keys).is_err(), "no address yet");
        searxng.set_address(service("SearXNG").unwrap(), "mini:8888");
        assert_eq!(usable(&searxng, &keys).unwrap().address, "http://mini:8888");
        // Its address is kept while another service is used.
        let back_to_brave = Choice {
            service: "Brave".into(),
            ..searxng
        };
        assert_eq!(
            status("SearXNG", &back_to_brave, &keys),
            ("Saved, not in use", false, true)
        );
        assert!(usable(&Choice::default(), &keys).is_err(), "nothing chosen");
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
