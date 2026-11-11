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
//!
//! Apart from those, with a quota of their own, are the words of the request
//! the run read more than one way (`readings`). A run most often names both
//! readings of a sentence that leaves something open, takes one and builds
//! only that, and what checks its work afterwards wanted the other. A
//! sentence holding one of `READ` is such a weighing when it, or one of the
//! `BACK` before it, quotes `QUOTED` or more words in a row of a requirement
//! the request states. Only that requirement's number and those words are
//! kept, never the reading the run took, so a helper is not led to it.

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

/// Readings given at most, the latest; apart from `KEPT`, so no case set
/// aside takes the place of one.
const READ_KEPT: usize = 5;

/// Words in a row a quoted literal shares with a requirement to be a quote
/// of it: fewer, such as "must fail", are found in many sentences.
const QUOTED: usize = 4;

/// Words and phrases that weigh what a sentence of the request means, in
/// lower case, each matched where a word starts, so "conflict" finds
/// "conflicts" and "conflicting" too; one with ` … ` in it matches its parts
/// in that order within a sentence, as a question opening with which does.
const READ: &[&str] = &[
    "ambiguous",
    "ambiguity",
    "interpretation",
    "could mean",
    "might mean",
    "means either",
    "or it means",
    "i read",
    "read as",
    "read it as",
    "hard to know",
    "contradict",
    "conflict",
    "two ways",
    "which … ?",
];

/// Words of a requirement the run read more than one way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reading {
    /// The requirement's number, as the requirements notice numbers it.
    pub(crate) number: usize,
    /// The words of it the run quoted as it weighed them.
    pub(crate) words: String,
}

/// The words of `sentences`, the numbered requirements of a request, that
/// the assistant's replies in `messages` read more than one way, in the
/// order they were last weighed, at most `READ_KEPT` of them and one for
/// each requirement, without the reading the run took.
pub(crate) fn readings(messages: &[Message], sentences: &[String]) -> Vec<Reading> {
    let listed: Vec<Vec<String>> = (sentences.iter())
        .take(crate::requirements::LISTED)
        .map(|sentence| words(sentence).into_iter().map(|(_, word)| word).collect())
        .collect();
    let mut found: Vec<Reading> = Vec::new();
    let own = messages
        .iter()
        .filter(|message| message.role == "assistant");
    for text in own.flat_map(|message| [&message.reasoning, &message.content]) {
        let units = units(text);
        for (at, unit) in units.iter().enumerate() {
            if !reads(&masked(unit)) {
                continue;
            }
            let Some(reading) = (at.saturating_sub(BACK)..=at)
                .rev()
                .find_map(|before| quoted(&units[before], &listed))
            else {
                continue;
            };
            // Weighed again, a requirement stands where it was weighed last.
            found.retain(|kept| kept.number != reading.number);
            found.push(reading);
        }
    }
    let first = found.len().saturating_sub(READ_KEPT);
    found.drain(first..).collect()
}

/// What a review helper's brief says of `readings`; empty with none.
pub(crate) fn reading_rows(readings: &[Reading]) -> String {
    let Some(first) = readings.first().map(|reading| reading.number) else {
        return String::new();
    };
    format!(
        "\n\nThe author read these words of the request more than one way; its choice is not \
         shown:{}\nFor each, name the plausible readings and write a row per reading, \
         `R{first} | reading: what the words would mean | PASS`, or FAIL or UNCLEAR in its \
         place. A change that meets only one reading where one implementation could meet \
         every one is FAIL. Where the readings exclude each other the row is UNCLEAR, quoting \
         the words that favour each.",
        list(readings)
    )
}

/// What a tester's brief says of `readings`; empty with none.
pub(crate) fn reading_tests(readings: &[Reading]) -> String {
    if readings.is_empty() {
        return String::new();
    }
    format!(
        "\n\nThe author read these words of the request more than one way; its choice is not \
         shown:{}\nFor each, name the plausible readings and write a test per reading, named \
         for the reading, with a row of its own. Where the readings exclude each other, say \
         so in each such row and quote the words that favour each.",
        list(readings)
    )
}

/// What the requirements notice tells the run of `readings`, after its
/// check; empty with none. A rule the request states for the exact case
/// stays first, as a notice of a failing test says it.
pub(crate) fn reading_notice(readings: &[Reading]) -> String {
    if readings.is_empty() {
        return String::new();
    }
    format!(
        "\n\nYour notes read these words of the request more than one way:{}\nFor each, if one \
         implementation can satisfy every reading without breaking another requirement, it \
         must: build that and test each reading. Only where the readings exclude each other, \
         take the one the request's words favour, quote them, and test that case. A narrower \
         rule the request sets for the exact case, such as a filter, a condition or an \
         exception, still comes before any wider reading.",
        list(readings)
    )
}

