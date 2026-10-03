//! Reading a web page.
//!
//! The gateway's web search (the Web toggle) finds pages, but only some
//! gateways offer it, and nothing could read a page the user pasted. This
//! fetches one address and hands the model its text: HTML is reduced to the
//! words a reader sees, written as markdown so its links, headings and code
//! survive; text and JSON pass through as they are; a PDF is saved as a file,
//! as nothing here reads one; anything else is refused.
//!
//! It asks before every fetch unless the user allows it for the session. An
//! address is also a way out, and a page read a moment ago may be what is
//! asking for it.
//!
//! The approval shows only the first address, so every redirect gets the
//! browser's check too, and none may lead from elsewhere onto this machine
//! or the local network. The check reads the address, not what its name
//! resolves to: a name pointed at a local address still gets through.
//!
//! The HTML reduction is a small scanner rather than a parser: the model
//! reads the words, not the layout, and a whole HTML engine is a lot of
//! binary for that. Links are kept, made absolute, since a page read without
//! them is a dead end: the model can neither follow nor cite what it read.
//!
//! A page read in the last 15 minutes is given again from memory, as a task
//! often reads the same page twice, unless the model asks for it fresh. Pages
//! on this machine or the local network are always fetched again: they are
//! the ones being changed while they are read.

use crate::tools::{Tool, ToolContext};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::Read;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The most of a page that is read, in bytes. What the model is shown is cut
/// much shorter than this; the rest is only there to find the text in.
const MAX_BYTES: u64 = 2 * 1024 * 1024;
/// The largest PDF saved, in bytes. Papers, manuals and data sheets are a few
/// megabytes; this leaves room for a long scanned one while a fetch the model
/// makes cannot fill the disk.
const MAX_PDF_BYTES: u64 = 20 * 1024 * 1024;
/// The longest link address kept. Past this it is nearly always tracking, and
/// it would take the room of a paragraph of the page.
const MAX_LINK: usize = 200;
/// How long a page read is given again from memory: a task that reads a page
/// twice mostly does so within minutes, and a page that changes seldom does
/// so by the minute.
const MEMO_TIME: Duration = Duration::from_secs(15 * 60);
/// The most pages remembered at once, looked through one by one.
const MEMO_PAGES: usize = 64;
/// The most text remembered at once, in bytes: four of the largest pages that
/// are read, or hundreds of ordinary ones.
const MEMO_BYTES: usize = 8 * 1024 * 1024;

/// How long a fetch may take, redirects and all.
const FETCH_TIME: Duration = Duration::from_secs(20);

/// Elements that hold nothing a reader sees on the page. Not `head`: its
/// closing tag may be left out, which would hide the page along with it.
const HIDDEN: &[&str] = &[
    "script", "style", "noscript", "svg", "template", "title", "iframe", "object",
];
/// Elements that start on a line of their own.
const BLOCKS: &[&str] = &[
    "p",
    "div",
    "br",
    "li",
    "ul",
    "ol",
    "tr",
    "table",
    "section",
    "article",
    "header",
    "footer",
    "nav",
    "main",
    "aside",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "pre",
    "blockquote",
    "hr",
    "dt",
    "dd",
    "figure",
    "form",
];

/// The model's way to read a page.
pub struct WebFetch;

impl Tool for WebFetch {
    fn hides_key_shapes(&self) -> bool {
        true
    }
    fn name(&self) -> &'static str {
        "web_fetch"
    }
    fn description(&self) -> &'static str {
        "Fetch a web page (http or https) and return its text: HTML is reduced to readable markdown with its links as full addresses, plain text and JSON come back as they are, and a PDF is saved as a file. A page read in the last 15 minutes is given again from memory unless `fresh` is true. Use it to read a page the user mentions, or one a search turned up."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "url": {"type": "string", "description": "The full address, starting with https:// or http://."},
            "fresh": {"type": "boolean", "description": "Fetch it again even if it was read in the last 15 minutes, for a page that has changed since."}
        }, "required": ["url"]})
    }
    fn run(&self, args: &Value, _: &ToolContext) -> Result<String, String> {
        let page = fetch(
            args["url"].as_str().unwrap_or_default(),
            &crate::redact::secrets(),
            args["fresh"].as_bool() == Some(true),
        )?;
        match page {
            Page::Text(text) => Ok(text),
            // Letronna's own media folder rather than the project's, as for a
            // browse screenshot: reading a page should not change the project.
            Page::Pdf { address, bytes } => save_pdf(&ToolContext::default(), &address, &bytes),
        }
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
        format!("GET {}", args["url"].as_str().unwrap_or_default())
    }
}

/// What a fetch brought back.
#[derive(Debug, PartialEq)]
enum Page {
    /// Text for the model, after the address it was read from.
    Text(String),
    /// A PDF, to be saved rather than read.
    Pdf { address: String, bytes: Vec<u8> },
}

