//! Keys and tokens kept out of tool results, approval cards, messages to a
//! chat app and the log.
//!
//! Every tool result -- what the model reads, the chat card shows and the
//! session keeps -- every approval card and every message to a chat app has
//! the keys and tokens Letronna holds replaced wherever they appear, by exact
//! value: nothing needs to see them, and a result travels to the model's
//! provider and, in a chat app, to other people. Output from outside the
//! user's files -- a command's, a page's, an MCP server's -- also has what
//! reads as a secret hidden: a word shaped like a vendor's key, a bearer
//! token, a web token, a private key's body, the value of a line such as
//! `API_KEY=...`, since an `.env` printed by `cat` holds keys that were never
//! saved here. Files read with read_file keep such text: the model edits
//! what it read, and code and fixtures are full of it. The log uses the same
//! pass with a broader rule and a marker of its own.
//!
//! Scrubbing comes before any cut, which could split a key and leave part of
//! it showing. Where the cut comes first -- output read only up to a limit,
//! a log that drops its oldest bytes -- what is left of a saved value at
//! either end of the piece is hidden too.
//!
//! A hidden value becomes `MARKER`, which no tool call may carry: a model
//! writing back a file it read, or typing what it saw, would put the marker
//! in the real key's place.

use std::ops::Range;

/// What a hidden key or token becomes in a tool result or a card. Plain
/// words in brackets, so the model knows what it is looking at, and nothing
/// a file would hold by chance, so a write can refuse it.
pub(crate) const MARKER: &str = "[secret hidden by Letronna]";
/// A saved value shorter than this is not looked for: it would match by
/// chance, in addresses and in ordinary words and numbers alike. Keys and
/// tokens are all longer; a short MCP password is left alone.
const SECRET_CHARS: usize = 16;
/// What a cut left of a saved value is hidden from this many characters on:
/// a shorter run at a cut matches ordinary text too often, and says too
/// little of a random key to be worth hiding.
const FRAGMENT_CHARS: usize = 6;
/// The starts of the key formats in common use, masked wherever they appear
/// even when the key is not one saved here.
const KEY_PREFIXES: &[&str] = &[
    "sk-",
    "sk_",
    "rk_",
    "whsec_",
    "gsk_",
    "xai-",
    "tvly-",
    "pplx-",
    "ghp_",
    "gho_",
    "ghs_",
    "ghu_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "xoxa-",
    "xoxb-",
    "xoxe-",
    "xoxo-",
    "xoxp-",
    "xoxr-",
    "xoxs-",
    "xapp-",
    "hf_",
    "npm_",
    "pypi-",
    "AKIA",
    "ASIA",
    "AIza",
];
/// A key-shaped word at least this long is masked: shorter ones are words.
const KEY_CHARS: usize = 16;
/// A word of letters and digits at least this long is masked in the log
/// even with no known prefix: nothing else the app logs is that long without
/// a break. Not in tool output, where it would hide every commit hash.
const TOKEN_CHARS: usize = 40;
/// The words in a name, as in `AWS_SECRET_ACCESS_KEY=...`, that say its
/// value is a secret: ending one of its words, as in `PGPASSWORD`, with or
/// without an s. One that only starts a word, as in `keyboard`, says nothing.
const SECRET_WORDS: &[&str] = &[
    "key",
    "token",
    "secret",
    "password",
    "passwd",
    "credential",
    "auth",
];
/// What ends the name of an error or a warning, as in Python's `KeyError:
/// 'name'`, which heads a message, not a value.
const RAISED: &[&str] = &["error", "exception", "warning"];
/// The last word of a name, with or without an s, for where a secret is kept
/// or what it is called, as in `ssl_key_file=/etc/ssl/private/x.key`, rather
/// than the secret.
const ABOUT_A_SECRET: &[&str] = &["file", "filename", "path", "dir", "url", "name", "type"];
/// A named secret's value is hidden from this length on: shorter ones are
/// flags, numbers and empty placeholders such as `true` or `3600`.
const VALUE_CHARS: usize = 8;