/// `readings` as a list's lines, each the requirement's number and the words.
fn list(readings: &[Reading]) -> String {
    (readings.iter())
        .map(|reading| format!("\n- R{}: \"{}\"", reading.number, reading.words))
        .collect()
}

/// Whether `prose`, as `masked` writes it, holds one of `READ`.
fn reads(prose: &str) -> bool {
    let inside = |c: char| c.is_alphanumeric() || c == '_' || c == '\'';
    READ.iter().any(|marker| {
        let mut parts = marker.split(" … ");
        let Some(first) = parts.next() else {
            return false;
        };
        let rest: Vec<&str> = parts.collect();
        prose.match_indices(first).any(|(at, _)| {
            let after = &prose[at + first.len()..];
            !prose[..at].chars().next_back().is_some_and(inside)
                && rest.iter().all(|part| after.contains(part))
        })
    })
}

/// The quote of a requirement in `unit`: among its literals, the longest
/// run of `QUOTED` or more words one shares with one of `listed`, the words
/// of each numbered requirement.
fn quoted(unit: &str, listed: &[Vec<String>]) -> Option<Reading> {
    let mut best: Option<(usize, Reading)> = None;
    for range in literals(unit) {
        let quote = words(&unit[range]);
        let said: Vec<&str> = quote.iter().map(|(_, word)| word.as_str()).collect();
        for (at, sentence) in listed.iter().enumerate() {
            let run = shared(&said, sentence);
            if run.len() < QUOTED || best.as_ref().is_some_and(|(len, _)| *len >= run.len()) {
                continue;
            }
            let len = run.len();
            let written: Vec<&str> = quote[run].iter().map(|(written, _)| *written).collect();
            let marks = [
                '"', '\'', '\u{201c}', '\u{201d}', ',', '.', ';', ':', '\u{2026}',
            ];
            let words = cut(written.join(" ").trim_matches(marks));
            let number = at + 1;
            best = Some((len, Reading { number, words }));
        }
    }
    best.map(|(_, reading)| reading)
}

/// The words of `text` as written, each beside itself in lower case without
/// the marks around it, so a quote is matched whatever points, quotes or
/// emphasis it was written with; a word of marks alone is left out.
pub(crate) fn words(text: &str) -> Vec<(&str, String)> {
    text.split_whitespace()
        .map(|word| {
            let bare = word.trim_matches(|c: char| !c.is_alphanumeric());
            (word, bare.to_lowercase())
        })
        .filter(|(_, bare)| !bare.is_empty())
        .collect()
}

