//! The cases a run named in its own working notes and then set aside.
//!
//! A run that builds what a request asks names, as it works, the cases it is
//! unsure of: an input with nothing in it, a second reading of a word, a
//! call the request did not spell out. Most often it then waves one off as
//! rare, out of scope or good enough, and that is a case the request's own
//! tests then check. The helpers that review and test its work are kept from
//! its reasons on purpose, so they judge by the request; that keeps these
//! cases from them too. So each is given to them as a row to check, without
//! the run's conclusion.
//!
//! They are picked by their words, with no model. Over the run's own replies,
//! their working notes and what they said, a sentence holding one of
//! `MARKERS` is a weigh-off, and the case it weighs off is the nearest
//! sentence, itself or one of the `BACK` before it, that holds a code span or
//! a quoted literal. Fenced code is skipped, each line is split into
//! sentences as a request is, and a sentence of `GLUED` words or fewer with
//! nothing quoted, such as "Fine." or "Rare.", is a verdict on the one before
//! and is joined to it.
//!
//! A case is its sentence through the clause that holds its last code span
//! or quoted literal, and on through the clauses after it until one where a
//! marker starts, which is dropped with all after it; where a marker follows
//! the literal inside that clause, the case ends with that clause. So what
//! the case is stays, and the verdict and the reasons after it go.

use crate::chat::Message;
use std::ops::Range;

/// Cases given at most: the latest, since a run's later notes are nearer
/// to the work as it stands.
const KEPT: usize = 10;

/// Characters a case is given in at most.
const CHARS: usize = 200;

/// Sentences before a weigh-off its case may be named in, as "Could also
/// set `x`? That would pollute it. I'll keep only y." names it two back.
const BACK: usize = 2;

/// Words in a sentence that, with nothing quoted, is a verdict on the one
/// before it rather than a sentence of its own.
const GLUED: usize = 3;

/// Words and phrases that weigh a case off, in lower case, each matched as
/// whole words; one with ` … ` in it matches its parts in that order within
/// a sentence.
const MARKERS: &[&str] = &[
    "rare",
    "unlikely",
    "not needed",
    "not required",
    "no need to",
    "either interpretation",
    "either way",
    "defensible",
    "toss-up",
    "toss up",
    "done deliberating",
    "hidden test may",
    "hidden tests may",
    "hidden test might",
    "hidden tests might",
    "hidden test probably",
    "hidden tests probably",
    "hidden test could",
    "hidden tests could",
    "more likely to match",
    "keep … fine",
    "arguably",
    "probably yes",
    "probably fine",
    "i'll keep",
    "i'll leave",
    "leave it",
    "set aside",
    "out of scope",
    "edge case",
    "won't matter",
    "doesn't matter",
    "good enough",
    "assume",
];

/// The cases the assistant's replies in `messages` weighed and set aside,
/// in the order they were last named, at most `KEPT` of them, each once by
/// its first code span, or by its words where it has none.
pub(crate) fn doubts(messages: &[Message]) -> Vec<String> {
    let mut found: Vec<(String, String)> = Vec::new();
    let own = messages
        .iter()
        .filter(|message| message.role == "assistant");
    for text in own.flat_map(|message| [&message.reasoning, &message.content]) {
        for case in cases(text) {
            let key = key(&case);
            // Named again, a case stands where it was named last.
            found.retain(|(kept, _)| *kept != key);
            found.push((key, case));
        }
    }
    let first = found.len().saturating_sub(KEPT);
    found.drain(first..).map(|(_, case)| cut(&case)).collect()
}

/// The section a helper's brief ends with for `doubts`; empty with none.
pub(crate) fn section(doubts: &[String]) -> String {
    if doubts.is_empty() {
        return String::new();
    }
    let mut text = String::from(
        "\n\nCases the author weighed and set aside, with its reasons withheld; each needs a \
         row (and, for the tester, a test) of its own, judged by the request's words:",
    );
    for doubt in doubts {
        text.push_str(&format!("\n- {doubt}"));
    }
    text
}

/// The cases `text` weighs off, in its order.
fn cases(text: &str) -> Vec<String> {
    let units = units(text);
    let mut cases = Vec::new();
    for (at, unit) in units.iter().enumerate() {
        if marks(&masked(unit)).is_empty() {
            continue;
        }
        let Some(named) = (at.saturating_sub(BACK)..=at)
            .rev()
            .find(|&before| !literals(&units[before]).is_empty())
        else {
            continue;
        };
        cases.push(case(&units[named]));
    }
    cases
}