/// The page at `raw`, after its final address, unless it or a redirect on
/// the way carries one of `secrets`; from memory when read lately, unless
/// `fresh`.
fn fetch(raw: &str, secrets: &[String], fresh: bool) -> Result<Page, String> {
    let url =
        reqwest::Url::parse(raw.trim()).map_err(|_| format!("`{raw}` is not a web address."))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("Only http and https pages can be fetched.".into());
    }
    // As written, since parsing lowercases the host and a key can be sent out
    // as part of a name, and as parsed, since that is what is fetched and the
    // browser reads `http:/10.0.0.1` as a name.
    crate::browser::address(raw, secrets)?;
    crate::browser::address(url.as_str(), secrets)?;
    // Looked up only once the address has passed, so a key added since a page
    // was read still stops it.
    let asked = memo_key(&url);
    let remember = !local(&url);
    if remember && !fresh {
        let memo = MEMO.lock().ok();
        if let Some(text) = memo.and_then(|mut memo| memo.get(&asked, Instant::now())) {
            return Ok(Page::Text(text));
        }
    }
    // A redirect arrives parsed, so a key is looked for in lowercase too.
    let secrets: Vec<String> = secrets
        .iter()
        .flat_map(|secret| [secret.clone(), secret.to_lowercase()])
        .collect();
    // Redirects are followed here rather than by a policy on the client,
    // which could not see this fetch's keys without a client of its own.
    // A request's own timeout is a deadline for its whole reply, where the
    // client's bounds only each wait on it, so a page trickled out a byte at
    // a time still ends when the deadline for every hop and the page does.
    let client = client()?;
    let deadline = Instant::now() + FETCH_TIME;
    let mut url = url;
    let mut previous = Vec::new();
    let response = loop {
        let mut request = client
            .get(url.clone())
            .timeout(deadline.saturating_duration_since(Instant::now()));
        if let Some(from) = previous.last().and_then(|from| referer(from, &url)) {
            request = request.header(reqwest::header::REFERER, from);
        }
        let response = request
            .send()
            .map_err(|e| format!("Could not fetch the page: {e}"))?;
        let Some(next) = moved_to(&response) else {
            break response;
        };
        // Before `follow`, whose address check would refuse it first in
        // words written for the browser.
        if !matches!(next.scheme(), "http" | "https") {
            return Err("The page redirected somewhere that is not a web page.".into());
        }
        previous.push(url);
        follow(&previous, &next, &secrets)?;
        url = next;
    };
    let status = response.status();
    if !status.is_success() {
        return Err(format!("The page answered HTTP {}.", status.as_u16()));
    }
    let reached = response.url().clone();
    let address = reached.to_string();
    let kind = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if kind.contains("pdf") {
        return pdf(response, address);
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("The page stopped arriving: {e}"))?;
    let mut body = String::from_utf8_lossy(&bytes).into_owned();
    if bytes.len() as u64 == MAX_BYTES {
        // The limit may have cut a key short, which can be told only while
        // the cut is still the end.
        body = crate::redact::scrub_cut(&body, true);
    }
    let text = if kind.contains("html") {
        match title_of(&body) {
            Some(title) => format!("{title}\n\n{}", text_of(&body, &reached)),
            None => text_of(&body, &reached),
        }
    } else if kind.is_empty()
        || kind.starts_with("text/")
        || kind.contains("json")
        || kind.contains("xml")
    {
        body
    } else {
        return Err(format!("That is not a page of text ({kind})."));
    };
    let text = format!("{address}\n\n{text}");
    if remember && let Ok(mut memo) = MEMO.lock() {
        memo.put([asked, memo_key(&reached)], text.clone(), Instant::now());
    }
    Ok(Page::Text(text))
}

/// The PDF `response` carries, unless it is larger than `MAX_PDF_BYTES`.
fn pdf(response: reqwest::blocking::Response, address: String) -> Result<Page, String> {
    let too_big = || {
        format!(
            "The PDF is larger than {} MB, which is more than is saved.",
            MAX_PDF_BYTES / (1024 * 1024)
        )
    };
    // Said up front by most servers, which spares reading the whole of it.
    if response.content_length().is_some_and(|n| n > MAX_PDF_BYTES) {
        return Err(too_big());
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_PDF_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("The PDF stopped arriving: {e}"))?;
    if bytes.len() as u64 > MAX_PDF_BYTES {
        return Err(too_big());
    }
    Ok(Page::Pdf { address, bytes })
}

/// Saves a PDF read from `address` under `ctx`'s media folder, saying where
/// and what it is.
fn save_pdf(ctx: &ToolContext, address: &str, bytes: &[u8]) -> Result<String, String> {
    // Named for the file the address ends in, else for the site.
    let url = reqwest::Url::parse(address).ok();
    let file = url
        .as_ref()
        .and_then(|url| url.path_segments()?.rev().find(|part| !part.is_empty()))
        .map(|part| part.trim_end_matches(".pdf"));
    let about = file
        .or_else(|| url.as_ref().and_then(|url| url.host_str()))
        .unwrap_or("page");
    let path = crate::media::save(ctx, about, "pdf", bytes)?;
    let pages = match pages(bytes) {
        Some(1) => "1 page, ".to_owned(),
        Some(n) => format!("{n} pages, "),
        None => String::new(),
    };
    Ok(format!(
        "{}{}\nA PDF of {pages}{}, from {address}. Its text is not read here.",
        crate::media::SAVED,
        path.display(),
        crate::media::size(bytes.len())
    ))
}

