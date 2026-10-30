//! The sentences of a request that state what must hold, read off its words.
//!
//! A run nobody watches that builds what a long request specifies ends once
//! its own idea of the change works, and what it misses is most often one
//! behaviour the request stated in a sentence of its own: an exact error
//! message, values accepted in any order, a second path to the same event.
//! So such a run is sent back once with those sentences quoted, to check its
//! change against each.
//!
//! They are picked by their words, with no model. A sentence is kept when,
//! outside its code, it says must, should, cannot, only, returns, raises,
//! accepts, rejects, fires, supports, defaults to, in any order, at most,
//! each or the like, opens with an imperative such as Add or Ensure, opens
//! with a condition such as When or If and goes on past a comma to what
//! follows, or opens with a code span and says what it does. One about
//! how to work rather than what to build (test files, the working tree, when
//! to stop) is left out, as are a question, a guess, a story told in the
//! first person, the past tense or of how things stand now, and a line that
//! is mostly code. Code blocks, indented code, console sessions, headings
//! and tables are skipped whole; a list item is a sentence of its own, and a
//! line broken inside a paragraph is joined to the next. A sentence ends at
//! a stop outside a code span and not after an abbreviation such as "e.g.".
//! The bar is a few such sentences, so a short request or a bug report,
//! which the notice to run a build or test covers, costs nothing.
//!
//! A run whose work is a file the request names rather than source, such as
//! a table a command wrote, is sent back the same way to check that file
//! with a script written for the check alone, since its own check tends to
//! reuse the code that made the file, and a mistake there agrees with
//! itself. Only the words of the request that read as a file's name or path
//! are looked up, never the folder walked, and only a file changed since the
//! run began counts.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Requirement sentences a notice quotes: past this many the list is a
/// second copy of the request, so it says how many more there are instead.
pub(crate) const LISTED: usize = 40;

/// Fewest requirement sentences that send a run back: a bug report states
/// its fix in three or fewer, its title among them, which the notice to run
/// a build or test covers, while a specification states many.
pub(crate) const FEWEST: usize = 4;

/// What a run that can start a helper is told first after the sentences: a
/// helper that has not seen the work reads the request afresh, where the
/// run itself tends to check only the paths it built, with its own code.
const REVIEW: &str = "First ask a helper that hasn't seen your work: with the agent tool, start a review helper and wait for its report. Give it the request word for word and the files you changed or made, and ask it to try each behaviour the request states against your work and report every one that fails, with the command that shows it, changing no file, and to work out each value it checks with code of its own, not your scripts or helpers. Fix what it reports along with the list, including any case it calls unclear or sets aside, unless the request rules that case out.";

/// The least time a run with a deadline needs left to be asked for the
/// review: the helper takes a few minutes, and the run about as long again
/// to fix what it reports, so with less the review would spend the time the
/// fix needed.
pub(crate) const REVIEW_TIME: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// The least a run's token budget needs left to be asked for the review:
/// what a helper may spend, so the review cannot use up what the fix needs.
pub(crate) const REVIEW_TOKENS: u64 = crate::agents::BUDGET;

/// What the run is told after the sentences.
const CHECK: &str = "These are picked by their wording, so read the request once more for any behaviour it states that the list leaves out. For each, run or write a quick test that shows it holds through the entry point a caller uses, end to end, not only by reading internal state, minding exact messages, strings and orderings. A behaviour tied to an event, such as something being demoted, closed or removed, holds on every call that can cause that event, not only the one the sentence names: find each and test it. A sentence that limits a behaviour, with words such as only, unless, otherwise, if they differ, or while keeping or preserving something, also says where it must not happen: test that side too, with the input closest to the line, such as a value that is there but empty or undefined, values that differ in one place, or new data for something that is to stay as it was. Code of yours that lets a stated behaviour be skipped on some path, such as a flag that turns it off, a default that leaves it out or an early return, needs the request to exempt that path; where it does not, make the behaviour happen there too. Where a sentence names several cases or inputs, try each one in every form it could be written, including any your parser or code might reject before it reaches your new check. Where the request lists the kinds of thing your change handles, such as several types, formats or sources, each other rule it states holds for every one of them: test each rule on each kind, not only on the first. Where it lets parts nest, test the rules inside a nested part too, including one at a position that is missing or empty. Where a sentence's words could mean more than one shape of input, such as a plain value or the same value wrapped in an object, accept and test every shape they could mean, not only the one your code already handles. Spell each the way the project already writes its parts, as its grammar, tests or docs show them, with the new syntax added where those parts allow it, and try that spelling first: it is the one a user writes, not one you invented. A class, function or file the request names that the project already has is the one to change, unless the request asks for a new one: find it and extend it rather than making a new one of that name beside it. Where your change does something the project already does elsewhere, such as reporting a failure, tracking a change or hiding an option, do it the same way: read how the project's own tests call and check the existing ones, follow that over a looser reading of the request's words, and test yours the same way, with the same calls and set-up, such as one object reused across tests or a reset between them. Where they make a change and then call or query for the first time, do that too rather than calling once beforehand to clear or warm it up, since a first call can take its own path. Fix any that fail; skip one that only gives background or the old behaviour. Remove any script or test you added only for this check, unless the task asks for tests. Only your last message is passed on as your answer, so end with your whole answer again, not only what changed.";