/// The sentences of `text` outside fenced code, a verdict of a few words
/// joined to the one before it.
fn units(text: &str) -> Vec<String> {
    let text = text.replace(['\u{2018}', '\u{2019}'], "'");
    let mut units: Vec<String> = Vec::new();
    let mut fence: Option<&str> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(mark) = fence {
            if line.starts_with(mark) {
                fence = None;
            }
            continue;
        }
        if let Some(mark) = ["```", "~~~"].into_iter().find(|m| line.starts_with(m)) {
            fence = Some(mark);
            continue;
        }
        let line = ["- ", "* ", "+ "]
            .iter()
            .find_map(|bullet| line.strip_prefix(bullet))
            .unwrap_or(line);
        for sentence in crate::requirements::split(line) {
            let short =
                sentence.split_whitespace().count() <= GLUED && literals(sentence).is_empty();
            match units.last_mut() {
                Some(last) if short => {
                    last.push(' ');
                    last.push_str(sentence);
                }
                _ => units.push(sentence.to_owned()),
            }
        }
    }
    units
}

/// The code spans and quoted literals of `text`, each with its marks, in
/// order: a span in backticks closed by as many, text in double quotes,
/// and text in single quotes that open after no letter and close before
/// none, so "it'd" is no quote.
fn literals(text: &str) -> Vec<Range<usize>> {
    let bytes = text.as_bytes();
    let word = |at: usize| bytes.get(at).is_some_and(|b| b.is_ascii_alphanumeric());
    let mut found = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let rest = &text[at..];
        let closed = if rest.starts_with('`') {
            let run = rest.bytes().take_while(|&b| b == b'`').count();
            (rest[run..].find(&rest[..run])).map(|close| run + close + run)
        } else if let Some(inner) = rest.strip_prefix('"') {
            inner.find('"').map(|close| close + 2)
        } else if let Some(inner) = rest.strip_prefix('\u{201c}') {
            inner.find('\u{201d}').map(|close| 3 + close + 3)
        } else if let Some(inner) = rest.strip_prefix('\'')
            && (at == 0 || !word(at - 1))
        {
            // Short, so an apostrophe further on is not taken for its end.
            (inner.find('\''))
                .filter(|&close| close <= 40 && !word(at + close + 2))
                .map(|close| close + 2)
        } else {
            None
        };
        match closed {
            Some(len) => {
                found.push(at..at + len);
                at += len;
            }
            None => at += rest.chars().next().map_or(1, char::len_utf8),
        }
    }
    found
}

/// `text` in lower case with each literal's bytes standing as `#`, so its
/// words are weighed at the same places without the quoted ones.
fn masked(text: &str) -> String {
    let mut bytes = text.to_ascii_lowercase().into_bytes();
    for range in literals(text) {
        bytes[range].fill(b'#');
    }
    String::from_utf8(bytes).unwrap_or_default()
}

/// Where each marker that `prose`, as `masked` writes it, holds starts.
fn marks(prose: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    for marker in MARKERS {
        let mut parts = marker.split(" … ");
        let Some(first) = parts.next() else {
            continue;
        };
        let rest: Vec<&str> = parts.collect();
        for start in whole(prose, first) {
            let mut from = start + first.len();
            let follows = rest.iter().all(|part| {
                let next = whole(prose, part).into_iter().find(|&at| at >= from);
                next.inspect(|&at| from = at + part.len()).is_some()
            });
            if follows {
                starts.push(start);
            }
        }
    }
    starts.sort_unstable();
    starts
}

/// Where `phrase` starts in `text` as whole words.
fn whole(text: &str, phrase: &str) -> Vec<usize> {
    let inside = |c: char| c.is_alphanumeric() || c == '_';
    (text.match_indices(phrase))
        .filter(|&(at, _)| {
            let before = text[..at].chars().next_back();
            let after = text[at + phrase.len()..].chars().next();
            !before.is_some_and(|c| inside(c) || c == '\'') && !after.is_some_and(inside)
        })
        .map(|(at, _)| at)
        .collect()
}