/// How many pages a PDF has, by the count at the root of its page tree, when
/// that is written out plainly rather than packed into a compressed stream.
fn pages(pdf: &[u8]) -> Option<usize> {
    // One pass, object by object, so a file made to be slow to read is not.
    let (mut tree, mut count, mut most) = (false, None, None);
    let mut at = 0;
    while at < pdf.len() {
        let rest = &pdf[at..];
        if rest.starts_with(b"endobj") {
            // Every node of the tree counts the pages below it, the root all.
            if tree {
                most = most.max(count);
            }
            (tree, count) = (false, None);
            at += b"endobj".len();
        } else if rest.starts_with(b"obj") {
            (tree, count) = (false, None);
            at += b"obj".len();
        } else if let Some(after) = rest.strip_prefix(b"/Type") {
            let kind = after.trim_ascii_start().strip_prefix(b"/Pages".as_slice());
            tree |= kind.is_some_and(|end| !end.first().is_some_and(u8::is_ascii_alphanumeric));
            at += b"/Type".len();
        } else if let Some(after) = rest.strip_prefix(b"/Count") {
            let digits = after.trim_ascii_start();
            let digits = &digits[..digits.iter().take_while(|b| b.is_ascii_digit()).count()];
            count = std::str::from_utf8(digits)
                .ok()
                .and_then(|n| n.parse().ok());
            at += b"/Count".len();
        } else {
            at += 1;
        }
    }
    most.filter(|&n: &usize| n > 0)
}

/// Pages read lately, oldest first, each under the address asked for and
/// the one it ended at.
struct Memo(VecDeque<Remembered>);

struct Remembered {
    read: Instant,
    addresses: [String; 2],
    text: String,
}

static MEMO: Mutex<Memo> = Mutex::new(Memo(VecDeque::new()));

impl Memo {
    /// The text read from `address` within `MEMO_TIME` of `now`.
    fn get(&mut self, address: &str, now: Instant) -> Option<String> {
        self.0
            .retain(|page| now.duration_since(page.read) < MEMO_TIME);
        let page = self
            .0
            .iter()
            .find(|page| page.addresses.iter().any(|a| a == address))?;
        Some(page.text.clone())
    }

    /// Remembers `text` as read from both `addresses`, in place of what was
    /// read from either before, forgetting the oldest pages to make room.
    fn put(&mut self, addresses: [String; 2], text: String, now: Instant) {
        self.0
            .retain(|page| !page.addresses.iter().any(|a| addresses.contains(a)));
        let mut bytes: usize = self.0.iter().map(|page| page.text.len()).sum();
        while self.0.len() >= MEMO_PAGES || bytes + text.len() > MEMO_BYTES {
            let Some(oldest) = self.0.pop_front() else {
                // Larger than all the room there is: not remembered.
                return;
            };
            bytes -= oldest.text.len();
        }
        self.0.push_back(Remembered {
            read: now,
            addresses,
            text,
        });
    }
}

/// `url` as the memo knows it: without the part after `#`, which is never
/// sent and names a place on the same page.
fn memo_key(url: &reqwest::Url) -> String {
    let mut url = url.clone();
    url.set_fragment(None);
    url.into()
}

/// The one HTTP client every fetch goes through, kept open between fetches.
/// It follows no redirect itself: `fetch` checks each one first.
fn client() -> Result<&'static reqwest::blocking::Client, String> {
    static CLIENT: OnceLock<Result<reqwest::blocking::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::blocking::Client::builder()
                .timeout(FETCH_TIME)
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(concat!("Letronna/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Where `response` sends the fetch on to, when it is a redirect reqwest
/// would have followed.
fn moved_to(response: &reqwest::blocking::Response) -> Option<reqwest::Url> {
    if !matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
        return None;
    }
    let to = response.headers().get(reqwest::header::LOCATION)?;
    response
        .url()
        .join(std::str::from_utf8(to.as_bytes()).ok()?)
        .ok()
}

/// The Referer a redirect from `from` on to `next` carries, as reqwest sent
/// it: none from https down to http, and never a password.
fn referer(from: &reqwest::Url, next: &reqwest::Url) -> Option<String> {
    if next.scheme() == "http" && from.scheme() == "https" {
        return None;
    }
    let mut from = from.clone();
    let _ = from.set_username("");
    let _ = from.set_password(None);
    from.set_fragment(None);
    Some(from.into())
}

/// Whether a redirect from the addresses in `previous` on to `next` is
/// followed, or why not.
fn follow(
    previous: &[reqwest::Url],
    next: &reqwest::Url,
    secrets: &[String],
) -> Result<(), String> {
    // `previous` starts with the address asked for, so this is five redirects.
    if previous.len() > 5 {
        return Err("The page redirected more than five times.".into());
    }
    crate::browser::address(next.as_str(), secrets)
        .map_err(|why| format!("The page redirected somewhere that is not fetched. {why}"))?;
    if local(next) && !previous.iter().all(local) {
        return Err(
            "The page redirected to this machine or the local network, which is fetched only \
             when asked for directly."
                .into(),
        );
    }
    Ok(())
}

/// Whether `url` names this machine or the local network by its host.
pub(crate) fn local(url: &reqwest::Url) -> bool {
    let host = url.host_str().unwrap_or_default();
    let ip = host.trim_matches(['[', ']']).parse::<std::net::IpAddr>();
    match ip.map(|ip| ip.to_canonical()) {
        Ok(std::net::IpAddr::V4(ip)) => {
            // Tailscale hands out 100.64.0.0/10, and a tailnet is as private
            // as a LAN.
            let shared = ip.octets()[0] == 100 && (ip.octets()[1] & 0xc0) == 64;
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_unspecified()
                || shared
        }
        Ok(std::net::IpAddr::V6(ip)) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
        }
        Err(_) => {
            // No public name lacks a dot, so one without is found on the local
            // network, and these endings are kept for private networks.
            let name = host.trim_end_matches('.');
            let zones = [
                ".localhost",
                ".local",
                ".home.arpa",
                ".internal",
                ".lan",
                ".ts.net",
            ];
            !name.contains('.') || zones.iter().any(|zone| name.ends_with(zone))
        }
    }
}