/// Which key-shaped text is hidden besides the exact values.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shapes {
    /// None: a file's own text, which the model edits.
    None,
    /// Vendor keys, bearer and web tokens, private keys and values named as
    /// secrets: output from outside the user's files.
    Vendor,
    /// Those, whatever follows the word bearer, and any long run of letters
    /// and digits: the log, which carries no content to lose.
    Any,
}

/// The user's own keys and tokens, longest first: what no address may carry
/// out and no result or card may show.
pub(crate) fn secrets() -> Vec<String> {
    let mut all: Vec<String> = crate::store::load_keys().into_values().collect();
    all.extend(crate::auth::account().map(|account| account.token));
    // The key `letronna -p` can be given in its environment, which is in
    // none of the files or vault entries the keys are read from.
    all.extend(std::env::var("AGENT_API_KEY").ok());
    let chats = crate::messaging::load();
    all.extend([
        chats.telegram.token,
        chats.discord.bot_token,
        chats.discord.webhook,
        chats.slack.app_token,
        chats.slack.bot_token,
        chats.email.password,
    ]);
    all.extend(crate::mcp::secrets());
    all.extend(crate::wallet::secrets());
    all.extend(crate::phone::secrets());
    let mut all: Vec<String> = all
        .into_iter()
        .map(|secret| secret.trim().to_owned())
        .filter(|secret| secret.chars().count() >= SECRET_CHARS)
        .collect();
    all.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    all.dedup();
    all
}

/// A tool's result, card or message with the user's keys and tokens hidden,
/// and with `shapes` whatever else reads as a secret as well.
pub(crate) fn scrub(text: &str, shapes: bool) -> String {
    let shapes = if shapes { Shapes::Vendor } else { Shapes::None };
    hide(text, &secrets(), shapes, MARKER)
}

/// `text`, cut from something longer at either end, scrubbed as `scrub`
/// does, with what the cut left of a saved value at either end hidden too,
/// and with `shapes` the start of a vendor's key the end cut short.
pub(crate) fn scrub_cut(text: &str, shapes: bool) -> String {
    let secrets = secrets();
    let text = hide_split(&hide_values(text, &secrets, MARKER), &secrets, MARKER);
    if !shapes {
        return text;
    }
    let mut text = hide_shapes(&text, Shapes::Vendor, MARKER);
    let word = text.len() - text.trim_end_matches(word_char).len();
    let tail = &text[text.len() - word..];
    if word >= FRAGMENT_CHARS && KEY_PREFIXES.iter().any(|prefix| tail.starts_with(prefix)) {
        text.replace_range(text.len() - word.., MARKER);
    }
    text
}

/// `text` with each of `secrets`, and what `shapes` takes for a secret,
/// replaced by `marker`.
pub(crate) fn hide(text: &str, secrets: &[String], shapes: Shapes, marker: &str) -> String {
    let text = hide_values(text, secrets, marker);
    match shapes {
        Shapes::None => text,
        _ => hide_shapes(&text, shapes, marker),
    }
}