/// The clauses of `prose`, as `masked` writes it, each ending after a stop,
/// comma, semicolon or colon before a space, or a dash between spaces.
fn clauses(prose: &str) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    let mut start = 0;
    for (at, c) in prose.char_indices() {
        let end = at + c.len_utf8();
        let next = prose[end..].chars().next();
        let ends = match c {
            ',' | ';' | ':' | '.' | '?' | '!' => next.is_none_or(char::is_whitespace),
            '-' | '\u{2013}' | '\u{2014}' => prose[..at].ends_with(' ') && next == Some(' '),
            _ => false,
        };
        if ends {
            found.push(start..end);
            start = end;
        }
    }
    if start < prose.len() {
        found.push(start..prose.len());
    }
    found
}

/// The case `unit` names, without the verdict and reasons after it.
fn case(unit: &str) -> String {
    let Some(quoted) = literals(unit).last().map(|range| range.end) else {
        return unit.to_owned();
    };
    let prose = masked(unit);
    let marks = marks(&prose);
    let clauses = clauses(&prose);
    let held = (clauses.iter())
        .position(|clause| clause.end >= quoted)
        .unwrap_or(clauses.len().saturating_sub(1));
    let marked = |range: &Range<usize>, from: usize| {
        (marks.iter()).any(|&at| at >= from && at >= range.start && at < range.end)
    };
    let end = match clauses.get(held) {
        Some(own) if marked(own, quoted) => own.end,
        _ => (clauses.iter().skip(held + 1))
            .find(|clause| marked(clause, 0))
            .map_or(unit.len(), |clause| clause.start),
    };
    let trailing = [',', ';', ':', '-', '\u{2013}', '\u{2014}', ' '];
    unit[..end].trim().trim_end_matches(trailing).to_owned()
}

/// What `case` is known by: its first code span, or its words where it has
/// none.
fn key(case: &str) -> String {
    let span = (literals(case).into_iter()).find(|range| case[range.clone()].starts_with('`'));
    match span {
        Some(range) => case[range].trim_matches('`').trim().to_owned(),
        None => case
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase(),
    }
}