/// The page's title, when it has one.
fn title_of(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let open = start + lower[start..].find('>')? + 1;
    let end = open + lower[open..].find("</title")?;
    Some(tidy(&decode(&html[open..end]))).filter(|title| !title.is_empty())
}

/// An HTML page as markdown: a line per block, and its links, headings and
/// code, a link made absolute against `base`, where the page was read.
fn text_of(html: &str, base: &reqwest::Url) -> String {
    // Lowercasing only ASCII keeps every byte where it was, so positions
    // found in one string cut the other.
    let lower = html.to_ascii_lowercase();
    let mut page = Markdown::new(base);
    let mut at = 0;
    while let Some(open) = html[at..].find('<').map(|i| at + i) {
        page.text(&decode(&html[at..open]));
        if lower[open..].starts_with("<!--") {
            at = lower[open..]
                .find("-->")
                .map_or(html.len(), |i| open + i + 3);
            continue;
        }
        let Some(close) = html[open..].find('>').map(|i| open + i) else {
            at = html.len();
            break;
        };
        let tag = &lower[open + 1..close];
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect();
        at = close + 1;
        if !closing && HIDDEN.contains(&name.as_str()) {
            // Everything up to its closing tag goes too, which is what keeps
            // a `<` inside a script from being read as the start of a tag.
            at = lower[at..]
                .find(&format!("</{name}"))
                .map_or(html.len(), |i| at + i);
            at = html[at..].find('>').map_or(html.len(), |i| at + i + 1);
            continue;
        }
        page.tag(&name, closing, &html[open + 1..close]);
    }
    page.text(&decode(&html[at..]));
    page.finish()
}

/// A page being written out as markdown by `text_of`. A link, heading or
/// piece of code is rewritten in place once it closes, from where its words
/// start in the prose.
struct Markdown<'a> {
    base: &'a reqwest::Url,
    /// What is finished: tidied prose and code blocks, in order.
    done: Vec<String>,
    /// The prose since the last code block.
    out: String,
    /// The open link: where its words start, and where it leads if kept.
    link: Option<(usize, Option<String>)>,
    /// The open heading: where its words start, and its level.
    heading: Option<(usize, usize)>,
    /// Where the open piece of code within a line starts.
    code: Option<usize>,
    /// The code block being read, its spacing kept.
    pre: Option<String>,
}

impl<'a> Markdown<'a> {
    fn new(base: &'a reqwest::Url) -> Self {
        Markdown {
            base,
            done: Vec::new(),
            out: String::new(),
            link: None,
            heading: None,
            code: None,
            pre: None,
        }
    }

    /// Adds words from between tags, entities already decoded.
    fn text(&mut self, text: &str) {
        match &mut self.pre {
            Some(code) => code.push_str(text),
            None => self.out.push_str(text),
        }
    }

    /// Takes in a tag named `name`, written `raw` between its `<` and `>`.
    fn tag(&mut self, name: &str, closing: bool, raw: &str) {
        if let Some(code) = &mut self.pre {
            // Inside a code block only its end and its line breaks count.
            match name {
                "pre" if closing => self.close_pre(),
                "br" => code.push('\n'),
                _ => {}
            }
            return;
        }
        let block = BLOCKS.contains(&name);
        if block && !closing {
            self.out.push('\n');
            if name == "li" {
                self.out.push_str("- ");
            }
        }
        let level = name
            .strip_prefix('h')
            .and_then(|n| n.parse::<usize>().ok())
            .filter(|n| (1..=6).contains(n));
        match (name, closing, level) {
            ("a", false, _) => {
                // A link left open ends where the next one starts.
                self.close_link();
                let target = attribute(raw, "href").and_then(|href| self.target(&href));
                self.link = Some((self.out.len(), target));
            }
            ("a", true, _) => self.close_link(),
            ("code", false, _) if self.code.is_none() => self.code = Some(self.out.len()),
            ("code", true, _) => self.close_code(),
            ("pre", false, _) => {
                self.flush();
                self.pre = Some(String::new());
            }
            // A heading inside a link, as a card of a list of posts has, is
            // left as words: the link is what it is there for.
            (_, false, Some(level)) if self.link.is_none() => {
                self.close_heading();
                self.heading = Some((self.out.len(), level));
            }
            (_, true, Some(_)) => self.close_heading(),
            _ => {}
        }
        if block && closing {
            self.out.push('\n');
        }
    }

    /// Where `href` leads as a full address, when that is a web page other
    /// than this one and short enough to be worth its room.
    fn target(&self, href: &str) -> Option<String> {
        let url = self.base.join(href.trim()).ok()?;
        // Not `javascript:`, `data:` or `mailto:`: nothing the fetch reads.
        if !matches!(url.scheme(), "http" | "https") {
            return None;
        }
        // A place on this same page, such as a table of contents has.
        if memo_key(&url) == memo_key(self.base) {
            return None;
        }
        Some(String::from(url)).filter(|url| url.len() <= MAX_LINK)
    }

    /// Ends the open link, written `[words](address)` when it leads anywhere
    /// kept, as the address alone when that is all its words say, and left as
    /// its words when not.
    fn close_link(&mut self) {
        let Some((start, target)) = self.link.take() else {
            return;
        };
        if self.code.is_some_and(|at| at >= start) {
            self.close_code();
        }
        let Some(url) = target else {
            return;
        };
        self.rewrite(start, |words| {
            let bare = url.split_once("://").map_or(url.as_str(), |(_, rest)| rest);
            if [url.as_str(), bare, bare.trim_end_matches('/')].contains(&words) {
                url.clone()
            } else {
                format!("[{words}]({url})")
            }
        });
    }

