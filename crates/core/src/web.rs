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
        fetch(args["url"].as_str().unwrap_or_default())
    }
    fn needs_approval(&self) -> bool {
        true
    }
    fn concurrent(&self) -> bool {
        true
    }
    fn preview(&self, args: &Value, _: &ToolContext) -> String {
        format!("GET {}", args["url"].as_str().unwrap_or_default())
    }
}

/// The page at `url` as text, after its final address.
fn fetch(url: &str) -> Result<String, String> {
    let url =
        reqwest::Url::parse(url.trim()).map_err(|_| format!("`{url}` is not a web address."))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("Only http and https pages can be fetched.".into());
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent(concat!("Letronna/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(url)
        .send()
        .map_err(|e| format!("Could not fetch the page: {e}"))?;
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
        assert!(fetch("file:///C:/Windows/win.ini").is_err());
        assert!(fetch("not an address").is_err());
    }

    #[test]
    #[ignore = "Fetches https://example.com: run with --ignored when online"]
    fn fetches_a_real_page() {
        let page = fetch("https://example.com").unwrap();
        assert!(page.contains("Example Domain"), "{page}");
    }
}