/// The longest run of `said` that is found, word for word, in `sentence`.
fn shared(said: &[&str], sentence: &[String]) -> Range<usize> {
    let mut best = 0..0;
    for from in 0..said.len() {
        for at in 0..sentence.len() {
            let len = (said[from..].iter().zip(&sentence[at..]))
                .take_while(|(said, written)| *said == written)
                .count();
            if len > best.len() {
                best = from..from + len;
            }
        }
    }
    best
}

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
pub(crate) fn literals(text: &str) -> Vec<Range<usize>> {
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

    /// What `readings` finds as pairs, a requirement's number and the words.
    fn read(messages: &[Message], sentences: &[&str]) -> Vec<(usize, String)> {
        let sentences: Vec<String> = sentences.iter().map(|s| (*s).to_owned()).collect();
        (readings(messages, &sentences).into_iter())
            .map(|reading| (reading.number, reading.words))
            .collect()
    }

    /// A requirement about adapters, whose last clauses can be read as one
    /// signal ever or as one signal on an abort.
    const ADAPTERS: &str = "The Mocha, Jasmine2, and QUnit browser-side adapters each guard at \
        every emission point including before and inside deferred callbacks by checking typeof \
        Testem before accessing Testem.aborted, suppressing events once aborted and signaling \
        all-test-results once, with QUnit also clearing its queue.";

    #[test]
    fn words_a_run_called_ambiguous_are_a_reading_of_their_requirement() {
        let notes = "Could hidden tests expect no all-test-results when aborted? Spec says \
                     \"suppressing events once aborted and signaling all-test-results once\" \
                     ambiguous but I read as signal once.";
        let found = read(
            &[said(notes, "")],
            &["Runner abort is idempotent.", ADAPTERS],
        );
        let words = "suppressing events once aborted and signaling all-test-results once";
        assert_eq!(found, [(2, words.to_owned())]);
    }

    #[test]
    fn a_question_of_which_cases_a_quoted_requirement_covers_is_a_reading() {
        // The quote leaves the middle of the sentence out, so the words kept
        // are the longest run of it the requirement holds.
        let notes = "Changes recorded on:\n\
            - initialization on entry (old_value=None? new_value=default)? Hmm. \"DataChangeInfo \
            records accumulated during the current macrostep, with state_id, key, old_value, \
            new_value attributes.\"\n\n\
            Which operations record changes? Entry init, exit removal, set_state_data, and \
            perhaps factory defaults. I'd say: entry initialization records change (old \
            None→value); exit removal records (value→None); set_state_data records (old→new). \
            That seems sensible and testable.";
        let sentences = [
            "get_state_data(state) returns active data dict or None.",
            "state_data_values property snapshots all active data by state identifier.",
            "get_data_changes() returns DataChangeInfo records accumulated during the current \
             macrostep, cleared at each macrostep boundary, with state_id, key, old_value, \
             new_value attributes.",
        ];
        let found = read(&[said(notes, "")], &sentences);
        let words = "DataChangeInfo records accumulated during the current macrostep";
        assert_eq!(found, [(3, words.to_owned())]);
    }

    #[test]
    fn two_requirements_a_run_found_in_conflict_are_a_reading_of_the_one_quoted_last() {
        // The request writes its emphasis and code marks, the notes none.
        let notes = "Hmm: \"Even when hidden, parsing must succeed if the user explicitly \
            provides the dependent option while the dependency is unsatisfied and not \
            required.\" Then next bullet: \"If the dependee is explicitly provided with a falsy \
            value (e.g., --flag=false), that counts as an unsatisfied dependency and supplying \
            the dependent option must fail.\" Contradiction unless latter requires `required: \
            true`. Maybe that test uses requiredWhen?\n\n\
            Hmm, maybe there's a different interpretation: required is default false; hidden \
            when unsatisfied. Explicitly providing dependent when unsatisfied but not required \
            succeeds. When dependee falsy, also succeeds. So bullet conflicts. Could be they \
            want explicit falsy to count as unsatisfied. And \"supplying the dependent option \
            must fail\" in required case. I'll go with that.";
        let sentences = [
            "Even when hidden, parsing must **succeed** if the user explicitly provides the \
             dependent option while the dependency is unsatisfied and not required.",
            "If the dependee is explicitly provided with a **falsy** value (e.g. \
             `--flag=false`), that counts as an unsatisfied dependency and supplying the \
             dependent option **must fail**.",
        ];
        let found = read(&[said(notes, "")], &sentences);
        let [(number, words)] = found.as_slice() else {
            panic!("{found:?}");
        };
        assert_eq!(*number, 2);
        assert!(
            words.starts_with("If the dependee is explicitly"),
            "{words}"
        );
        assert!(words.ends_with("the dependent option must fail"), "{words}");
    }

    #[test]
    fn a_weighing_that_quotes_nothing_of_the_request_is_no_reading() {
        let sentences = ["Ranges must be written as `a..b` with both ends given."];
        let none = |notes: &str| read(&[said(notes, "")], &sentences).is_empty();
        assert!(none("This is ambiguous, and I read it as inclusive."));
        // Quoted, but not the request's words, or too few of them.
        assert!(none(
            "The error \"start is past its end here\" is ambiguous."
        ));
        assert!(none("\"must be written\" could mean either form."));
        // The request's words with no weighing of them.
        assert!(none("It says \"must be written as `a..b`\". Do that."));
        // Too far back to be what is weighed.
        assert!(none(
            "It says \"must be written as `a..b`\". Parse the start. Then parse the end. \
             Then build the range. The naming is ambiguous."
        ));
        // A marker inside a word, or itself quoted, weighs nothing.
        assert!(none(
            "It says \"must be written as `a..b`\", so I spread as needed."
        ));
        assert!(none(
            "Print \"ambiguous\" for \"must be written as `a..b`\"."
        ));
    }

    #[test]
    fn at_most_a_few_readings_are_kept_one_for_each_requirement() {
        let sentences: Vec<String> = (1..=8)
            .map(|n| format!("Kind {n} must be stored in its own table."))
            .collect();
        let notes: Vec<String> = (1..=8)
            .map(|n| format!("\"Kind {n} must be stored in its own table\" could mean a file."))
            .collect();
        let found = readings(&[said(&notes.join("\n"), "")], &sentences);
        let numbers: Vec<usize> = found.iter().map(|reading| reading.number).collect();
        assert_eq!(numbers, [4, 5, 6, 7, 8]);
        // Weighed again, a requirement is listed once, where it was last.
        let again = "\"Kind 5 must be stored in its own table\" is ambiguous. \
                     \"Kind 5 must be stored\" – hmm.\n\
                     I read \"Kind 2 must be stored in its own table\" as a file.\n\
                     Two ways to take \"Kind 5 must be stored in\" then.";
        let found = readings(&[said(again, "")], &sentences);
        let numbers: Vec<usize> = found.iter().map(|reading| reading.number).collect();
        assert_eq!(numbers, [2, 5]);
        // The cases set aside are counted apart, so neither crowds the other out.
        let aside: Vec<String> = (1..=12)
            .map(|n| format!("Input `{n}` alone? Rare."))
            .collect();
        let both = [said(&aside.join("\n"), ""), said(&notes.join("\n"), "")];
        assert_eq!(doubts(&both).len(), KEPT);
        assert_eq!(readings(&both, &sentences).len(), READ_KEPT);
    }

    #[test]
    fn the_reading_a_run_took_is_not_kept() {
        let notes = "\"Open ranges such as `a..` default to the end\" could mean the end of the \
                     line or of the file; I read it as the end of the line and will clamp there.";
        let sentences = ["Open ranges such as `a..` default to the end of the input."];
        let found = read(&[said(notes, "")], &sentences);
        assert_eq!(
            found,
            [(1, "Open ranges such as `a..` default to the end".to_owned())]
        );
        let sentences: Vec<String> = sentences.iter().map(|s| (*s).to_owned()).collect();
        let found = readings(&[said(notes, "")], &sentences);
        for text in [
            reading_rows(&found),
            reading_tests(&found),
            reading_notice(&found),
        ] {
            assert!(text.contains("- R1: \"Open ranges such as `a..` default to the end\""));
            assert!(!text.contains("line"), "{text}");
            assert!(!text.contains("clamp"), "{text}");
        }
    }

    #[test]
    fn readings_are_found_in_working_notes_and_in_what_was_said() {
        let sentences = ["Ranges must be written as `a..b` with both ends given."];
        let weighed = "\"must be written as `a..b`\" is ambiguous about spaces.";
        let found = [(1, "must be written as `a..b`".to_owned())];
        assert_eq!(read(&[said(weighed, "")], &sentences), found);
        assert_eq!(read(&[said("", weighed)], &sentences), found);
        let user = Message {
            role: "user".into(),
            content: weighed.into(),
            ..Default::default()
        };
        assert!(read(&[user], &sentences).is_empty());
    }

    #[test]
    fn each_helper_and_the_run_are_told_of_readings_in_their_own_words() {
        assert_eq!(reading_rows(&[]), "");
        assert_eq!(reading_tests(&[]), "");
        assert_eq!(reading_notice(&[]), "");
        let found = [Reading {
            number: 12,
            words: "signaling all-test-results once".into(),
        }];
        let listed = "more than one way; its choice is not shown:\n\
                      - R12: \"signaling all-test-results once\"\nFor each, name the plausible \
                      readings and write a ";
        let rows = reading_rows(&found);
        assert!(rows.contains(listed), "{rows}");
        assert!(
            rows.contains("`R12 | reading: what the words would mean | PASS`"),
            "{rows}"
        );
        assert!(
            rows.contains(
                "meets only one reading where one implementation could meet every one is FAIL"
            ),
            "{rows}"
        );
        assert!(
            rows.ends_with("quoting the words that favour each."),
            "{rows}"
        );
        let tests = reading_tests(&found);
        assert!(tests.contains(listed), "{tests}");
        assert!(
            tests.contains("a test per reading, named for the reading"),
            "{tests}"
        );
        let told = reading_notice(&found);
        assert!(
            told.starts_with(
                "\n\nYour notes read these words of the request more than one way:\n\
                 - R12: \"signaling all-test-results once\"\nFor each, if one implementation \
                 can satisfy every reading without breaking another requirement, it must: \
                 build that and test each reading."
            ),
            "{told}"
        );
        assert!(
            told.contains("Only where the readings exclude each other"),
            "{told}"
        );
        // A rule the request sets for the exact case is not undone by it.
        assert!(
            told.contains("still comes before any wider reading"),
            "{told}"
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