    /// Ends the open heading, written after as many `#` as its level.
    fn close_heading(&mut self) {
        let Some((start, level)) = self.heading.take() else {
            return;
        };
        if self.link.as_ref().is_some_and(|(at, _)| *at >= start) {
            self.close_link();
        }
        if self.code.is_some_and(|at| at >= start) {
            self.close_code();
        }
        self.rewrite(start, |words| format!("{} {words}", "#".repeat(level)));
    }

    /// Ends the open piece of code within a line, written between backticks.
    fn close_code(&mut self) {
        let Some(start) = self.code.take() else {
            return;
        };
        self.rewrite(start, |words| {
            let ticks = "`".repeat(backticks(words) + 1);
            format!("{ticks}{words}{ticks}")
        });
    }

    /// Ends the open code block, written fenced with its spacing as it was.
    fn close_pre(&mut self) {
        let Some(code) = self.pre.take() else {
            return;
        };
        let code = code.replace("\r\n", "\n");
        // A line break right after `<pre>` is not part of what it holds.
        let code = code.strip_prefix('\n').unwrap_or(&code).trim_end();
        if code.trim().is_empty() {
            return;
        }
        let fence = "`".repeat((backticks(code) + 1).max(3));
        self.done.push(format!("{fence}\n{code}\n{fence}"));
    }

    /// Puts what was written since `start` on one line, as `made` writes it;
    /// nothing when it has no words. Anything opened within it starts again
    /// after it, which is as much as markup opened and closed out of order
    /// gets.
    fn rewrite(&mut self, start: usize, made: impl FnOnce(&str) -> String) {
        let region = &self.out[start..];
        let words = region.split_whitespace().collect::<Vec<_>>().join(" ");
        // The spaces it stood between still part it from the words around.
        let before = if region.starts_with(char::is_whitespace) {
            " "
        } else {
            ""
        };
        let after = if region.ends_with(char::is_whitespace) {
            " "
        } else {
            ""
        };
        let written = if words.is_empty() {
            before.to_owned()
        } else {
            format!("{before}{}{after}", made(&words))
        };
        self.out.truncate(start);
        self.out.push_str(&written);
        let end = self.out.len();
        let link = self.link.as_mut().map(|(at, _)| at);
        let heading = self.heading.as_mut().map(|(at, _)| at);
        for at in [link, heading, self.code.as_mut()].into_iter().flatten() {
            if *at > start {
                *at = end;
            }
        }
    }

    /// Ends whatever is open and sets the prose so far aside, tidied.
    fn flush(&mut self) {
        self.close_code();
        self.close_link();
        self.close_heading();
        let prose = tidy(&self.out);
        self.out.clear();
        if !prose.is_empty() {
            self.done.push(prose);
        }
    }

    /// The page written out, whatever was left open ended with it.
    fn finish(mut self) -> String {
        self.close_pre();
        self.flush();
        self.done.join("\n")
    }
}

/// The value of the attribute `name` in the tag `raw`, entities decoded.
fn attribute(raw: &str, name: &str) -> Option<String> {
    let lower = raw.to_ascii_lowercase();
    let mut at = 0;
    while let Some(found) = lower[at..].find(name).map(|i| at + i) {
        at = found + name.len();
        // Not the end of another name, such as `data-href`.
        if !lower[..found].ends_with(|c: char| c.is_ascii_whitespace()) {
            continue;
        }
        let Some(value) = raw[at..].trim_start().strip_prefix('=') else {
            continue;
        };
        let value = value.trim_start();
        let value = match value.chars().next() {
            Some(quote @ ('"' | '\'')) => value[1..].split(quote).next(),
            _ => value.split(|c: char| c.is_ascii_whitespace()).next(),
        };
        return Some(decode(value.unwrap_or_default()));
    }
    None
}

/// The longest run of backticks in `text`, which a fence around it must
/// outdo.
fn backticks(text: &str) -> usize {
    text.split(|c| c != '`').map(str::len).max().unwrap_or(0)
}

