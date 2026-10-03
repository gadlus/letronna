//! Replies shaped for a chat app on a phone.
//!
//! A reply longer than a service takes goes out in parts, cut where a
//! reader would pause -- between paragraphs, else between lines -- and never
//! so that a code block loses its fence: one cut in two is closed at the end
//! of one part and opened again, with its language, at the start of the
//! next. Lengths are counted in UTF-16 units, which is what Telegram counts
//! and never fewer than the characters Discord counts.
//!
//! Telegram shows markdown's asterisks and backticks as they are, so its
//! parts go as the small HTML it formats. The conversion covers what models
//! write -- code, bold, italic, strikethrough, links, headings, bullets and
//! quotes -- and escapes everything else, so a slip costs formatting and
//! never the message: a part Telegram refuses is sent again as plain text.
//!
//! Slack shows the same marks as they are too, and has its own, mrkdwn, so
//! its parts go through the same reading into those: one asterisk for bold,
//! underscores for italic, a link in angle brackets.

/// How many messages one reply may take. Past that the rest stays in the
/// window rather than flooding the chat: ten Telegram parts are some 40,000
/// characters, far more than a phone is for.
const MAX_PARTS: usize = 10;
/// Room a part keeps beside its longest line for a code block's fences: an
/// opening one with a language of up to 32 characters, and a closing one.
const FENCE_ROOM: usize = 64;

/// The app a part is written out for.
#[derive(Clone, Copy, PartialEq)]
enum Target {
    Telegram,
    Slack,
}