/// What a run whose work is files the request names is told after the
/// sentences, in place of `CHECK`.
const CHECK_OUTPUT: &str = "These are picked by their wording, so read the request once more for any rule it states that the list leaves out. Check each against what the files named above hold now, with a short new script that reads only those files and the task's inputs and works out every value it compares itself, reusing none of the scripts or helper functions that made them, since a mistake there would agree with itself. Print only what fails, never a large file whole. Fix any that fail, then delete the check script. Only your last message is passed on as your answer, so end with your whole answer again, not only what changed.";

/// Files the notice for outputs names: the first few are enough to find the
/// rest by.
const NAMED: usize = 5;

/// Words of a request that read as a file's name or path looked up at most,
/// so a request listing a whole tree costs a few dozen looks.
const LOOKED: usize = 64;

/// Words and phrases that make a sentence a requirement, in lower case.
const MARKERS: &[&str] = &[
    "must",
    "should",
    "shall",
    "cannot",
    "can't",
    "can not",
    "may not",
    "need to",
    "needs to",
    "has to",
    "have to",
    "required",
    "requires",
    "only",
    "never",
    "does not",
    "do not",
    "doesn't",
    "don't",
    "return",
    "returns",
    "returned",
    "raise",
    "raises",
    "raised",
    "throw",
    "throws",
    "thrown",
    "reject",
    "rejects",
    "rejected",
    "accept",
    "accepts",
    "accepted",
    "fire",
    "fires",
    "fired",
    "emit",
    "emits",
    "emitted",
    "yield",
    "yields",
    "support",
    "supports",
    "supported",
    "defaults to",
    "default to",
    "in any order",
    "at most",
    "at least",
    "no more than",
    "exactly",
    "instead of",
    "preserve",
    "preserves",
    "preserved",
    "is valid",
    "are valid",
    "is invalid",
    "are invalid",
    "treated as",
    "counts as",
    "each",
    "every",
];

/// First words that make a sentence an instruction to build something.
const IMPERATIVES: &[&str] = &[
    "add",
    "allow",
    "always",
    "apply",
    "avoid",
    "call",
    "convert",
    "create",
    "drop",
    "ensure",
    "exclude",
    "export",
    "expose",
    "fix",
    "handle",
    "implement",
    "include",
    "keep",
    "make",
    "map",
    "remove",
    "rename",
    "respect",
    "sort",
    "store",
    "treat",
    "use",
    "validate",
    "write",
];

/// First words of a condition, which with a comma after it states what
/// follows when it holds.
const CONDITIONS: &[&str] = &["when", "whenever", "if", "unless", "once"];

/// Phrases of the instructions a benchmark or task runner wraps a request
/// in, about how to work rather than what to build.
const PROCESS: &[&str] = &[
    "add test files",
    "modify test files",
    "edit test files",
    "change test files",
    "modify the tests",
    "test files unless",
    "or add test",
    "working tree",
    "this checkout",
    "git checkout of",
    "when you are done",
    "when you're done",
    "when you have finished",
    "smallest correct change",
    "smallest possible change",
    "source code in place",
];

