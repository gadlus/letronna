//! What every provider must pass, the template included: a name of its own,
//! a base that says the format it speaks, and a model list read from the
//! reply its format sends, recorded in `fixtures/` and served from this
//! machine, so no key is ever needed.

use super::{AuthKind, PROVIDERS, Provider, Wire};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

fn all() -> Vec<&'static dyn Provider> {
    let mut all = PROVIDERS.to_vec();
    all.push(&super::_template::Template);
    all
}

/// The recorded model list for `wire`, and the ids it holds.
fn fixture(wire: Wire) -> (&'static str, &'static [&'static str]) {
    match wire {
        Wire::OpenAiChat => (
            include_str!("fixtures/openai_chat_models.json"),
            &["gpt-test-mini", "gpt-test-large"],
        ),
        Wire::Anthropic => (
            include_str!("fixtures/anthropic_models.json"),
            &["claude-test-opus", "claude-test-haiku"],
        ),
    }
}

/// A base URL on this machine that answers `GET /v1/models` once with `body`.
fn serve(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream);
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if let Some(n) = line.to_lowercase().strip_prefix("content-length:") {
                length = n.trim().parse().unwrap_or(0);
            }
            if line.trim().is_empty() {
                break;
            }
        }
        reader
            .by_ref()
            .take(length)
            .read_to_end(&mut Vec::new())
            .unwrap();
        let reply = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        reader.get_mut().write_all(reply.as_bytes()).unwrap();
    });
    base
}

#[test]
fn every_provider_has_a_name_of_its_own_and_says_what_it_needs() {
    let mut names = std::collections::HashSet::new();
    for p in all() {
        let info = p.info();
        assert!(!info.name.trim().is_empty(), "a provider with no name");
        assert!(names.insert(info.name), "{} is named twice", info.name);
        assert!(!info.blurb.trim().is_empty(), "{} has no blurb", info.name);
        assert!(info.docs.starts_with("https://"), "{}'s docs", info.name);
    }
}

#[test]
fn every_base_is_given_or_asked_for_and_speaks_its_format() {
    for p in all() {
        let info = p.info();
        if p.auth() == AuthKind::Url {
            assert!(info.base.is_empty(), "{} asks for a URL", info.name);
            continue;
        }
        let base = info.base;
        assert!(
            base.starts_with("https://") || base.starts_with("http://localhost"),
            "{}'s base {base}",
            info.name
        );
        assert!(!base.ends_with('/'), "{}'s base ends in /", info.name);
        let anthropic = super::wire::anthropic::speaks(base);
        assert_eq!(
            p.wire() == Wire::Anthropic,
            anthropic,
            "{} names a format its base does not speak",
            info.name
        );
    }
}

#[test]
fn every_provider_reads_its_model_list() {
    for p in all() {
        let (body, ids) = fixture(p.wire());
        let base = serve(body);
        if p.wire() == Wire::Anthropic {
            super::wire::anthropic::STAND_INS
                .lock()
                .unwrap()
                .push(base.clone());
        }
        let models = p
            .models(&base, "test-key")
            .unwrap_or_else(|e| panic!("{}: {e}", p.info().name));
        assert_eq!(models, ids, "{}'s models", p.info().name);
    }
}