/// Entities written out as the characters they stand for; anything that is
/// not a known one is left as it was written.
fn decode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        // An entity is short; a `;` further on belongs to something else.
        let semi = rest.bytes().take(12).position(|b| b == b';');
        let decoded = semi.and_then(|semi| {
            let entity = &rest[1..semi];
            let numeric = || {
                let code = match entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => entity.strip_prefix('#')?.parse().ok(),
                };
                code.and_then(char::from_u32)
            };
            let c = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => numeric(),
            };
            c.map(|c| (c, semi))
        });
        match decoded {
            Some((c, semi)) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// One space between words and no empty lines: the model reads the words,
/// and every blank line and run of indentation is tokens spent on nothing.
fn tidy(text: &str) -> String {
    text.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    const PAGE: &str = r#"<!DOCTYPE html>
<html><head><title>Rust &amp; Letronna</title>
<style>body { color: red; }</style>
<script>if (a < b) { document.write("<p>hidden</p>"); }</script></head>
<body>
  <!-- a comment <p>not shown</p> -->
  <h1>Getting   started</h1>
  <p>Install with <code>cargo</code>&nbsp;and run it.</p>
  <ul><li>fast</li><li>small &#8212; native</li></ul>
  <p>Caf&eacute; &#x263A; &copy x</p>
</body></html>"#;

    #[test]
    fn a_page_is_reduced_to_what_a_reader_sees() {
        let text = text_of(PAGE, &reqwest::Url::parse("https://example.com/").unwrap());
        assert_eq!(
            text,
            "# Getting started\nInstall with `cargo` and run it.\n- fast\n- small — native\nCaf&eacute; ☺ &copy x"
        );
        assert!(!text.contains("hidden") && !text.contains("color"));
        assert_eq!(title_of(PAGE).as_deref(), Some("Rust & Letronna"));
    }

    #[test]
    fn links_headings_and_code_are_kept_as_markdown() {
        let long = format!("/track?{}", "x".repeat(MAX_LINK));
        let page = format!(
            r##"<h2>Install <a href="#install">&#182;</a></h2>
<p>Read <a href="../api/index.html">the <b>API</b>
   reference</a> or <a href='https://other.example/x?a=1&amp;b=2'>another site</a>,
<a href="https://other.example">other.example</a>.</p>
<p>Not <a href="javascript:alert(1)">a script</a>, <a href="data:text/html,hi">data</a>,
<a href="mailto:someone@example.com">mail</a>, <a href="{long}">tracking</a>, <a>nowhere</a>
or <a data-href="/x" href=/top#part>the top</a>.</p>
<pre><code class="language-rust">fn main() {{
    println!("&lt;hi&gt; ```");
}}
</code></pre>
<p>Then run <code>cargo run</code>.<h3></h3></p>
<a href="/post"><h3>A post</h3><p>What it says</p></a>"##
        );
        let base = reqwest::Url::parse("https://example.com/docs/guide/#intro").unwrap();
        let fence = "````";
        assert_eq!(
            text_of(&page, &base),
            [
                "## Install ¶",
                "Read [the API reference](https://example.com/docs/api/index.html) or \
                 [another site](https://other.example/x?a=1&b=2),",
                "https://other.example/.",
                "Not a script, data,",
                "mail, tracking, nowhere",
                "or [the top](https://example.com/top#part).",
                fence,
                "fn main() {",
                "    println!(\"<hi> ```\");",
                "}",
                fence,
                "Then run `cargo run`.",
                "[A post What it says](https://example.com/post)",
            ]
            .join("\n")
        );
    }

    #[test]
    fn markup_left_open_or_out_of_order_keeps_its_words() {
        let base = reqwest::Url::parse("https://example.com/").unwrap();
        let page = "<p><a href=/a>one <code>two</a> three</code> <a href=/b>four <a href=/c>five</a>\
                    </p><h1>six <a href=/d>seven</h1> eight<pre>  nine";
        assert_eq!(
            text_of(page, &base),
            "[one `two`](https://example.com/a) three [four](https://example.com/b) \
             [five](https://example.com/c)\n# six [seven](https://example.com/d)\neight\n\
             ```\n  nine\n```"
        );
    }

    #[test]
    fn only_web_addresses_are_fetched() {
        assert!(fetch("file:///C:/Windows/win.ini", &[], false).is_err());
        assert!(fetch("not an address", &[], false).is_err());
    }

    #[test]
    fn an_address_the_browser_would_refuse_is_not_fetched() {
        let why = fetch("http://169.254.169.254/latest/meta-data/", &[], false).unwrap_err();
        assert!(why.contains("metadata"), "{why}");
        // One slash short still fetches the same address.
        let why = fetch("http:/169.254.169.254/latest/meta-data/", &[], false).unwrap_err();
        assert!(why.contains("metadata"), "{why}");
        // Sent out as a name, a key keeps its capitals only as written.
        let key = "Sk-Live-0123456789ABCDEF".to_owned();
        let why = fetch(&format!("https://{key}.evil.example/"), &[key], false).unwrap_err();
        assert!(why.contains("keys or tokens"), "{why}");
    }

    #[test]
    fn a_redirect_may_not_lead_from_elsewhere_onto_this_machine_or_network() {
        let url = |url: &str| reqwest::Url::parse(url).unwrap();
        let away = [url("https://example.com/")];
        for inward in [
            "http://127.0.0.1:8080/",
            "http://2130706433/",
            "http://localhost/",
            "http://app.localhost./",
            "http://0.0.0.0/",
            "http://192.168.1.1/admin",
            "http://10.0.0.5/",
            "http://100.101.102.103/",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://[fd12:3456::1]/",
            "http://router/",
            "http://nas.local/",
        ] {
            let why = follow(&away, &url(inward), &[]).unwrap_err();
            assert!(why.contains("local network"), "{inward}: {why}");
        }
        assert!(follow(&away, &url("https://www.example.com/"), &[]).is_ok());
        assert!(follow(&away, &url("http://100.128.0.1/"), &[]).is_ok());
        // Asked for directly, a local page may move about on this machine,
        // but not by way of somewhere else.
        let here = [url("http://127.0.0.1:3000/")];
        assert!(follow(&here, &url("http://localhost:3000/app"), &[]).is_ok());
        let via = [url("http://127.0.0.1:3000/"), url("https://example.com/")];
        assert!(follow(&via, &url("http://192.168.1.1/"), &[]).is_err());
    }

    /// A server on this computer that answers each request with what
    /// `answer` makes of its path, and the paths it has been asked for.
    fn serve(answer: fn(&str) -> String) -> (String, Arc<Mutex<Vec<String>>>) {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.local_addr().unwrap());
        let asked = Arc::new(Mutex::new(Vec::new()));
        let heard = asked.clone();
        std::thread::spawn(move || {
            for mut socket in server.incoming().flatten() {
                // A request that never ends is answered as it stands rather
                // than holding the server, so a test fails instead of hanging.
                let _ = socket.set_read_timeout(Some(Duration::from_secs(10)));
                let mut head = Vec::new();
                let mut block = [0; 4096];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match socket.read(&mut block) {
                        Ok(n) if n > 0 => head.extend_from_slice(&block[..n]),
                        _ => break,
                    }
                }
                let head = String::from_utf8_lossy(&head);
                let path = head.split(' ').nth(1).unwrap_or_default().to_owned();
                let reply = answer(&path);
                heard.lock().unwrap().push(path);
                let _ = socket.write_all(reply.as_bytes());
            }
        });
        (base, asked)
    }

    /// Where each page of the stand-in site sends the fetch on to.
    fn site(path: &str) -> String {
        let to = match path {
            "/hop" => "/end",
            "/loop" => "/loop",
            "/metadata" => "http://169.254.169.254/latest/meta-data/",
            "/leak" => "/?k=sk-live-0123456789abcdef",
            "/name" => "http://Sk-Live-0123456789ABCDEF.invalid/",
            "/ftp" => "ftp://example.com/file",
            _ => {
                return "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 7\r\n\
                        Connection: close\r\n\r\narrived"
                    .into();
            }
        };
        format!(
            "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
    }

    #[test]
    fn every_redirect_is_checked_like_the_address_asked_for() {
        let (base, asked) = serve(site);
        let key = "sk-live-0123456789abcdef".to_owned();
        let page = fetch(&format!("{base}/hop"), std::slice::from_ref(&key), false).unwrap();
        assert_eq!(page, Page::Text(format!("{base}/end\n\narrived")));
        let why = fetch(&format!("{base}/metadata"), &[], false).unwrap_err();
        assert!(why.contains("metadata"), "{why}");
        let why = fetch(&format!("{base}/leak"), std::slice::from_ref(&key), false).unwrap_err();
        assert!(why.contains("keys or tokens"), "{why}");
        // A redirect's host arrives lowercased, capitals and all.
        let named = "Sk-Live-0123456789ABCDEF".to_owned();
        let why = fetch(&format!("{base}/name"), &[named], false).unwrap_err();
        assert!(why.contains("keys or tokens"), "{why}");
        let why = fetch(&format!("{base}/loop"), &[], false).unwrap_err();
        assert!(why.contains("more than five"), "{why}");
        let why = fetch(&format!("{base}/ftp"), &[], false).unwrap_err();
        assert!(why.contains("not a web page"), "{why}");
        let asked = asked.lock().unwrap();
        assert!(!asked.iter().any(|path| path.contains(&key)), "{asked:?}");
        // The address asked for, then the five redirects that are followed.
        assert_eq!(asked.iter().filter(|path| *path == "/loop").count(), 6);
    }

    /// A PDF of two pages, its page tree written out plainly.
    const PDF: &str = "%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R /Outlines 5 0 R >> endobj\n\
        2 0 obj << /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >> endobj\n\
        3 0 obj << /Type /Page /Parent 2 0 R >> endobj\n\
        4 0 obj << /Type/Page /Parent 2 0 R >> endobj\n\
        5 0 obj << /Type /Outlines /Count 7 >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF\n";

    /// The stand-in site's PDFs: one to save, and one said to be too large.
    fn papers(path: &str) -> String {
        match path {
            "/papers/2401.12345v2.pdf" => format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{PDF}",
                PDF.len()
            ),
            _ => format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n%PDF-",
                MAX_PDF_BYTES + 1
            ),
        }
    }

    #[test]
    fn a_pdf_is_saved_as_a_file_and_one_too_large_is_refused() {
        let (base, _) = serve(papers);
        let page = fetch(&format!("{base}/papers/2401.12345v2.pdf"), &[], false).unwrap();
        let Page::Pdf { address, bytes } = page else {
            panic!("not a PDF: {page:?}");
        };
        assert_eq!(bytes, PDF.as_bytes());
        let dir = std::env::temp_dir().join(format!("letronna-web-pdf-{}", std::process::id()));
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        let said = save_pdf(&ctx, &address, &bytes).unwrap();
        let path = crate::media::saved(&said).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), PDF.as_bytes());
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("2401-12345v2-") && name.ends_with(".pdf"),
            "{name}"
        );
        assert!(said.contains("A PDF of 2 pages, 1 KB, from http"), "{said}");
        std::fs::remove_dir_all(dir).unwrap();

        let why = fetch(&format!("{base}/scan.pdf"), &[], false).unwrap_err();
        assert!(why.contains("larger than 20 MB"), "{why}");
    }

    #[test]
    fn a_pdf_gives_its_page_count_only_when_it_can_be_read_plainly() {
        assert_eq!(pages(PDF.as_bytes()), Some(2));
        let packed =
            "%PDF-1.5\n1 0 obj << /Type /ObjStm /N 3 /Length 9 >> stream\nxx\nendstream endobj";
        assert_eq!(pages(packed.as_bytes()), None);
        assert_eq!(pages(b"1 0 obj << /Type /PagesX /Count 3 >> endobj"), None);
        // The count may come first, and an object's end be missing.
        assert_eq!(
            pages(b"obj << /Count 4 /Type/Pages >> endobj obj /Type /Pages /Count 9"),
            Some(4)
        );
        // A file of nothing but tree nodes that never end is read in one go.
        let endless = b"/Type /Pages /Count 1 ".repeat(MAX_PDF_BYTES as usize / 22);
        assert_eq!(pages(&endless), None);
    }

    #[test]
    fn a_page_is_remembered_for_a_while_under_both_its_addresses() {
        let mut memo = Memo(VecDeque::new());
        let now = Instant::now();
        let asked = || {
            [
                "https://a.example/".to_owned(),
                "https://b.example/".to_owned(),
            ]
        };
        memo.put(asked(), "page".into(), now);
        assert_eq!(memo.get("https://a.example/", now).as_deref(), Some("page"));
        let later = now + MEMO_TIME - Duration::from_secs(1);
        assert_eq!(
            memo.get("https://b.example/", later).as_deref(),
            Some("page")
        );
        assert_eq!(memo.get("https://b.example/", now + MEMO_TIME), None);
        // Read again, a page takes the place of what either address had.
        memo.put(asked(), "old".into(), now);
        let moved = [
            "https://b.example/".to_owned(),
            "https://c.example/".to_owned(),
        ];
        memo.put(moved, "new".into(), now);
        assert_eq!(memo.get("https://a.example/", now), None);
        assert_eq!(memo.get("https://b.example/", now).as_deref(), Some("new"));
        assert_eq!(memo.0.len(), 1);
    }

    #[test]
    fn the_memo_forgets_its_oldest_pages_to_keep_within_its_bounds() {
        let mut memo = Memo(VecDeque::new());
        let now = Instant::now();
        let at = |n: usize| {
            [
                format!("https://{n}.example/"),
                format!("https://{n}.example/"),
            ]
        };
        for n in 0..MEMO_PAGES + 3 {
            memo.put(at(n), n.to_string(), now);
        }
        assert_eq!(memo.0.len(), MEMO_PAGES);
        assert_eq!(memo.get("https://2.example/", now), None);
        assert!(memo.get("https://3.example/", now).is_some());
        // Two halves fill it, so one more byte leaves the older half out.
        let large = "x".repeat(MEMO_BYTES / 2);
        memo.put(at(100), large.clone(), now);
        memo.put(at(101), large, now);
        assert_eq!(memo.0.len(), 2);
        memo.put(at(102), "x".into(), now);
        assert_eq!(memo.get("https://100.example/", now), None);
        assert!(memo.get("https://101.example/", now).is_some());
        memo.put(at(103), "x".repeat(MEMO_BYTES + 1), now);
        assert!(memo.0.is_empty());
    }

    #[test]
    fn a_remembered_page_is_given_again_unless_asked_fresh() {
        // Never looked up, so the page can only have come from the memo.
        let address = "https://remembered.invalid/page";
        let text = format!("{address}\n\nfrom memory");
        let keys = [address.to_owned(), address.to_owned()];
        MEMO.lock().unwrap().put(keys, text.clone(), Instant::now());
        let again = format!("{address}#part");
        assert_eq!(fetch(&again, &[], false), Ok(Page::Text(text)));
        let why = fetch(address, &[], true).unwrap_err();
        assert!(why.contains("Could not fetch"), "{why}");
        // A key found in the address since still stops it.
        let key = "remembered".to_owned();
        let why = fetch(address, &[key], false).unwrap_err();
        assert!(why.contains("keys or tokens"), "{why}");
    }

    #[test]
    fn a_page_on_this_machine_is_fetched_again_every_time() {
        let (base, asked) = serve(site);
        for _ in 0..2 {
            let page = fetch(&format!("{base}/local"), &[], false).unwrap();
            assert_eq!(page, Page::Text(format!("{base}/local\n\narrived")));
        }
        assert_eq!(asked.lock().unwrap().len(), 2);
    }

    #[test]
    fn every_fetch_shares_one_client() {
        let first = client().expect("the client builds");
        let second = client().expect("the client builds");
        assert!(std::ptr::eq(first, second));
    }

    #[test]
    fn a_second_fetch_rides_the_first_ones_connection() {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.local_addr().unwrap());
        let connections = Arc::new(Mutex::new(0));
        let counted = connections.clone();
        std::thread::spawn(move || {
            for mut socket in server.incoming().flatten() {
                *counted.lock().unwrap() += 1;
                std::thread::spawn(move || {
                    let _ = socket.set_read_timeout(Some(Duration::from_secs(10)));
                    let mut block = [0; 4096];
                    // Every request is answered on the socket it came in on,
                    // which stays open for the next.
                    while matches!(socket.read(&mut block), Ok(n) if n > 0) {
                        let reply = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\
                                     Content-Length: 7\r\n\r\narrived";
                        if socket.write_all(reply.as_bytes()).is_err() {
                            break;
                        }
                    }
                });
            }
        });
        for _ in 0..2 {
            let page = fetch(&format!("{base}/"), &[], true).unwrap();
            assert_eq!(page, Page::Text(format!("{base}/\n\narrived")));
            // Time for the pool to take the connection back once its reply
            // has been read.
            std::thread::sleep(Duration::from_millis(200));
        }
        assert_eq!(*connections.lock().unwrap(), 1);
    }

    #[test]
    #[ignore = "Fetches https://example.com: run with --ignored when online"]
    fn fetches_a_real_page() {
        let Ok(Page::Text(page)) = fetch("https://example.com", &[], true) else {
            panic!("no page");
        };
        assert!(page.contains("Example Domain"), "{page}");
    }
}