/// Words of a report's story rather than its ask: a guess, the first person,
/// the past tense or how things stand now.
const STORY: &[&str] = &[
    "currently",
    "seem",
    "seems",
    "for some reason",
    "maybe",
    "perhaps",
    "probably",
    "afaik",
    "guess",
    "was",
    "were",
    "previously",
    "i",
    "i'm",
    "i've",
    "i'll",
    "i'd",
    "my",
    "me",
];

/// Words, in lower case and without their stop, after which a stop ends no
/// sentence.
const ABBREVIATIONS: &[&str] = &[
    "e.g", "i.e", "eg", "ie", "etc", "vs", "cf", "approx", "incl", "esp", "resp", "no", "fig", "al",
];

/// What a code span stands as while a sentence's words are weighed: one word
/// that is neither a marker nor code.
const SPAN: &str = "\u{2026}";

/// A run of lines that may hold sentences, and whether it is a list's item,
/// whose first sentence may start in lower case.
struct Block {
    text: String,
    item: bool,
}

/// The requirement sentences of `request`, in its order, each once and as
/// written but for its spacing.
pub(crate) fn sentences(request: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for block in blocks(request) {
        for (at, sentence) in split(&block.text).into_iter().enumerate() {
            // One starting in lower case goes on from words cut off by code,
            // as "returns" after a snippet does, unless it opens an item.
            let lower = sentence.chars().next().is_some_and(char::is_lowercase);
            if lower && !(block.item && at == 0) {
                continue;
            }
            if states(sentence) && !found.iter().any(|kept| kept == sentence) {
                found.push(sentence.to_owned());
            }
        }
    }
    found
}

/// What sends a run back to check its change against `sentences`, the
/// first `LISTED` of them quoted, asked first for a helper's review when it
/// can `review`; or, where its work is `outputs`, files the request names,
/// to check those with a script of its own.
pub(crate) fn notice(sentences: &[String], review: bool, outputs: &[String]) -> String {
    let brand = crate::BRAND;
    let work = match outputs {
        [] => "your change".to_owned(),
        _ => files(outputs),
    };
    let mut text = format!(
        "[{brand} check] Before ending, check {work} against each requirement the request states:"
    );
    for sentence in sentences.iter().take(LISTED) {
        text.push_str(&format!("\n- {sentence}"));
    }
    if sentences.len() > LISTED {
        let more = sentences.len() - LISTED;
        text.push_str(&format!(
            "\n- and {more} more in the request; check those too."
        ));
    }
    text.push('\n');
    if review {
        text.push_str(REVIEW);
        text.push(' ');
    }
    text.push_str(if outputs.is_empty() {
        CHECK
    } else {
        CHECK_OUTPUT
    });
    text
}

/// `outputs` as a notice names them: "`a`", "`a` and `b`", or the first
/// `NAMED` and how many more.
fn files(outputs: &[String]) -> String {
    let shown: Vec<String> = (outputs.iter().take(NAMED))
        .map(|file| format!("`{file}`"))
        .collect();
    match shown.split_last() {
        Some((last, [])) => last.clone(),
        Some(_) if outputs.len() > NAMED => {
            let more = outputs.len() - NAMED;
            let noun = if more == 1 { "file" } else { "files" };
            format!("{} and {more} more {noun}", shown.join(", "))
        }
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    }
}

/// The files `request` names by a path or a file's name that were changed
/// after `since`, each once and as the request first writes it: a relative
/// path or a name is looked for in `workspace`, an absolute path where it
/// points.
pub(crate) fn outputs(request: &str, workspace: &Path, since: SystemTime) -> Vec<String> {
    let mut looked: Vec<&str> = Vec::new();
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    for word in crate::inputs::words(request).filter(|word| filelike(word)) {
        if looked.contains(&word) {
            continue;
        }
        if looked.len() == LOOKED {
            break;
        }
        looked.push(word);
        // Before looking, which on Windows signs in to a share it names.
        if crate::paths::unsafe_raw(word, Some(workspace)).is_some() {
            continue;
        }
        let path = workspace.join(word);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() || !meta.modified().is_ok_and(|at| at > since) {
            continue;
        }
        // `out.csv`, `./out.csv` and its full path are one file.
        let path = path.canonicalize().unwrap_or(path);
        if !found.iter().any(|(_, seen)| *seen == path) {
            found.push((word.to_owned(), path));
        }
    }
    found.into_iter().map(|(word, _)| word).collect()
}