/// `case` in at most `CHARS` characters, cut at a word with a `…`.
fn cut(case: &str) -> String {
    if case.chars().count() <= CHARS {
        return case.to_owned();
    }
    let head: String = case.chars().take(CHARS - 1).collect();
    let at = head.rfind(char::is_whitespace).unwrap_or(head.len());
    format!("{}\u{2026}", head[..at].trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An assistant reply with `reasoning` as its working notes and `content`.
    fn said(reasoning: &str, content: &str) -> Message {
        Message {
            role: "assistant".into(),
            reasoning: reasoning.into(),
            content: content.into(),
            ..Default::default()
        }
    }

    fn weighed(notes: &str) -> Vec<String> {
        doubts(&[said(notes, "")])
    }

    #[test]
    fn cases_a_run_waved_off_are_found_without_its_verdict() {
        let found = |notes: &str| weighed(notes);
        assert_eq!(
            found("If test does `expect(...).toBe('false')` it'd fail ... Keep removal. Fine."),
            ["If test does `expect(...).toBe('false')` it'd fail ..."]
        );
        assert_eq!(
            found("`Added(ChildOf, ChildOf(a))`? Rare"),
            ["`Added(ChildOf, ChildOf(a))`?"]
        );
        assert_eq!(
            found(
                "Could also set `e.stackFrames`? That would pollute the Error. \
                 I'll keep only stack."
            ),
            ["Could also set `e.stackFrames`?"]
        );
        assert_eq!(
            found(
                "Return `[]` or `None` for an empty input? \
                 Which is more likely to match hidden tests?"
            ),
            ["Return `[]` or `None` for an empty input?"]
        );
        assert_eq!(
            found("Repeated `--tag` flags: accumulate or replace? I'll keep accumulation"),
            ["Repeated `--tag` flags: accumulate or replace?"]
        );
        assert_eq!(
            found("Calling `reset()` twice clears the history too. Is that desired? Probably yes?"),
            ["Calling `reset()` twice clears the history too. Is that desired?"]
        );
        assert_eq!(
            found("What if width <= 0: return \"\"? Edge case, won't matter."),
            ["What if width <= 0: return \"\"?"]
        );
        assert_eq!(
            found("Negative `count` returns an empty list. That's arguably correct."),
            ["Negative `count` returns an empty list."]
        );
        assert_eq!(
            found(
                "Is `end` inclusive in `range(a, b)` and `slice(a, b)`? \
                 Go exclusive for both. Done deliberating."
            ),
            ["Is `end` inclusive in `range(a, b)` and `slice(a, b)`?"]
        );
    }

    #[test]
    fn a_weigh_off_with_nothing_quoted_near_it_is_no_case() {
        assert!(weighed("This is rare enough to leave as it is.").is_empty());
        assert!(weighed("Edge case handling seems good enough here.").is_empty());
        // Too far back to be the case it weighs off.
        let far = "Parse `a..b` first. Then read the tokens out. Then build the spans up. \
                   Then check each one twice. Rare.";
        assert!(weighed(far).is_empty(), "{:?}", weighed(far));
    }

    #[test]
    fn a_case_with_no_weigh_off_is_no_doubt() {
        assert!(weighed("What if width <= 0: return \"\"? Handle it first.").is_empty());
        assert!(weighed("Call `parse()` then `check()`. It'd fail otherwise.").is_empty());
    }

    #[test]
    fn the_reasons_after_a_verdict_in_the_same_clause_are_dropped() {
        assert_eq!(
            weighed("Handling `None` is not needed, since callers check it first."),
            ["Handling `None` is not needed"]
        );
        assert_eq!(
            weighed("If `width` is 0 we could return `\"\"`, but that's an edge case; leave it."),
            ["If `width` is 0 we could return `\"\"`"]
        );
    }

    #[test]
    fn code_in_fences_and_quoted_markers_weigh_nothing_off() {
        let notes = "Plan:\n```\nif x == \"\" { return } // rare\n```\nUse `\"rare\"` as the key.";
        assert!(weighed(notes).is_empty(), "{:?}", weighed(notes));
        // Single quotes inside words are no literal.
        assert!(weighed("It'd be rare and that's it.").is_empty());
    }

    #[test]
    fn a_case_named_twice_stands_where_it_was_named_last() {
        let found = doubts(&[
            said("`a..` at the end? Rare.", ""),
            said("`b..c` reversed? Out of scope, leave it.", ""),
            said("", "Open `a..` at the very end of input: edge case."),
        ]);
        assert_eq!(
            found,
            ["`b..c` reversed?", "Open `a..` at the very end of input"]
        );
    }

    #[test]
    fn only_the_latest_cases_are_kept_in_order() {
        let notes: Vec<String> = (1..=12)
            .map(|n| format!("Input `{n}` alone? Rare."))
            .collect();
        let found = weighed(&notes.join("\n"));
        assert_eq!(found.len(), KEPT);
        assert_eq!(found.first().map(String::as_str), Some("Input `3` alone?"));
        assert_eq!(found.last().map(String::as_str), Some("Input `12` alone?"));
    }

    #[test]
    fn a_long_case_is_cut_at_a_word() {
        let long = format!(
            "A key `k` {}that never closes? Rare.",
            "with many words ".repeat(20)
        );
        let found = weighed(&long);
        let [case] = found.as_slice() else {
            panic!("{found:?}");
        };
        assert!(case.chars().count() <= CHARS, "{case}");
        let head = case
            .strip_suffix('\u{2026}')
            .unwrap_or_else(|| panic!("{case}"));
        let words = ["with", "many", "words"];
        assert!(words.iter().any(|word| head.ends_with(word)), "{case}");
    }

    #[test]
    fn only_the_assistants_replies_are_read() {
        let user = Message {
            role: "user".into(),
            content: "Empty `name`? Rare.".into(),
            ..Default::default()
        };
        assert!(doubts(&[user]).is_empty());
        assert_eq!(
            doubts(&[said("", "Empty `name`? Rare.")]),
            ["Empty `name`?"]
        );
    }

    #[test]
    fn the_section_lists_each_case_and_is_empty_without_one() {
        assert_eq!(section(&[]), "");
        let text = section(&["Empty `name`?".to_owned()]);
        assert!(
            text.starts_with("\n\nCases the author weighed and set aside"),
            "{text}"
        );
        assert!(
            text.ends_with("judged by the request's words:\n- Empty `name`?"),
            "{text}"
        );
    }
}