/// `text` with every place one of `secrets` appears replaced by `marker`,
/// values that overlap or hold one another hidden as one.
fn hide_values(text: &str, secrets: &[String], marker: &str) -> String {
    let mut found: Vec<Range<usize>> = secrets
        .iter()
        .filter(|secret| !secret.is_empty())
        .flat_map(|secret| text.match_indices(secret.as_str()))
        .map(|(at, secret)| at..at + secret.len())
        .collect();
    if found.is_empty() {
        return text.to_owned();
    }
    found.sort_by_key(|range| range.start);
    let mut merged: Vec<Range<usize>> = Vec::new();
    for range in found {
        match merged.last_mut() {
            Some(last) if range.start < last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut shown = 0;
    for hidden in merged {
        out.push_str(&text[shown..hidden.start]);
        out.push_str(marker);
        shown = hidden.end;
    }
    out.push_str(&text[shown..]);
    out
}

/// `text` with a run at its start that ends one of `secrets`, and a run at
/// its end that begins one, replaced by `marker`: what a cut left of a value
/// it split.
fn hide_split(text: &str, secrets: &[String], marker: &str) -> String {
    let head = secrets.iter().map(|secret| ended(text, secret)).max();
    let tail = secrets.iter().map(|secret| begun(text, secret)).max();
    let (head, tail) = (head.unwrap_or(0), tail.unwrap_or(0));
    if head == 0 && tail == 0 {
        return text.to_owned();
    }
    if head + tail >= text.len() {
        return marker.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    if head > 0 {
        out.push_str(marker);
    }
    out.push_str(&text[head..text.len() - tail]);
    if tail > 0 {
        out.push_str(marker);
    }
    out
}

/// The length of the longest run ending `text` that begins `secret` and is
/// `FRAGMENT_CHARS` long or more, or 0.
fn begun(text: &str, secret: &str) -> usize {
    (1..secret.len())
        .rev()
        .filter(|&len| secret.is_char_boundary(len))
        .find(|&len| text.ends_with(&secret[..len]))
        .filter(|&len| secret[..len].chars().count() >= FRAGMENT_CHARS)
        .unwrap_or(0)
}

/// The length of the longest run starting `text` that ends `secret` and is
/// `FRAGMENT_CHARS` long or more, or 0.
fn ended(text: &str, secret: &str) -> usize {
    (1..secret.len())
        .rev()
        .map(|len| secret.len() - len)
        .filter(|&at| secret.is_char_boundary(at))
        .find(|&at| text.starts_with(&secret[at..]))
        .filter(|&at| secret[at..].chars().count() >= FRAGMENT_CHARS)
        .map_or(0, |at| secret.len() - at)
}

/// `text` with what `shapes` takes for a secret replaced by `marker`.
fn hide_shapes(text: &str, shapes: Shapes, marker: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut after_bearer = false;
    let mut line_start = true;
    let mut rest = text;
    while let Some(first) = rest.chars().next() {
        let end = if shapes == Shapes::Any && after_bearer && !first.is_whitespace() {
            // The log hides whatever follows the word, dots and all.
            after_bearer = false;
            out.push_str(marker);
            rest.find(char::is_whitespace).unwrap_or(rest.len())
        } else if rest.starts_with(marker) {
            // Hidden already, as a saved value.
            out.push_str(marker);
            marker.len()
        } else if let Some(hidden) = shaped(rest, line_start) {
            out.push_str(&rest[..hidden.start]);
            out.push_str(marker);
            hidden.end
        } else if !word_char(first) {
            out.push(first);
            first.len_utf8()
        } else {
            let mut end = rest.find(|c: char| !word_char(c)).unwrap_or(rest.len());
            let word = &rest[..end];
            out.push_str(if key_shaped(word, shapes) {
                marker
            } else {
                word
            });
            after_bearer = word.eq_ignore_ascii_case("bearer");
            if after_bearer
                && shapes == Shapes::Vendor
                && let Some((spaces, token)) = bearer_token(&rest[end..], marker)
            {
                out.push_str(&rest[end..end + spaces]);
                out.push_str(marker);
                end += spaces + token;
            }
            end
        };
        line_start = rest[..end].ends_with('\n');
        rest = &rest[end..];
    }
    out
}

/// What at the start of `rest` reads as a secret by its shape, as the range
/// to hide, the text before it kept.
fn shaped(rest: &str, line_start: bool) -> Option<Range<usize>> {
    let named = line_start.then(|| named_value(rest)).flatten();
    named
        .or_else(|| private_key(rest))
        .or_else(|| web_token(rest).map(|len| 0..len))
        .or_else(|| google_token(rest).map(|len| 0..len))
}

/// The spaces and the token after the word bearer, at the start of `rest`,
/// when the token is one to hide: a run of token characters `KEY_CHARS`
/// long or more, or one that starts with a value hidden already.
fn bearer_token(rest: &str, marker: &str) -> Option<(usize, usize)> {
    let spaces = spaces(rest);
    let token = &rest[spaces..];
    let hidden = if token.starts_with(marker) {
        marker.len()
    } else {
        0
    };
    let len = hidden
        + token[hidden..]
            .find(|c: char| !token_char(c))
            .unwrap_or(token.len() - hidden);
    (spaces > 0 && (hidden > 0 || len >= KEY_CHARS)).then_some((spaces, len))
}

/// Where the value is in the line starting `rest` when the name before it
/// says it is a secret, as in `API_KEY=...`, `password: ...`, `export
/// TOKEN="..."` or `"SecretAccessKey": "..."`. A value not in quotes must
/// end its word, so code such as `token = make_token(user)` is kept.
fn named_value(rest: &str) -> Option<Range<usize>> {
    let line = &rest[..rest.find('\n').unwrap_or(rest.len())];
    let mut at = spaces(line);
    for lead in ["export ", "- "] {
        if line[at..].starts_with(lead) {
            at += lead.len();
            at += spaces(&line[at..]);
        }
    }
    let quote = line[at..]
        .chars()
        .next()
        .filter(|c| *c == '"' || *c == '\'');
    at += quote.map_or(0, char::len_utf8);
    let name = line[at..]
        .find(|c: char| !(c.is_ascii_alphanumeric() || "_.-".contains(c)))
        .unwrap_or(line.len() - at);
    if !secret_name(&line[at..at + name]) {
        return None;
    }
    at += name;
    if let Some(quote) = quote {
        at += line[at..].starts_with(quote).then_some(1)?;
    }
    at += spaces(&line[at..]);
    at += line[at..].starts_with(['=', ':']).then_some(1)?;
    // `==` compares and `::` names a path.
    if line[at..].starts_with(['=', ':']) {
        return None;
    }
    at += spaces(&line[at..]);
    let value = &line[at..];
    let range = match value.chars().next() {
        Some(close @ ('"' | '\'')) => at + 1..at + 1 + value[1..].find(close)?,
        // A quoted name, as in JSON, takes a quoted string for a secret.
        _ if quote.is_some() => return None,
        _ => {
            let len = value.find(|c: char| !secret_char(c)).unwrap_or(value.len());
            let next = value[len..].chars().next();
            if next.is_some_and(|c| !(c.is_whitespace() || c == '"' || c == '\'')) {
                return None;
            }
            at..at + len
        }
    };
    (line[range.clone()].chars().count() >= VALUE_CHARS).then_some(range)
}

/// Whether `name`, as in `name=value`, says its value is a secret: one of
/// its words ends with a secret word, and its last word does not make it an
/// error's name or a name for where a secret is kept.
fn secret_name(name: &str) -> bool {
    let words = name_words(name);
    let Some(last) = words.last() else {
        return false;
    };
    // A header such as `Authorization` or `Proxy-Authorization` carries the
    // key itself, with or without a scheme before it.
    if last == "authorization" {
        return true;
    }
    let about = last.strip_suffix('s').unwrap_or(last);
    if RAISED.iter().any(|raised| last.ends_with(raised)) || ABOUT_A_SECRET.contains(&about) {
        return false;
    }
    // A capital inside a secret word, as in `passWord`, splits it in two, so
    // the parts between `_`, `.` and `-` are read whole as well.
    words.iter().any(|word| ends_with_secret_word(word))
        || name
            .split(['_', '.', '-'])
            .any(|part| ends_with_secret_word(&part.to_ascii_lowercase()))
}

/// Whether `word`, in small letters, ends with a secret word, with or without
/// an s.
fn ends_with_secret_word(word: &str) -> bool {
    let word = word.strip_suffix('s').unwrap_or(word);
    SECRET_WORDS.iter().any(|secret| word.ends_with(secret))
}

/// The words of `name`, lowercased: split at `_`, `.` and `-` and where a
/// capital follows a small letter (`apiKey`). An acronym runs into the word
/// after it, so a run of capitals ends before its last one when a small
/// letter follows (`APIKey` is api and key). Letters and digits are split
/// too, so `KEY2` is still a key.
fn name_words(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut spaced = String::with_capacity(name.len() * 2);
    for (at, &c) in chars.iter().enumerate() {
        let after = chars.get(at + 1).copied();
        let starts_word = at
            .checked_sub(1)
            .map(|before| chars[before])
            .is_some_and(|before| {
                (before.is_ascii_lowercase() && c.is_ascii_uppercase())
                    || (before.is_ascii_uppercase()
                        && c.is_ascii_uppercase()
                        && after.is_some_and(|after| after.is_ascii_lowercase()))
                    || before.is_ascii_digit() != c.is_ascii_digit()
            });
        if starts_word || !c.is_ascii_alphanumeric() {
            spaced.push(' ');
        }
        if c.is_ascii_alphanumeric() {
            spaced.push(c.to_ascii_lowercase());
        }
    }
    spaced.split_whitespace().map(str::to_owned).collect()
}

/// The body of the private key block starting `rest`: from the line after
/// `-----BEGIN ... PRIVATE KEY-----` to its `-----END` line or, where a cut
/// came first, through the lines of base64 there are.
fn private_key(rest: &str) -> Option<Range<usize>> {
    if !rest.starts_with("-----BEGIN ") {
        return None;
    }
    let head = rest.find('\n')?;
    let label = rest[..head].trim_end();
    if !(label.ends_with("-----") && label.contains("PRIVATE KEY")) {
        return None;
    }
    let start = head + 1;
    let end = match rest[head..].find("\n-----END ") {
        Some(end) => head + end,
        None => {
            let base64 = |line: &&str| {
                let line = line.trim_end();
                !line.is_empty()
                    && line
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
            };
            let lines = rest[start..].split_inclusive('\n').take_while(base64);
            let len: usize = lines.map(str::len).sum();
            start + rest[start..start + len].trim_end().len()
        }
    };
    (end > start && !rest[start..end].trim().is_empty()).then_some(start..end)
}

/// The length of the web token starting `rest`: three base64url parts
/// joined by dots, the first starting `eyJ`, as `{"` encodes. A lone part
/// is kept, since base64 of any JSON starts that way.
fn web_token(rest: &str) -> Option<usize> {
    if !rest.starts_with("eyJ") {
        return None;
    }
    let mut len = 0;
    for part in 0..3 {
        if part > 0 {
            len += rest[len..].starts_with('.').then_some(1)?;
        }
        let run = rest[len..]
            .find(|c: char| !word_char(c))
            .unwrap_or(rest.len() - len);
        if run == 0 {
            return None;
        }
        len += run;
    }
    (len >= KEY_CHARS).then_some(len)
}

/// The length of the Google access token starting `rest`, whose dot would
/// split it for the word rule.
fn google_token(rest: &str) -> Option<usize> {
    let token = rest.strip_prefix("ya29.")?;
    let len = token
        .find(|c: char| !(word_char(c) || c == '.'))
        .unwrap_or(token.len());
    // A sentence's full stop is not part of it.
    let len = token[..len].trim_end_matches('.').len();
    Some("ya29.".len() + len).filter(|len| *len >= KEY_CHARS)
}

fn word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// A character of a bearer token, as RFC 6750 spells one.
fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "-._~+/=".contains(c)
}

/// A character a secret value is written with: anything visible but quotes
/// and the brackets and punctuation code puts around a value.
fn secret_char(c: char) -> bool {
    c.is_ascii_graphic() && !"\"'`()[]{}<>,;\\".contains(c)
}

/// How many spaces and tabs start `text`.
fn spaces(text: &str) -> usize {
    text.len() - text.trim_start_matches([' ', '\t']).len()
}

fn key_shaped(word: &str, shapes: Shapes) -> bool {
    let long = word.len() >= KEY_CHARS;
    let prefixed = long && KEY_PREFIXES.iter().any(|prefix| word.starts_with(prefix));
    let digit = word.chars().any(|c| c.is_ascii_digit());
    match shapes {
        Shapes::None => false,
        // A name such as `npm_config_cache` has the shape but no digit; a
        // random key this long all but always has one.
        Shapes::Vendor => prefixed && digit,
        Shapes::Any => {
            // A lone part of a web token, as the log has always hidden it.
            let part = long && word.starts_with("eyJ");
            let mixed =
                word.len() >= TOKEN_CHARS && digit && word.chars().any(|c| c.is_ascii_alphabetic());
            prefixed || part || mixed
        }
    }
}

/// Whether `after` holds the marker more often than `before` did: a change
/// that would put a hidden value back as its marker.
pub(crate) fn adds_marker(before: &str, after: &str) -> bool {
    after.matches(MARKER).count() > before.matches(MARKER).count()
}

/// Whether writing `after` over a file holding `before` would put a hidden
/// value back as its marker. Where the file holds a value that reads as the
/// marker, any marker in `after` could be standing for it.
pub(crate) fn rewrites_marker(before: &str, after: &str) -> bool {
    adds_marker(before, after)
        || after.contains(MARKER) && adds_marker(before, &scrub(before, false))
}

/// Why a write that adds the marker is refused, `what` naming what carried it.
pub(crate) fn refused(what: &str) -> String {
    format!(
        "Refused: {what} has {MARKER} in it, which stands for a key or token that was \
         hidden from you, so this would replace the real one with the marker. Leave the \
         line that holds it as it is and change the lines around it with edit_file, or \
         write a placeholder for the user to fill in."
    )
}

/// Why a call to `tool` whose arguments hold the marker is refused.
pub(crate) fn carried(tool: &str) -> String {
    format!(
        "Refused: the arguments of `{tool}` have {MARKER} in them, which stands for a \
         key or token that was hidden from you, so the call would write, type or send \
         the marker in the key's place. Leave the value out, or have a command read it \
         where it is kept -- an environment variable or its file."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved() -> Vec<String> {
        vec![
            "https://chat.example/hooks/9876543210/tok-0123456789abcdef".into(),
            "tok-0123456789abcdef".into(),
        ]
    }

    fn vendor(text: &str) -> String {
        hide(text, &[], Shapes::Vendor, MARKER)
    }

    #[test]
    fn a_saved_value_is_hidden_wherever_it_appears() {
        let text = "hook https://chat.example/hooks/9876543210/tok-0123456789abcdef \
                    and X-Key:tok-0123456789abcdef; also mytok-0123456789abcdefgh";
        let hidden = hide(text, &saved(), Shapes::None, MARKER);
        assert_eq!(
            hidden,
            format!("hook {MARKER} and X-Key:{MARKER}; also my{MARKER}gh")
        );
    }

    #[test]
    fn saved_values_that_overlap_are_hidden_as_one() {
        let saved = ["0123456789abcdefXYZW".into(), "XYZWklmnopqrstuvwxyz".into()];
        let text = "a 0123456789abcdefXYZWklmnopqrstuvwxyz b";
        assert_eq!(
            hide(text, &saved, Shapes::None, MARKER),
            format!("a {MARKER} b")
        );
    }

    #[test]
    fn what_a_cut_left_of_a_saved_value_is_hidden_at_either_end() {
        let saved = ["QWERTYUIOPasdfghjklZXCVBNM0123456789qwerty".to_owned()];
        let text = "6789qwerty and more, then QWERTYUIOPas";
        assert_eq!(
            hide_split(text, &saved, MARKER),
            format!("{MARKER} and more, then {MARKER}")
        );
        // Five characters say too little to hide, and could be anything.
        let short = "werty and more, then QWERT";
        assert_eq!(hide_split(short, &saved, MARKER), short);
        assert_eq!(hide_split("QWERTYUIOP", &saved, MARKER), MARKER);
    }

    #[test]
    fn vendor_shapes_are_hidden_only_where_asked() {
        let text = "OPENAI_KEY_FILE sk-proj-0123456789abcdef and ghp_0123456789abcdefghij";
        assert_eq!(hide(text, &[], Shapes::None, MARKER), text);
        assert_eq!(
            vendor(text),
            format!("OPENAI_KEY_FILE {MARKER} and {MARKER}")
        );
    }

    #[test]
    fn more_vendor_keys_are_hidden() {
        for key in [
            "ASIAIOSFODNN7EXAMPLE",
            "xoxa-2-0123456789-abcdef",
            "xoxe-1-0123456789-abcdef",
            "xoxr-0123456789-abcdefgh",
            "xoxs-0123456789-abcdefgh",
            "rk_live_0123456789abcdefgh",
            "whsec_0123456789abcdefghij",
            "xai-0123456789abcdefghijkl",
            "tvly-dev-0123456789abcdefgh",
            "pplx-0123456789abcdefghijkl",
            "ya29.a0AfH6SMB-0123456789_abcdef",
        ] {
            assert_eq!(
                vendor(&format!("got {key}.")),
                format!("got {MARKER}."),
                "{key}"
            );
        }
    }

    #[test]
    fn a_web_token_is_hidden_whole_and_plain_base64_kept() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N";
        assert_eq!(
            vendor(&format!("token {jwt} end")),
            format!("token {MARKER} end")
        );
        // Base64 of any JSON starts `eyJ`; one part alone is data, not a token.
        let blob = "eyJuYW1lIjoiYXBwIiwidmVyc2lvbiI6MX0=";
        assert_eq!(vendor(&format!("config {blob}")), format!("config {blob}"));
    }

    #[test]
    fn a_private_key_keeps_its_first_and_last_lines_only() {
        let key = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n\
                   QyNTUxOQAAACBhc2Rm+/=\n-----END OPENSSH PRIVATE KEY-----\nnext";
        assert_eq!(
            vendor(key),
            format!(
                "-----BEGIN OPENSSH PRIVATE KEY-----\n{MARKER}\n\
                 -----END OPENSSH PRIVATE KEY-----\nnext"
            )
        );
        // Cut before its end: the base64 lines there are.
        let cut = "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA\nMIIEpA";
        assert_eq!(
            vendor(cut),
            format!("-----BEGIN RSA PRIVATE KEY-----\n{MARKER}")
        );
        // A search that lists the line has no key to hide.
        let listed = "a.pem:-----BEGIN RSA PRIVATE KEY-----\nb.pem:-----END RSA PRIVATE KEY-----";
        assert_eq!(vendor(listed), listed);
    }

    #[test]
    fn values_named_as_secrets_are_hidden_and_code_is_kept() {
        let text = "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY\n\
                    aws_secret_access_key = wJalrXUtnFEMI/K7MDENG\n\
                    export SESSION_SECRET=\"0123abcd0123abcd\"\n\
                    \x20 \"SecretAccessKey\": \"wJalrXUtnFEMI/K7MDENG\",\n\
                    password: correct-horse-battery\n\
                    - DB_PASSWD=hunter2hunter2\r\n";
        let hidden = format!(
            "AWS_SECRET_ACCESS_KEY={MARKER}\naws_secret_access_key = {MARKER}\n\
             export SESSION_SECRET=\"{MARKER}\"\n  \"SecretAccessKey\": \"{MARKER}\",\n\
             password: {MARKER}\n- DB_PASSWD={MARKER}\r\n"
        );
        assert_eq!(vendor(text), hidden);
        let kept = "token = make_token(user)\n    auth: AuthConfig,\n\
                    TOKENIZERS_PARALLELISM=false\nauthor = \"[Name] <someone@example.com>\"\n\
                    \"keys\": [\"a\", \"b\"],\n\"max_tokens\": 409600000,\n\
                    echo API_KEY=abcdefghijkl\nprimary_key == other_primary_key";
        assert_eq!(vendor(kept), kept);
    }

    #[test]
    fn errors_and_names_that_only_hold_a_secret_word_are_kept() {
        let text = "Traceback (most recent call last):\n\
                    \x20 File \"/app/load.py\", line 8, in <module>\n\
                    \x20   settings = table[\"retrieval_settings\"]\n\
                    KeyError: 'retrieval_settings'\n\
                    jwt.exceptions.InvalidTokenError: Signature verification failed\n\
                    keyboard.drv=keyboard.drv\n\
                    ssh-keygen: generating new host keys\n\
                    smtpd_tls_key_file=/etc/ssl/private/x.key\n\
                    \x20 \"authorization_endpoint\": \"https://accounts.example.com/o/oauth2/auth\",\n";
        assert_eq!(vendor(text), text);
    }

    #[test]
    fn secret_words_that_end_a_word_or_come_in_the_plural_are_hidden() {
        let text = "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY\n\
                    PGPASSWORD=hunter2hunter2\n\
                    API_KEYS=0123abcd0123abcd\n\
                    password = \"correct horse battery staple\"\n\
                    \x20 \"passWord\": \"hunter2hunter2\"\n";
        let hidden = format!(
            "AWS_SECRET_ACCESS_KEY={MARKER}\nPGPASSWORD={MARKER}\nAPI_KEYS={MARKER}\n\
             password = \"{MARKER}\"\n  \"passWord\": \"{MARKER}\"\n"
        );
        assert_eq!(vendor(text), hidden);
    }

    #[test]
    fn a_name_is_read_word_by_word() {
        assert_eq!(
            name_words("smtpd_tls_keyFile.APIKey2"),
            ["smtpd", "tls", "key", "file", "api", "key", "2"]
        );
        for name in [
            "PGPASSWORD",
            "secretkey",
            "AUTHTOKEN",
            "accessToken",
            "APIKey",
            "api_keys",
            "secrets",
            "API_KEY2",
            "x-auth-token",
            "OAuthToken",
            "credentials",
            "passWord",
            "userPassWord",
            "PASSWORDs",
            "Authorization",
            "Proxy-Authorization",
        ] {
            assert!(secret_name(name), "{name}");
        }
        for name in [
            "keyboard",
            "keygen",
            "keyring",
            "KeyError",
            "TokenWarning",
            "SecretException",
            "key_file",
            "keyfile",
            "tokenPath",
            "key_files",
            "KEY_FILENAME",
            "GIT_AUTHOR_NAME",
            "TOKENIZERS_PARALLELISM",
            "authorization_endpoint",
        ] {
            assert!(!secret_name(name), "{name}");
        }
    }

    #[test]
    fn a_bearer_token_is_hidden_and_nothing_after_it() {
        let text = "Authorization: Bearer tok-0123456789abcdef\n\
                    authorization: bearer eyJhbGciOiJI.UzI1NiJ9.abc, next\n\
                    Bearer tok-0123456789abcdef.tailpart-XYZ done";
        let expected = format!(
            "Authorization: Bearer {MARKER}\nauthorization: bearer {MARKER}, next\n\
             Bearer {MARKER} done"
        );
        assert_eq!(hide(text, &saved(), Shapes::Vendor, MARKER), expected);
        // A page that names the scheme loses nothing after it.
        let spec = r#"{"securitySchemes":{"auth":{"type":"http","scheme":"bearer","bearerFormat":"JWT"}},"paths":{"/users":{"get":{"summary":"List users"}}}}"#;
        assert_eq!(vendor(spec), spec);
        let script = r#"headers.Authorization="Bearer "+token;fetch(url,{headers})"#;
        assert_eq!(vendor(script), script);
    }

    #[test]
    fn command_output_keeps_hashes_ids_names_and_prose() {
        let text = "commit 0123456789abcdef0123456789abcdef01234567\n\
                    id 123e4567-e89b-12d3-a456-426614174000\n\
                    integrity sha512-0123456789abcdefghijklmnopqrstuvwxyzABCDEF==\n\
                    data aGVsbG8gd29ybGQsIHRoaXMgaXMgcGxhaW4gYmFzZTY0IGRhdGE=\n\
                    npm_config_cache_folder=/tmp/npm sk-1 hf_hub\n\
                    The bearer of this letter, and Bearer: none, the bearer\n\
                    0123456789abcdefghijklmno";
        assert_eq!(vendor(text), text);
    }

    #[test]
    fn the_log_hides_as_it_always_has() {
        let text = "Bearer abc.def-123 0123456789abcdefghij0123456789abcdefghij";
        assert_eq!(hide(text, &[], Shapes::Any, "***"), "Bearer *** ***");
        assert_eq!(vendor(text), text);
        // Whatever follows the word, a saved value's tail included.
        let text = "Bearer tok-0123456789abcdef.tailpart-XYZ \"scheme\":\"bearer\",\"x\":1 y";
        assert_eq!(
            hide(text, &saved(), Shapes::Any, "***"),
            "Bearer *** \"scheme\":\"bearer*** y"
        );
        assert_eq!(hide("eyJ0eXAiOiJKV1QifQ", &[], Shapes::Any, "***"), "***");
    }

    #[test]
    fn only_a_write_that_adds_the_marker_is_caught() {
        let kept = format!("const MARKER: &str = \"{MARKER}\";");
        assert!(!adds_marker(&kept, &kept.replace("const", "pub const")));
        assert!(adds_marker("key = sk-real", &format!("key = {MARKER}")));
        assert!(adds_marker(&kept, &format!("{kept}\n{MARKER}")));
        assert!(!adds_marker("", "no marker"));
    }
}