/// Whether `word` reads as a file's path or name, such as `out/a.csv` or
/// `report.json`, rather than a plain word, a number or a web address.
fn filelike(word: &str) -> bool {
    if word.contains("://") {
        return false;
    }
    let extension = word.rsplit_once('.').map(|(_, extension)| extension);
    word.contains(['/', '\\'])
        || extension.is_some_and(|extension| {
            extension.chars().all(char::is_alphanumeric)
                && extension.chars().any(char::is_alphabetic)
        })
}

/// The blocks of prose in `request`, its lines joined, with code, console
/// sessions, headings, tables and lone tags left out.
fn blocks(request: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut lines: Vec<&str> = Vec::new();
    let mut item = false;
    let mut fence: Option<&str> = None;
    // A console's output runs from its prompt to the next blank line.
    let mut output = false;
    let mut flush = |lines: &mut Vec<&str>, item: &mut bool| {
        if !lines.is_empty() {
            blocks.push(Block {
                text: lines.join(" "),
                item: *item,
            });
        }
        lines.clear();
        *item = false;
    };
    for raw in request.lines() {
        let line = raw.trim();
        if let Some(mark) = fence {
            if line.starts_with(mark) {
                fence = None;
            }
            continue;
        }
        if line.is_empty() {
            output = false;
            flush(&mut lines, &mut item);
            continue;
        }
        if output {
            continue;
        }
        if let Some(mark) = ["```", "~~~"].into_iter().find(|m| line.starts_with(m)) {
            flush(&mut lines, &mut item);
            fence = Some(mark);
            continue;
        }
        if prompt(line) {
            flush(&mut lines, &mut item);
            output = true;
            continue;
        }
        // Four spaces open a code block only where no paragraph is under
        // way, since a list item's next lines are indented too.
        let indented = raw.starts_with('\t') || (raw.starts_with("    ") && lines.is_empty());
        if indented || heading(line) || line.starts_with('|') || tag(line) {
            flush(&mut lines, &mut item);
            continue;
        }
        if let Some(rest) = bullet(line) {
            flush(&mut lines, &mut item);
            item = true;
            lines.push(rest);
            continue;
        }
        // An issue's title has no stop and runs straight into its body.
        if let [first] = lines.as_slice()
            && !item
            && !first.ends_with(['.', '!', '?', ':', ';', ','])
            && line.starts_with(char::is_uppercase)
        {
            flush(&mut lines, &mut item);
        }
        lines.push(line);
    }
    flush(&mut lines, &mut item);
    blocks
}

/// The sentences of `text`, split at a stop followed by a space or the end,
/// outside a code span and not after an abbreviation or before a word in
/// lower case.
fn split(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut sentences = Vec::new();
    let (mut start, mut at) = (0, 0);
    while at < bytes.len() {
        let c = bytes[at];
        if c == b'`' {
            let run = bytes[at..].iter().take_while(|&&b| b == b'`').count();
            let ticks = &text[at..at + run];
            // A span never closed is plain text from its ticks on.
            at += run + text[at + run..].find(ticks).map_or(0, |end| end + run);
            continue;
        }
        let stop = matches!(c, b'.' | b'!' | b'?');
        if stop && bytes.get(at + 1).is_none_or(|next| *next == b' ') {
            let last = text[start..at]
                .split_whitespace()
                .last()
                .unwrap_or_default();
            let last = last.trim_start_matches(['(', '"', '\'']).to_lowercase();
            let next = text[at + 1..].trim_start().chars().next();
            let goes_on = c == b'.'
                && (ABBREVIATIONS.contains(&last.as_str()) || next.is_some_and(char::is_lowercase));
            if !goes_on {
                sentences.push(text[start..=at].trim());
                start = at + 1;
            }
        }
        at += 1;
    }
    let rest = text[start..].trim();
    if !rest.is_empty() {
        sentences.push(rest);
    }
    sentences
}