/// A style as Telegram's tag for it and Slack's mark.
type Style = (&'static str, char);
const BOLD: Style = ("b", '*');
const ITALIC: Style = ("i", '_');
const STRIKE: Style = ("s", '~');

/// A code block's opening fence: its run of backticks and its language.
#[derive(Clone, Debug, PartialEq)]
struct Fence {
    ticks: usize,
    lang: String,
}

impl Fence {
    fn open(&self) -> String {
        format!("{}{}", "`".repeat(self.ticks), self.lang)
    }

    fn close(&self) -> String {
        "`".repeat(self.ticks)
    }

    /// Whether `line` ends this block: a run at least as long, alone.
    fn closed_by(&self, line: &str) -> bool {
        fence_of(line).is_some_and(|(ticks, info)| ticks >= self.ticks && info.is_empty())
    }
}

/// The run of three or more backticks `line` starts with, and what follows.
fn fence_of(line: &str) -> Option<(usize, &str)> {
    let rest = line.trim_start();
    let body = rest.trim_start_matches('`');
    let ticks = rest.len() - body.len();
    (ticks >= 3).then(|| (ticks, body.trim()))
}

/// The code block `line` opens, if it is an opening fence. Only a language
/// made of the characters one is spelled with is kept, since it ends up in
/// an HTML attribute.
fn opens(line: &str) -> Option<Fence> {
    let (ticks, info) = fence_of(line)?;
    if info.contains('`') {
        return None;
    }
    let word = info.split_whitespace().next().unwrap_or_default();
    let known = word.len() <= 32
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_+#.-".contains(c));
    Some(Fence {
        ticks,
        lang: if known {
            word.to_owned()
        } else {
            String::new()
        },
    })
}

/// `text` in a code block, shown exactly as written: the fence is longer
/// than any run of backticks inside, so nothing in it can close the block.
pub(super) fn fenced(text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat((longest + 1).max(3));
    format!("{fence}\n{text}\n{fence}")
}

fn units(text: &str) -> usize {
    text.encode_utf16().count()
}

/// `text` in parts of at most `max` units, each readable on its own.
pub(super) fn split(text: &str, max: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut part = Part::default();
    let width = max.saturating_sub(FENCE_ROOM).max(1);
    for line in text.lines().flat_map(|line| pieces(line, width)) {
        // Room for the fence that closes the part if it ends inside a block.
        let closing = part.next(line).map_or(0, |fence| fence.ticks + 1);
        while part.added > 0 && part.size + 1 + units(line) + closing > max {
            part.cut(&mut parts, max);
        }
        part.push(line);
    }
    part.finish(&mut parts);
    if parts.len() > MAX_PARTS {
        let left: usize = parts[MAX_PARTS - 1..]
            .iter()
            .map(|part| part.chars().count())
            .sum();
        parts.truncate(MAX_PARTS - 1);
        parts.push(format!(
            "\u{2026} The rest of this reply, {left} more characters, is in Letronna."
        ));
    }
    parts
}

/// `line` in pieces of at most `width` units, broken after a space where
/// one falls in the second half of a piece. Only a line too long for a part
/// is broken.
fn pieces(line: &str, width: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = line;
    while units(rest) > width {
        let mut end = 0;
        let mut used = 0;
        for (at, c) in rest.char_indices() {
            if used + c.len_utf16() > width {
                break;
            }
            used += c.len_utf16();
            end = at + c.len_utf8();
        }
        // Always move on, whatever the width.
        let end = end.max(rest.chars().next().map_or(1, char::len_utf8));
        let cut = rest[..end]
            .rfind(' ')
            .filter(|&at| units(&rest[..at]) >= width / 2)
            .map_or(end, |at| at + 1);
        out.push(&rest[..cut]);
        rest = &rest[cut..];
    }
    out.push(rest);
    out
}

/// The part being filled.
#[derive(Default)]
struct Part {
    lines: Vec<String>,
    /// Units the lines take, joined by line breaks.
    size: usize,
    /// The code block open after the last line.
    open: Option<Fence>,
    /// The latest place the part could end outside any code block: how many
    /// lines it would keep, and the line the next part would start at.
    soft: Option<(usize, usize)>,
    /// Lines of the reply in it, as against a fence opened again.
    added: usize,
}

impl Part {
    /// The code block open after `line`, were it added.
    fn next(&self, line: &str) -> Option<Fence> {
        match &self.open {
            Some(fence) if fence.closed_by(line) => None,
            Some(fence) => Some(fence.clone()),
            None => opens(line),
        }
    }

    fn push(&mut self, line: &str) {
        let next = self.next(line);
        if self.open.is_none() && self.added > 0 {
            // A blank line is where a paragraph ends; a fence is where a
            // block starts, which reads better whole in the next part.
            if line.trim().is_empty() {
                self.soft = Some((self.lines.len(), self.lines.len() + 1));
            } else if next.is_some() {
                self.soft = Some((self.lines.len(), self.lines.len()));
            }
        }
        if !self.lines.is_empty() {
            self.size += 1;
        }
        self.size += units(line);
        self.lines.push(line.to_owned());
        self.open = next;
        self.added += 1;
    }

    /// End the part: at the latest paragraph or block that leaves it at
    /// least half full, else here, closing a code block the cut goes through
    /// and opening it again at the start of the next part.
    fn cut(&mut self, parts: &mut Vec<String>, max: usize) {
        let soft = self
            .soft
            .filter(|&(keep, _)| units(&self.lines[..keep].join("\n")) >= max / 2);
        let lines = std::mem::take(&mut self.lines);
        let open = self.open.take();
        *self = Part::default();
        let Some((keep, next)) = soft else {
            let mut text = lines.join("\n");
            if let Some(fence) = open {
                text.push('\n');
                text.push_str(&fence.close());
                self.size = units(&fence.open());
                self.lines.push(fence.open());
                self.open = Some(fence);
            }
            return emit(parts, &text);
        };
        emit(parts, &lines[..keep].join("\n"));
        // The rest starts outside any block, so it is laid down again as is.
        for line in &lines[next..] {
            self.push(line);
        }
    }

    fn finish(mut self, parts: &mut Vec<String>) {
        // A block the reply never closed is closed here, so the last part
        // stands on its own as the others do.
        if let Some(fence) = self.open.take() {
            self.lines.push(fence.close());
        }
        emit(parts, &self.lines.join("\n"));
    }
}

/// Add `text` as a part, unless it holds nothing to read.
fn emit(parts: &mut Vec<String>, text: &str) {
    let text = text.trim_end().trim_start_matches(['\n', '\r']);
    if !text.trim().is_empty() {
        parts.push(text.to_owned());
    }
}

/// One part of a reply as the HTML Telegram formats.
pub(super) fn telegram_html(markdown: &str) -> String {
    render(markdown, Target::Telegram)
}

/// One part of a reply in Slack's mrkdwn.
pub(super) fn slack_mrkdwn(markdown: &str) -> String {
    render(markdown, Target::Slack)
}

/// One part of a reply in the marks app `to` formats.
fn render(markdown: &str, to: Target) -> String {
    let mut out = Vec::new();
    let mut code: Option<(Fence, Vec<String>)> = None;
    let mut quote = Vec::new();
    for line in markdown.lines() {
        if let Some((fence, body)) = code.as_mut() {
            if !fence.closed_by(line) {
                body.push(escape(line));
                continue;
            }
            out.push(pre(fence, body, to));
            code = None;
            continue;
        }
        let quoted = line.trim_start().strip_prefix('>');
        if quoted.is_none() && !quote.is_empty() {
            out.push(blockquote(&quote, to));
            quote.clear();
        }
        if let Some(rest) = quoted {
            quote.push(inline(rest.strip_prefix(' ').unwrap_or(rest), false, to));
        } else if let Some(fence) = opens(line) {
            code = Some((fence, Vec::new()));
        } else {
            out.push(block(line, to));
        }
    }
    if let Some((fence, body)) = &code {
        out.push(pre(fence, body, to));
    }
    if !quote.is_empty() {
        out.push(blockquote(&quote, to));
    }
    out.join("\n")
}

fn pre(fence: &Fence, body: &[String], to: Target) -> String {
    let code = body.join("\n");
    match (to, fence.lang.is_empty()) {
        // Slack would show the language as the block's first line.
        (Target::Slack, _) => format!("```\n{code}\n```"),
        (Target::Telegram, true) => format!("<pre>{code}</pre>"),
        (Target::Telegram, false) => format!(
            "<pre><code class=\"language-{}\">{code}</code></pre>",
            fence.lang
        ),
    }
}

fn blockquote(lines: &[String], to: Target) -> String {
    match to {
        Target::Telegram => format!("<blockquote>{}</blockquote>", lines.join("\n")),
        Target::Slack => lines
            .iter()
            .map(|line| format!("> {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn styled(inner: &str, (tag, mark): Style, to: Target) -> String {
    match to {
        Target::Telegram => format!("<{tag}>{inner}</{tag}>"),
        Target::Slack => format!("{mark}{inner}{mark}"),
    }
}

/// One line outside code: a heading goes bold, a bullet gets a dot.
fn block(line: &str, to: Target) -> String {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];
    let hashes = trimmed.len() - trimmed.trim_start_matches('#').len();
    if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
        let text = trimmed[hashes..].trim();
        // A heading written in bold is made bold once: Slack would show the
        // second pair of marks as they are.
        let text = text
            .strip_prefix("**")
            .and_then(|text| text.strip_suffix("**"))
            .filter(|inner| !inner.contains("**"))
            .unwrap_or(text);
        let heading = inline(text, true, to);
        return match heading.is_empty() {
            true => heading,
            false => styled(&heading, BOLD, to),
        };
    }
    for bullet in ["- ", "* ", "+ "] {
        if let Some(item) = trimmed.strip_prefix(bullet) {
            return format!("{indent}\u{2022} {}", inline(item, false, to));
        }
    }
    inline(line, false, to)
}

/// A run of text with its inline marks as the app's and everything else
/// escaped. `nested` is true inside bold, italic or a link, which Telegram
/// lets hold no code, so a code span there keeps only its text, in Slack
/// too so that both show a reply alike.
fn inline(text: &str, nested: bool, to: Target) -> String {
    let mut out = String::new();
    let mut at = 0;
    while let Some(c) = text[at..].chars().next() {
        let before = text[..at].chars().next_back();
        if let Some((html, used)) = span(&text[at..], before, nested, to) {
            out.push_str(&html);
            at += used;
            continue;
        }
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
        at += c.len_utf8();
    }
    out
}

/// The mark `rest` opens with as the app's, and how many bytes it took, or
/// `None` when `rest` starts with plain text. `before` is the character
/// ahead of it: an underscore inside a word, as in snake_case, is no mark.
fn span(rest: &str, before: Option<char>, nested: bool, to: Target) -> Option<(String, usize)> {
    if rest.starts_with('`') {
        let ticks = rest.len() - rest.trim_start_matches('`').len();
        let run = &rest[..ticks];
        // Without a run to close them, or anything between, the backticks
        // are text.
        let found = rest[ticks..].find(run);
        let Some(end) = found.filter(|&end| !rest[ticks..ticks + end].trim().is_empty()) else {
            return Some((run.to_owned(), ticks));
        };
        let code = escape(rest[ticks..ticks + end].trim());
        let html = match (nested, to) {
            (true, _) => code,
            (false, Target::Telegram) => format!("<code>{code}</code>"),
            (false, Target::Slack) => format!("`{code}`"),
        };
        return Some((html, end + 2 * ticks));
    }
    // Slack reads a mark only at the edge of a word, so one inside a word,
    // as in 2*3*4, stays as written rather than becoming one it shows bare.
    let outside_word = |&(_, used): &(&str, usize)| {
        to == Target::Telegram
            || !(before.is_some_and(char::is_alphanumeric)
                || rest[used..].starts_with(char::is_alphanumeric))
    };
    for (mark, style) in [("**", BOLD), ("~~", STRIKE)] {
        if let Some((inner, used)) = delimited(rest, mark).filter(outside_word) {
            return Some((styled(&inline(inner, true, to), style, to), used));
        }
    }
    let starred = rest.starts_with('*');
    let underscored = rest.starts_with('_') && !before.is_some_and(char::is_alphanumeric);
    if starred || underscored {
        let (inner, used) = delimited(rest, &rest[..1]).filter(outside_word)?;
        if underscored && rest[used..].starts_with(char::is_alphanumeric) {
            return None;
        }
        return Some((styled(&inline(inner, true, to), ITALIC, to), used));
    }
    if rest.starts_with('[') {
        return link(rest, to);
    }
    None
}

/// What lies between `mark` and the next `mark` that can close it, and the
/// bytes both marks and it take. Neither end may be a space, as in
/// markdown, so `2 * 3 * 4` stays arithmetic.
fn delimited<'a>(rest: &'a str, mark: &str) -> Option<(&'a str, usize)> {
    let body = rest.strip_prefix(mark)?;
    if body.starts_with(char::is_whitespace) || body.starts_with(mark) {
        return None;
    }
    let mut from = 0;
    while let Some(found) = body[from..].find(mark) {
        let end = from + found;
        let inner = &body[..end];
        // A single star next to another is part of a bold mark.
        let doubled = mark.len() == 1 && body[end + 1..].starts_with(mark);
        if !inner.is_empty() && !inner.ends_with(char::is_whitespace) && !doubled {
            return Some((inner, end + 2 * mark.len()));
        }
        from = end + mark.len() + usize::from(doubled);
    }
    None
}

/// `[text](address)` as a link, for web addresses only: a `tg://` one can
/// ask Telegram to join a group or switch proxies, and nobody asked the
/// model for that.
fn link(rest: &str, to: Target) -> Option<(String, usize)> {
    let close = rest.find("](")?;
    let text = &rest[1..close];
    let after = &rest[close + 2..];
    let end = after.find(')')?;
    let address = &after[..end];
    let opens = ["https://", "http://"]
        .iter()
        .any(|scheme| address.starts_with(scheme));
    if text.trim().is_empty()
        || text.contains(['[', ']'])
        || !opens
        || address.contains(char::is_whitespace)
    {
        return None;
    }
    let text = inline(text, true, to);
    let html = match to {
        Target::Telegram => format!(
            "<a href=\"{}\">{text}</a>",
            escape(address).replace('"', "&quot;")
        ),
        // A bar in the address would end it there and start the label.
        Target::Slack => format!("<{}|{text}>", escape(address).replace('|', "%7C")),
    };
    Some((html, close + 2 + end + 1))
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::{MAX_PARTS, fenced, slack_mrkdwn, split, telegram_html, units};

    /// Each line opening or closing a block, so a part with an even number
    /// has every block it opens closed.
    fn fences(part: &str) -> usize {
        part.lines()
            .filter(|line| line.trim_start().starts_with("```"))
            .count()
    }

    #[test]
    fn a_short_reply_goes_whole_and_an_empty_one_not_at_all() {
        assert_eq!(split("all good", 100), ["all good"]);
        assert!(split("", 100).is_empty());
        assert!(split("\n  \n", 100).is_empty());
    }

    #[test]
    fn a_long_reply_is_cut_between_paragraphs() {
        let paragraph = "word ".repeat(15).trim_end().to_owned();
        let text = [paragraph.as_str(); 6].join("\n\n");
        let parts = split(&text, 200);
        assert!(parts.len() > 1);
        assert!(parts.iter().all(|part| units(part) <= 200));
        // Nothing lost or cut mid-paragraph: the parts are whole paragraphs.
        assert_eq!(parts.join("\n\n"), text);
    }

    #[test]
    fn a_code_block_cut_in_two_is_closed_and_opened_again() {
        let code: Vec<String> = (0..40).map(|n| format!("let x{n} = {n};")).collect();
        let text = format!("Here:\n```rust\n{}\n```\nDone.", code.join("\n"));
        let parts = split(&text, 200);
        assert!(parts.len() > 2);
        for part in &parts {
            assert!(units(part) <= 200, "{part}");
            assert_eq!(fences(part) % 2, 0, "{part}");
        }
        assert!(parts[1..].iter().all(|part| part.starts_with("```rust\n")));
        // Every line of code arrives once, in order.
        let arrived: Vec<&str> = parts
            .iter()
            .flat_map(|part| part.lines())
            .filter(|line| line.starts_with("let "))
            .collect();
        assert_eq!(arrived, code);
        assert!(
            parts
                .last()
                .is_some_and(|part| part.ends_with("```\nDone."))
        );
        // One the reply left open is closed at its end.
        assert_eq!(split("```sh\nls", 100), ["```sh\nls\n```"]);
    }

    #[test]
    fn a_block_that_starts_late_goes_whole_into_the_next_part() {
        let code = ["let value = compute(1, 2, 3);"; 3].join("\n");
        let text = format!("{}\n```\n{code}\n```", "word ".repeat(25).trim_end());
        let parts = split(&text, 200);
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert_eq!(parts[1], format!("```\n{code}\n```"));
    }

    #[test]
    fn a_line_longer_than_a_part_breaks_at_a_space() {
        let words = ["reading"; 100].join(" ");
        let parts = split(&words, 100);
        assert!(parts.iter().all(|part| units(part) <= 100));
        let back: Vec<&str> = parts.iter().flat_map(|p| p.split_whitespace()).collect();
        assert!(back.iter().all(|word| *word == "reading"));
        assert_eq!(back.len(), 100);
    }

    #[test]
    fn length_is_counted_as_telegram_counts_it() {
        // Each of these is two UTF-16 units, the unit Telegram's limit is in.
        let text = "\u{1F600}".repeat(120);
        let parts = split(&text, 100);
        assert!(parts.iter().all(|part| units(part) <= 100));
        // Cut between characters, never inside one.
        assert_eq!(parts.concat().replace('\n', ""), text);
    }

    #[test]
    fn past_the_most_parts_the_rest_is_left_in_letronna() {
        let paragraph = "word ".repeat(18).trim_end().to_owned();
        let text = [paragraph.as_str(); 40].join("\n\n");
        let parts = split(&text, 100);
        assert_eq!(parts.len(), MAX_PARTS);
        let last = parts.last().map(String::as_str).unwrap_or_default();
        assert!(last.contains("The rest of this reply"), "{last}");
    }

    #[test]
    fn telegram_gets_the_common_marks_as_tags_and_the_rest_escaped() {
        assert_eq!(
            telegram_html("**Done** and *checked*: `a<b>` & [the page](https://x.y/?a=1&b=\"2\")"),
            "<b>Done</b> and <i>checked</i>: <code>a&lt;b&gt;</code> &amp; \
             <a href=\"https://x.y/?a=1&amp;b=&quot;2&quot;\">the page</a>"
        );
        assert_eq!(telegram_html("~~gone~~ <br>"), "<s>gone</s> &lt;br&gt;");
    }

    #[test]
    fn code_blocks_keep_their_text_exactly() {
        assert_eq!(
            telegram_html("```rust\nlet a = b < c && *d*;\n```\nafter"),
            "<pre><code class=\"language-rust\">let a = b &lt; c &amp;&amp; *d*;</code></pre>\nafter"
        );
        // A block a cut left open runs to the end of the part.
        assert_eq!(telegram_html("```\nx"), "<pre>x</pre>");
        // A language that is not one cannot reach the attribute.
        assert_eq!(telegram_html("```\"><b>\nx\n```"), "<pre>x</pre>");
    }

    #[test]
    fn what_only_looks_like_a_mark_stays_as_written() {
        for text in [
            "call my_var_name now",
            "2 * 3 * 4",
            "ls *.rs *.toml",
            "a ** b",
            "an ` alone",
            "[a](javascript:alert(1))",
            "[not a link] (https://x.y)",
        ] {
            assert_eq!(telegram_html(text), text.replace('<', "&lt;"), "{text}");
        }
    }

    #[test]
    fn headings_bullets_and_quotes_read_as_they_would_on_screen() {
        assert_eq!(
            telegram_html("# Result\n- one\n  * two\n> quoted `x`\n> more\nend"),
            "<b>Result</b>\n\u{2022} one\n  \u{2022} two\n\
             <blockquote>quoted <code>x</code>\nmore</blockquote>\nend"
        );
    }

    #[test]
    fn code_inside_bold_or_a_link_keeps_only_its_text() {
        // Telegram refuses code nested in either.
        assert_eq!(telegram_html("**use `x`**"), "<b>use x</b>");
        assert_eq!(
            telegram_html("[`run`](https://x.y)"),
            "<a href=\"https://x.y\">run</a>"
        );
    }

    #[test]
    fn every_tag_opened_is_closed_in_order() {
        for text in [
            "**a *b** c*",
            "*a **b* c**",
            "[x **y](https://z.w) v**",
            "_a **b_ c**",
            "**unclosed",
            "```\nopen block",
            "> **quote\nplain**",
        ] {
            let html = telegram_html(text);
            let mut open = Vec::new();
            for tag in html.split('<').skip(1).filter_map(|t| t.split('>').next()) {
                let name = tag.trim_start_matches('/').split(' ').next().unwrap_or("");
                match tag.starts_with('/') {
                    true => assert_eq!(open.pop(), Some(name.to_owned()), "{html}"),
                    false => open.push(name.to_owned()),
                }
            }
            assert!(open.is_empty(), "{html}");
        }
    }

    #[test]
    fn slack_gets_the_common_marks_as_its_own_and_the_rest_escaped() {
        assert_eq!(
            slack_mrkdwn("**Done** and *checked*: `a<b>` & [the page](https://x.y/?a=1&b=2)"),
            "*Done* and _checked_: `a&lt;b&gt;` &amp; <https://x.y/?a=1&amp;b=2|the page>"
        );
        assert_eq!(
            slack_mrkdwn("~~gone~~ <br> _kept_"),
            "~gone~ &lt;br&gt; _kept_"
        );
    }

    #[test]
    fn a_link_label_with_ampersand_is_escaped_inside_the_link() {
        assert_eq!(
            slack_mrkdwn("see [Q&A <here>](https://x.y/a|b)."),
            "see <https://x.y/a%7Cb|Q&amp;A &lt;here&gt;>."
        );
    }

    #[test]
    fn a_fenced_block_drops_its_language_tag() {
        assert_eq!(
            slack_mrkdwn("```rust\nlet a = b < c && *d*;\n```\nafter"),
            "```\nlet a = b &lt; c &amp;&amp; *d*;\n```\nafter"
        );
        // A block a cut left open is closed at the end of the part.
        assert_eq!(slack_mrkdwn("```sh\nls"), "```\nls\n```");
    }

    #[test]
    fn snake_case_and_two_times_three_stay_literal() {
        for text in [
            "call my_var_name now",
            "2 * 3 * 4",
            "2*3*4",
            "ls *.rs *.toml",
            "a ** b",
            "x**2** and ~~y~~z",
            "an ` alone",
            "[a](javascript:alert(1))",
            "[not a link] (https://x.y)",
        ] {
            assert_eq!(slack_mrkdwn(text), text, "{text}");
        }
    }

    #[test]
    fn slack_headings_bullets_and_quotes_read_as_they_would_on_screen() {
        assert_eq!(
            slack_mrkdwn("# Result\n- one\n  * two\n> quoted `x` & y\n> more\nend"),
            "*Result*\n\u{2022} one\n  \u{2022} two\n> quoted `x` &amp; y\n> more\nend"
        );
        // Bold once, not with a second pair Slack would show as it is.
        assert_eq!(slack_mrkdwn("## **Summary**"), "*Summary*");
    }

    #[test]
    fn code_inside_bold_or_a_link_keeps_only_its_text_in_slack_too() {
        assert_eq!(slack_mrkdwn("**use `x`**"), "*use x*");
        assert_eq!(slack_mrkdwn("[`run`](https://x.y)"), "<https://x.y|run>");
    }

    #[test]
    fn a_fence_is_longer_than_any_backticks_it_holds() {
        assert_eq!(fenced("ls"), "```\nls\n```");
        let quoted = fenced("see\n```\nx\n```");
        assert!(quoted.starts_with("````\n") && quoted.ends_with("\n````"));
        // And it stays one block, whole, as Telegram formats it.
        assert_eq!(telegram_html(&quoted), "<pre>see\n```\nx\n```</pre>");
    }
}