/// Whether `sentence` states something the change must do, rather than how
/// to work, a question, a story or code.
fn states(sentence: &str) -> bool {
    if sentence.ends_with('?') {
        return false;
    }
    let prose = outside_code(sentence)
        .to_lowercase()
        .replace('\u{2019}', "'");
    let words: Vec<&str> = prose.split_whitespace().collect();
    if words.len() < 3 {
        return false;
    }
    // More than one word in five that reads as code is a snippet, not a sentence.
    if words.iter().filter(|word| codelike(word)).count() * 5 > words.len() {
        return false;
    }
    if PROCESS.iter().any(|phrase| prose.contains(phrase)) {
        return false;
    }
    if STORY.iter().any(|word| holds(&prose, word)) {
        return false;
    }
    let first = words[0].trim_matches(|c: char| !c.is_alphanumeric());
    let condition = CONDITIONS.contains(&first) && prose.contains(',');
    // A sentence that opens with a name in code and goes on with a word says
    // what that name does.
    let named = words[0] == SPAN && words[1].chars().all(char::is_alphabetic);
    IMPERATIVES.contains(&first)
        || condition
        || named
        || MARKERS.iter().any(|marker| holds(&prose, marker))
}

/// `sentence` with each closed code span standing as `SPAN`.
fn outside_code(sentence: &str) -> String {
    let mut prose = String::new();
    let mut rest = sentence;
    while let Some(open) = rest.find('`') {
        let run = rest[open..].bytes().take_while(|&b| b == b'`').count();
        let ticks = &rest[open..open + run];
        let Some(close) = rest[open + run..].find(ticks) else {
            break;
        };
        prose.push_str(&rest[..open]);
        prose.push_str(SPAN);
        rest = &rest[open + run + close + run..];
    }
    prose.push_str(rest);
    prose
}

/// Whether `text` holds `phrase` as whole words, not as part of a name such
/// as `read-only`, `obj.only` or the "i" of "i.e.".
fn holds(text: &str, phrase: &str) -> bool {
    let inside = |c: char| c.is_alphanumeric() || matches!(c, '\'' | '-' | '_');
    let joined = |mut beside: std::str::Chars| match beside.next() {
        Some('.') => beside.next().is_some_and(char::is_alphanumeric),
        next => next.is_some_and(inside),
    };
    text.match_indices(phrase).any(|(at, _)| {
        let before: String = text[..at].chars().rev().take(2).collect();
        !joined(before.chars()) && !joined(text[at + phrase.len()..].chars())
    })
}

/// Whether `word`, outside a code span, reads as code: a path, a call, an
/// assignment, markup or a name with an underscore.
fn codelike(word: &str) -> bool {
    let word = word.trim_matches(['*', ',', '.', ';', ':', '!', '?']);
    let called = word
        .char_indices()
        .any(|(at, c)| c == '(' && word[..at].ends_with(char::is_alphanumeric));
    called
        || word.contains("::")
        || word.contains([
            '=', '/', '\\', '"', '{', '}', '[', ']', '<', '>', '#', '@', '$', '|', '_',
        ])
}

/// Whether `line` opens a console session: a shell's or Python's prompt, or
/// a traceback.
fn prompt(line: &str) -> bool {
    let numbered = line
        .strip_prefix("In [")
        .and_then(|rest| rest.split_once("]:"))
        .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    numbered
        || [">>>", "$ ", "Traceback ("]
            .iter()
            .any(|p| line.starts_with(p))
}

/// Whether `line` is a markdown heading.
fn heading(line: &str) -> bool {
    let marks = line.bytes().take_while(|&b| b == b'#').count();
    marks > 0 && (line.len() == marks || line[marks..].starts_with(' '))
}

/// Whether `line` is a lone tag a task is wrapped in, as `<task>` is.
fn tag(line: &str) -> bool {
    let Some(name) = line.strip_prefix('<').and_then(|l| l.strip_suffix('>')) else {
        return false;
    };
    let name = name.strip_prefix('/').unwrap_or(name);
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// The words of a list's item after its bullet or number.
fn bullet(line: &str) -> Option<&str> {
    if let Some(rest) = ["- ", "* ", "+ "].iter().find_map(|b| line.strip_prefix(b)) {
        return Some(rest.trim_start());
    }
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    let rest = line[digits..]
        .strip_prefix(". ")
        .or_else(|| line[digits..].strip_prefix(") "));
    (1..=3)
        .contains(&digits)
        .then_some(rest)
        .flatten()
        .map(str::trim_start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn sentences_that_state_what_must_hold_are_kept_in_order() {
        let request = "Add a `retry` option to the uploader. It is a nice idea. \
                       Failed uploads should be tried again up to three times. \
                       The client returns the last error when every try fails.";
        assert_eq!(
            sentences(request),
            [
                "Add a `retry` option to the uploader.",
                "Failed uploads should be tried again up to three times.",
                "The client returns the last error when every try fails.",
            ]
        );
    }

    #[test]
    fn instructions_about_how_to_work_are_not_requirements() {
        let request = "You are in a git checkout of the project at /src. Edit the source code in place.\n\n\
                       Do not modify or add test files unless the task requires it. \
                       Make the smallest correct change. When you are done, stop; \
                       your changes are collected from the working tree.\n\n\
                       <task>\nThe parser must reject a trailing comma.\n</task>";
        assert_eq!(
            sentences(request),
            ["The parser must reject a trailing comma."]
        );
    }

    #[test]
    fn code_spans_abbreviations_and_versions_do_not_end_a_sentence() {
        let request = "Wrappers (e.g. `withDefault`) must keep the flag. \
                       Calling `cfg.load()` on v1.2.3 files returns `None`. Values such as \
                       `\"a. B\"` must parse.";
        assert_eq!(
            sentences(request),
            [
                "Wrappers (e.g. `withDefault`) must keep the flag.",
                "Calling `cfg.load()` on v1.2.3 files returns `None`.",
                "Values such as `\"a. B\"` must parse.",
            ]
        );
    }

    #[test]
    fn code_blocks_indented_code_and_console_sessions_are_skipped() {
        let request = "The loader must accept tabs.\n\n\
                       ```\nfn must_return() -> bool { true }\n```\n\n\
                       \tif value: return None\n\n\
                       >>> load('x')\nValueError: it must not fail\n\n\
                       $ run --check\nerror: must not\n\n\
                       It should also accept spaces.";
        assert_eq!(
            sentences(request),
            [
                "The loader must accept tabs.",
                "It should also accept spaces."
            ]
        );
    }

    #[test]
    fn each_list_item_is_a_sentence_of_its_own() {
        let request = "Rules\n\n\
                       - the name must be unique\n\
                       - Empty names are rejected\n\
                       1. Only one default is allowed\n\
                       * **Order:** results are returned in the order given";
        assert_eq!(
            sentences(request),
            [
                "the name must be unique",
                "Empty names are rejected",
                "Only one default is allowed",
                "**Order:** results are returned in the order given",
            ]
        );
    }

    #[test]
    fn lines_broken_inside_a_paragraph_join_into_one_sentence() {
        let request = "When the cache is full the oldest entry\nmust be dropped first.";
        assert_eq!(
            sentences(request),
            ["When the cache is full the oldest entry must be dropped first."]
        );
    }

    #[test]
    fn an_issue_title_is_a_sentence_apart_from_its_body() {
        let request =
            "Export should keep empty rows\nOur report has gaps. Export should keep them.";
        assert_eq!(
            sentences(request),
            ["Export should keep empty rows", "Export should keep them."]
        );
    }

    #[test]
    fn questions_guesses_stories_and_snippets_are_dropped() {
        let request = "Should the cache be shared? It seems the lock should be held longer. \
                       I think it should return early. The old code was only reading half. \
                       Currently the pool cannot grow. \
                       \"DIRS\": os.getenv(\"X\").split(\",\") # should be filter(None, x) here. \
                       Each worker must hold its own lock.";
        assert_eq!(sentences(request), ["Each worker must hold its own lock."]);
    }

    #[test]
    fn conditions_and_what_a_named_api_does_are_requirements() {
        let request = "When the active consumer is cancelled, the next one is promoted. \
                       If the pool is empty the call blocks. \
                       `Channel.close()` cancels all consumers. \
                       `retry` option. \
                       Links in a chain are checked each on its own.";
        assert_eq!(
            sentences(request),
            [
                "When the active consumer is cancelled, the next one is promoted.",
                "`Channel.close()` cancels all consumers.",
                "Links in a chain are checked each on its own.",
            ]
        );
    }

    #[test]
    fn a_sentence_said_twice_is_listed_once() {
        let request = "The flag must default to off.\n\nThe flag must default to off.";
        assert_eq!(sentences(request), ["The flag must default to off."]);
    }

    #[test]
    fn plain_description_is_not_a_requirement() {
        let request = "This module parses dates. Dates come in many shapes. Thanks.";
        assert!(sentences(request).is_empty());
    }

    #[test]
    fn the_notice_quotes_at_most_listed_sentences_and_counts_the_rest() {
        let all: Vec<String> = (1..=LISTED + 3)
            .map(|n| format!("Rule {n} must hold."))
            .collect();
        let text = notice(&all, false, &[]);
        assert!(text.starts_with("[Letronna check] Before ending, check your change"));
        assert!(text.contains(&format!("\n- Rule {LISTED} must hold.\n")));
        assert!(!text.contains(&format!("Rule {} must", LISTED + 1)));
        assert!(text.contains("\n- and 3 more in the request; check those too.\n"));
        assert!(text.ends_with(CHECK) && !text.contains(REVIEW));
        assert!(notice(&all, true, &[]).ends_with(&format!("\n{REVIEW} {CHECK}")));
    }

    #[test]
    fn the_notice_for_outputs_names_them_and_says_to_check_them_with_new_code() {
        let rules = ["Rows must be sorted by date.".to_owned()];
        let outputs = |count: usize| -> Vec<String> {
            (1..=count).map(|n| format!("out/part{n}.csv")).collect()
        };
        let text = notice(&rules, false, &outputs(1));
        assert_eq!(
            text,
            format!(
                "[Letronna check] Before ending, check `out/part1.csv` against each requirement \
                 the request states:\n- Rows must be sorted by date.\n{CHECK_OUTPUT}"
            )
        );
        let two = notice(&rules, true, &outputs(2));
        assert!(
            two.contains("check `out/part1.csv` and `out/part2.csv` against"),
            "{two}"
        );
        assert!(
            two.ends_with(&format!("\n{REVIEW} {CHECK_OUTPUT}")),
            "{two}"
        );
        let many = notice(&rules, false, &outputs(NAMED + 2));
        let shown = "`out/part1.csv`, `out/part2.csv`, `out/part3.csv`, `out/part4.csv`, \
                     `out/part5.csv` and 2 more files against";
        assert!(many.contains(shown), "{many}");
        let one_more = notice(&rules, false, &outputs(NAMED + 1));
        assert!(
            one_more.contains("`out/part5.csv` and 1 more file against"),
            "{one_more}"
        );
    }

    /// Sets when `file` was last changed.
    fn changed_at(file: &Path, at: SystemTime) {
        let file = std::fs::File::options().write(true).open(file).unwrap();
        file.set_modified(at).unwrap();
    }

    #[test]
    fn outputs_are_the_files_a_request_names_that_changed_since_the_run_began() {
        let dir = crate::store::tests::Scratch::new("requirements-outputs");
        let elsewhere = crate::store::tests::Scratch::new("requirements-outputs-elsewhere");
        let since = SystemTime::now();
        let (before, after) = (
            since - Duration::from_secs(3600),
            since + Duration::from_secs(5),
        );
        std::fs::create_dir_all(dir.join("out")).unwrap();
        for (file, at) in [
            ("params.txt", after),
            ("out/table.csv", after),
            ("input.json", before),
            ("scratch.txt", after),
            ("results", after),
        ] {
            std::fs::write(dir.join(file), "made").unwrap();
            changed_at(&dir.join(file), at);
        }
        let summary = elsewhere.join("summary.md");
        std::fs::write(&summary, "made").unwrap();
        changed_at(&summary, after);
        let request = format!(
            "Fit the curve to input.json and save its parameters to **`params.txt`**. Put the \
             table in out/table.csv, a summary at {}, and the results in results. \
             Check ./params.txt and {} once more, and see https://example.com/out/table.csv.",
            summary.display(),
            dir.join("params.txt").display()
        );
        assert_eq!(
            outputs(&request, &dir, since),
            [
                "params.txt".to_owned(),
                "out/table.csv".to_owned(),
                summary.display().to_string(),
            ]
        );
    }
}
