//! What a request says a run must leave behind, and whether it did.
//!
//! A run nobody watches -- `-p`, a schedule, a benchmark's task -- is judged
//! by the files it leaves, and one that is missing or will not parse fails
//! every check that reads it, however good the rest of the work is. A model
//! saying it is done is no proof of that, so a session's own run looks
//! before it ends: the request is read for the files it names under `out/`,
//! with any CSV header or JSON keys it spells out for them, and each file is
//! looked at on disk.
//!
//! Both halves are plain text matching rather than a model's reading, so
//! nothing is invented, and the matching leans to missing: a file counts only
//! where the words the user typed ask for it to be made -- one outside `out/`
//! only by its whole path, right after them -- and one named to read, ask
//! about, move, leave alone or not make, or inside code, a diff or a page
//! quoted in, is no output. What it misses goes unchecked, which costs
//! less than sending a run back for a file nobody asked for: that costs a
//! round, and where only the last message is passed on, as from `-p`, a
//! schedule, a chat app or MCP, the answer given before it unless the model
//! gives it again. So a file that was there as the run began and is as it was
//! is not looked at, and one the request only asks about is never held to
//! what it already held. What looks wrong goes back to the model once, file
//! by file, to weigh against the request, put right only where it asked and
//! end with its whole answer again, since this reading can still be wrong;
//! nothing here edits a file, since rewriting work that was right loses more
//! than it mends.

use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;
use std::time::SystemTime;

/// The kinds of file a request may name as an output: those whose format is
/// checked here, and plain ones, which are checked to be there and not empty.
const KINDS: [&str; 11] = [
    "json", "jsonl", "csv", "md", "txt", "yaml", "yml", "docx", "xlsx", "pptx", "png",
];

/// Words that keep the files a sentence names out of the contract, since it
/// names them to be left for later, left out, taken away or left alone.
const NOT_NOW: [&str; 12] = [
    "must not", "never", "not yet", "not now", "optional", "skip", "delete", "deleted", "remove",
    "removed", "move", "rename",
];

/// Where a sentence turns to examples, so the files it names after them
/// need not be made.
const EXAMPLES: [&str; 3] = ["e.g.", "for example", "for instance"];

/// Words that make a sentence or a list's heading about files to read, so
/// that what it names is input, unless it says to make something too.
const READING: [&str; 3] = ["read", "input", "inputs"];

/// Words that make a sentence ask for files to be made, though it reads some
/// too; one with none of them, naming a file to ask about or explain, asks
/// for nothing.
const MAKING: [&str; 15] = [
    "write", "create", "save", "produce", "update", "generate", "export", "output", "outputs",
    "build", "make", "emit", "store", "put", "deliver",
];

/// Words for changing a file that, like the `MAKING` ones, keep what a
/// sentence names out when they follow "not" or "never".
const CHANGING: [&str; 5] = ["modify", "change", "touch", "overwrite", "edit"];

/// Words said before a making or changing word to forbid it, as in "do not
/// generate" or "told not to write".
const NEGATING: [&str; 7] = [
    "not",
    "don't",
    "dont",
    "never",
    "not to",
    "no need to",
    "forbidden to",
];

/// Words that may come between a `NEGATING` word and the one it forbids, as
/// in "do not ever write" or "do not, under any circumstances, create".
const BETWEEN: [&str; 6] = [
    "",
    "ever ",
    "yet ",
    "even ",
    "try to ",
    "under any circumstances ",
];

/// The making and changing words as "without" and "avoid" take them, as in
/// "without creating"; a bare "without", as in "without a BOM", says how a
/// file is made, not that it is not.
const DOING: [&str; 19] = [
    "creating",
    "writing",
    "saving",
    "producing",
    "updating",
    "generating",
    "exporting",
    "outputting",
    "building",
    "making",
    "emitting",
    "storing",
    "putting",
    "delivering",
    "modifying",
    "changing",
    "touching",
    "overwriting",
    "editing",
];

/// Making words said of something else, as in "it reads out/data.csv and
/// writes out/plot.png": they end a `USING` word's reach, but a sentence with
/// only these says what a thing does and asks for nothing.
const MAKES: [&str; 13] = [
    "writes",
    "creates",
    "saves",
    "produces",
    "updates",
    "generates",
    "exports",
    "builds",
    "makes",
    "emits",
    "stores",
    "puts",
    "delivers",
];

/// Words a sentence may open with before the making word that asks for a
/// file elsewhere, as in "Also create" or "Then write".
const OPENERS: [&str; 8] = [
    "also", "then", "finally", "next", "lastly", "now", "and", "please",
];

/// The most words a making word's object may run to before "to", "as" or
/// "into" leads to a file elsewhere: enough for "the totals for each
/// region", and a longer run is likely a clause about something else.
const OBJECT: usize = 6;

/// The kinds of file a request gives a header for.
const TABLES: [&str; 4] = ["csv", "tsv", "xls", "xlsx"];

/// The kinds of file a request gives keys for.
const DATA: [&str; 4] = ["json", "jsonl", "yaml", "yml"];

/// Words that put a header given in a sentence naming no file in a table of
/// a document, such as a README's, rather than in the CSV named before.
const DOCUMENT: [&str; 3] = ["markdown", "readme", "table whose header row"];

/// Words that take the file named after them as one to use or look over, as
/// in "a test that loads out/fixture.json" or "check out/data.csv and write
/// up what is wrong", though the sentence asks for something to be made.
const USING: [&str; 19] = [
    "read",
    "reads",
    "reading",
    "parse",
    "parses",
    "parsing",
    "load",
    "loads",
    "loading",
    "explain",
    "explains",
    "explaining",
    "check",
    "inspect",
    "look",
    "review",
    "examine",
    "proofread",
    "validate",
];

/// Words before a `USING` one that make it name a thing rather than take a
/// file, as in "the load report", "your review" or "each check".
const THING: [&str; 13] = [
    "the", "a", "an", "your", "my", "our", "their", "its", "his", "her", "each", "every", "per",
];

/// Words that make a sentence describe a file that is there already, so a
/// header or keys it gives are that file's and no output's.
const GIVEN: [&str; 4] = ["input", "inputs", "source", "existing"];

/// Words that make the columns or keys a sentence lists ones to leave out or
/// to sort or group by, as in "Drop the columns: tmp,debug", so they are no
/// output's header or keys.
const NOT_KEPT: [&str; 9] = [
    "drop",
    "exclude",
    "ignore",
    "omit",
    "strip",
    "redact",
    "group by",
    "sort by",
    "not include",
];

/// Bad lines reported for one file: three show the pattern, and more would
/// crowd out the other files.
const LINES: usize = 3;

/// Problems one notice lists, so a run with every file wrong still gets a
/// notice short enough to act on.
const LISTED: usize = 12;

/// The largest file read to check its format, 16 MiB; past that only being
/// there is checked, so a huge output does not hold up the run's end.
const LARGEST: u64 = 16 << 20;

/// What a request asks a run to leave in its workspace.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Contract {
    outputs: Vec<Output>,
}

/// One file a request asks for, and what it spells out about the content.
#[derive(Debug, PartialEq)]
struct Output {
    /// From the workspace, with forward slashes, as `out/report.csv`.
    path: String,
    /// The CSV header the request gives word for word.
    header: Option<String>,
    /// Keys the request says the JSON holds.
    keys: Vec<String>,
    /// The file's size and change time as the request was read, if it was
    /// there then.
    found: Option<Stamp>,
}

/// A file's size and the time it was last changed, where the system keeps one.
type Stamp = (u64, Option<SystemTime>);

/// One CSV record: the line it starts on, counting from 1, and its fields.
struct Record {
    line: usize,
    fields: Vec<String>,
}

/// The files `text` names under `workspace`'s `out/`, in full or as
/// `out/...`, with the header and keys it gives for each.
pub(crate) fn from_request(text: &str, workspace: &Path) -> Contract {
    let lines = typed(text);
    let raw: Vec<&str> = crate::references::typed(text).lines().collect();
    let mut outputs: Vec<Output> = Vec::new();
    // Whether the list going on is under a heading such as "Read:" or "Do
    // not create these yet:", so its items are inputs, which a later round
    // may well have been told to remove, or files for later; or under one
    // such as "Outputs:", so its items are to be made.
    let (mut inputs, mut wanted) = (false, false);
    // An item heading a list of its own that keeps it out, as "- Not yet:"
    // does: how far in it is, and the state to go back to after its list.
    let mut held: Option<(usize, (bool, bool))> = None;
    for line in &lines {
        let trimmed = line.trim();
        let indent = line.len() - line.trim_start().len();
        // A line no further in than that item is past its list.
        if let Some((at, state)) = held
            && !trimmed.is_empty()
            && indent <= at
        {
            (inputs, wanted) = state;
            held = None;
        }
        let item = listed(line);
        let sentences = sentences(line);
        for sentence in &sentences {
            let asked = (item && wanted) || says(sentence, &MAKING);
            if !asked || (item && inputs) || left_out(sentence) {
                continue;
            }
            let lower = sentence.to_ascii_lowercase();
            let example = EXAMPLES.iter().filter_map(|words| lower.find(words)).min();
            let before = &sentence[..example.unwrap_or(sentence.len())];
            // Whether the last of those words so far takes a file to use, so
            // the ones named after it are not made, until a making word.
            let mut using = false;
            let lead = std::iter::once("").chain(words(before));
            for (previous, word) in lead.zip(words(before)) {
                let cue = word.to_lowercase();
                let previous = previous.to_lowercase();
                // After "the", "a" or "your", as in "the load report" or
                // "write your review to", a word that could take a file
                // names a thing.
                let thing = THING.contains(&previous.as_str());
                let uses = USING.contains(&cue.as_str()) && !thing;
                if uses || MAKING.contains(&cue.as_str()) || MAKES.contains(&cue.as_str()) {
                    using = uses;
                    continue;
                }
                // As in "out/a.json but not out/b.json" or "no out/b.json".
                let not = ["not", "no"].contains(&previous.as_str());
                // A path elsewhere is as often an input, or a file code the run
                // writes is to make when it runs, so it counts only where the
                // sentence asks for it outright.
                let elsewhere = || elsewhere(word, workspace).filter(|_| asks_for(before, word));
                let path = output_path(word, workspace).or_else(elsewhere);
                let Some(path) = path.filter(|_| !using && !not) else {
                    continue;
                };
                if !outputs.iter().any(|output| output.path == path) {
                    let found = stamp(&workspace.join(&path));
                    outputs.push(Output {
                        path,
                        header: None,
                        keys: Vec::new(),
                        found,
                    });
                }
            }
        }
        // An indented line goes on with the item above it, under the same
        // heading.
        let nested = line.starts_with(char::is_whitespace);
        // An item heading a list to keep out holds the state off for the
        // lines under it; inside another such list they are out already.
        let heads = item && held.is_none() && trimmed.ends_with(':');
        if heads && sentences.last().is_some_and(|last| holds(last)) {
            held = Some((indent, (inputs, wanted)));
            (inputs, wanted) = (true, false);
        }
        if !item && !nested && !trimmed.is_empty() {
            let heading = sentences.last().filter(|_| trimmed.ends_with(':'));
            inputs = heading.is_some_and(|last| holds(last));
            wanted = heading.is_some_and(|last| {
                let plain = plain(last);
                let names = |word: &str| plain.contains(&format!(" {word}"));
                let making = says(last, &MAKING)
                    || ["output", "deliverable", "artifact"].into_iter().any(names);
                making && !holds(last)
            });
        }
    }
    // A header belongs to the one table its sentence names, or else to the
    // table named last before it, and keys likewise to a data file; and to
    // no output when that one is an input or not a CSV or JSON file, which
    // the request may well describe the same way, nor when the sentence
    // giving them is about a file already there.
    let (mut table, mut data) = (None, None);
    for (at, line) in lines.iter().enumerate() {
        // A line of its own on another output, such as "report.md
        // requirements:", is no longer about the table or data file before
        // it; an item is, since a list may name the table's siblings before
        // its header.
        if !listed(line) {
            for i in words(line).filter_map(|word| named(word, &outputs, workspace)) {
                let other = kind(&outputs[i].path);
                table = table.filter(|_| TABLES.contains(&other.as_str()));
                data = data.filter(|_| DATA.contains(&other.as_str()));
            }
        }
        for word in words(line) {
            let kind = kind(word);
            if TABLES.contains(&kind.as_str()) {
                table = named(word, &outputs, workspace);
            } else if DATA.contains(&kind.as_str()) {
                data = named(word, &outputs, workspace);
            }
        }
        // A sentence naming files, none of them a table, is about those: a
        // header in it is theirs, as one on a report.md item is, and not the
        // table's named before; so too for keys and the data file. One that
        // names no file is about those its line names.
        let files = |text: &str| -> Vec<String> {
            words(text)
                .map(kind)
                .filter(|kind| KINDS.contains(&kind.as_str()) || TABLES.contains(&kind.as_str()))
                .collect()
        };
        let about = |sentence: &str, group: &[&str]| {
            let mut kinds = files(sentence);
            if kinds.is_empty() {
                kinds = files(line);
            }
            kinds.is_empty() || kinds.iter().any(|kind| group.contains(&kind.as_str()))
        };
        // A table in a README or in markdown may go unnamed, so a header in
        // a sentence naming no file but one of those is no CSV's.
        let document = |sentence: &str| files(sentence).is_empty() && says(sentence, &DOCUMENT);
        for sentence in sentences(line).into_iter().filter(|s| !given(s)) {
            if let Some(i) = bound(sentence, &TABLES, table, &outputs, workspace)
                && kind(&outputs[i].path) == "csv"
                && outputs[i].header.is_none()
                && about(sentence, &TABLES)
                && !document(sentence)
            {
                outputs[i].header = header_in(sentence, next_line(&raw, &lines, at));
            }
            if let Some(i) = bound(sentence, &DATA, data, &outputs, workspace)
                && kind(&outputs[i].path) == "json"
                && about(sentence, &DATA)
            {
                outputs[i].keys.extend(keys_in(sentence));
            }
        }
    }
    Contract { outputs }
}

/// What is wrong with each file `contract` asks for, as it stands in
/// `workspace` now, one problem each, naming its file.
pub(crate) fn problems(contract: &Contract, workspace: &Path) -> Vec<String> {
    contract
        .outputs
        .iter()
        .flat_map(|output| {
            let file = workspace.join(&output.path);
            // One there before the run and as it was is as the user left it,
            // and the request may only ask what is wrong with it.
            if output.found.is_some() && stamp(&file) == output.found {
                return Vec::new();
            }
            wrong(output, &file)
        })
        .collect()
}

/// The message that sends a run back to weigh `problems` against its
/// request and to end with its whole answer again, since a request misread
/// here would otherwise leave a note on this check in the answer's place.
pub(crate) fn notice(problems: &[String]) -> String {
    let brand = crate::BRAND;
    let mut text = format!("[{brand} check] Before ending, compare these with the request:");
    for problem in problems.iter().take(LISTED) {
        text.push_str(&format!("\n- {problem}"));
    }
    if problems.len() > LISTED {
        text.push_str(&format!("\n- and {} more.", problems.len() - LISTED));
    }
    text.push_str(
        "\nFix the ones it asked for, in place; ignore any it did not ask for. Only your last message is passed on as your answer, so end with your whole answer again, not only what changed.",
    );
    text
}

/// The records of CSV `text` as Python's csv module reads them, or the line
/// a quote that is never closed opened on.
fn records(text: &str) -> Result<Vec<Record>, usize> {
    // Python would quietly run the rest of the file into one field after a
    // quote never closed, so its line is given instead, to say where to look.
    let mut records = Vec::new();
    let (mut fields, mut field) = (Vec::new(), String::new());
    let (mut line, mut start, mut opened) = (1, 1, 1);
    // `fresh` at a field's first character, where a quote opens quoting.
    let (mut quoted, mut fresh, mut any) = (false, true, false);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                '\n' => {
                    line += 1;
                    field.push(c);
                }
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' if fresh => {
                (quoted, opened, fresh, any) = (true, line, false, true);
            }
            ',' => {
                fields.push(std::mem::take(&mut field));
                (fresh, any) = (true, true);
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\r' => {
                if any {
                    fields.push(std::mem::take(&mut field));
                    let fields = std::mem::take(&mut fields);
                    records.push(Record {
                        line: start,
                        fields,
                    });
                }
                line += 1;
                (start, fresh, any) = (line, true, false);
            }
            _ => {
                field.push(c);
                (fresh, any) = (false, true);
            }
        }
    }
    if quoted {
        return Err(opened);
    }
    if any {
        fields.push(field);
        records.push(Record {
            line: start,
            fields,
        });
    }
    Ok(records)
}

/// Whether `line` is an item of a list, marked or numbered.
fn listed(line: &str) -> bool {
    let trimmed = line.trim_start();
    let number = trimmed.trim_start_matches(|c: char| c.is_ascii_digit());
    let marker = if number.len() < trimmed.len() {
        number.strip_prefix(['.', ')'])
    } else {
        trimmed.strip_prefix(['-', '*', '+'])
    };
    // A space after the mark, so a bold line such as "**Not yet:**" heads
    // a list rather than being an item of the one before.
    marker.is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
}

/// The sentences of `line`, each ended by a full stop, question mark or
/// exclamation mark before a space: a path holds dots, but never one before a
/// space, and "e.g." ends no sentence.
fn sentences(line: &str) -> Vec<&str> {
    let mut sentences = Vec::new();
    let mut start = 0;
    for (at, _) in line.match_indices(['.', '?', '!']) {
        let before = line[..at].to_ascii_lowercase();
        let ends = line[at + 1..].starts_with(' ');
        if !ends || before.ends_with("e.g") || before.ends_with("i.e") {
            continue;
        }
        sentences.push(&line[start..at]);
        start = at + 2;
    }
    sentences.push(&line[start..]);
    sentences
}

/// The lines of `text` the user typed, with none of what `@` attached after
/// them and each line of a fenced block -- code, a file or a page quoted in
/// -- left blank, since the paths there are what it holds, not what is asked.
fn typed(text: &str) -> Vec<&str> {
    let tag = crate::untrusted::TAG;
    let (opening, closing) = (format!("<{tag}-"), format!("</{tag}-"));
    // The mark, backtick or tilde, and how many of it opened the code block
    // going on, if one is, and whether words from outside are.
    let (mut code, mut outside) = (None, false);
    let mut lines = Vec::new();
    for line in crate::references::typed(text).lines() {
        let trimmed = line.trim();
        let mark = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'));
        let run = mark.map_or(0, |mark| {
            trimmed.len() - trimmed.trim_start_matches(mark).len()
        });
        let fenced = if let Some((open, length)) = code {
            // As many of its marks or more, alone, close it: an editor fences
            // a file with more than its text holds, so the text cannot.
            if mark == Some(open) && run >= length && run == trimmed.len() {
                code = None;
            }
            true
        } else if outside {
            outside = !trimmed.starts_with(&closing);
            true
        } else if let Some(mark) = mark.filter(|&mark| run >= 3 && !trimmed[run..].contains(mark)) {
            code = Some((mark, run));
            true
        } else {
            outside = trimmed.starts_with(&opening);
            outside
        };
        lines.push(if fenced { "" } else { line });
    }
    lines
}

/// Whether the files `sentence` names are not for this run to leave: to be
/// left for later, out or alone, not made, or only read.
fn left_out(sentence: &str) -> bool {
    says(sentence, &NOT_NOW) || refused(sentence) || reads(sentence)
}

/// Whether a list's `heading` keeps the files under it out, as a `left_out`
/// one or "Later:" does; only here, since "later" in a sentence may say what
/// comes after a file made now.
fn holds(heading: &str) -> bool {
    left_out(heading) || says(heading, &["later"])
}

/// Whether `sentence` says not to make or change what it names, in any of
/// the words for that.
fn refused(sentence: &str) -> bool {
    let plain = plain(sentence);
    let said = |before: &str, word: &str| {
        BETWEEN
            .iter()
            .any(|between| plain.contains(&format!(" {before} {between}{word} ")))
    };
    let verbs = || MAKING.iter().chain(&CHANGING);
    NEGATING
        .iter()
        .any(|before| verbs().any(|verb| said(before, verb)))
        || ["without", "avoid", "avoiding"]
            .iter()
            .any(|before| DOING.iter().any(|doing| said(before, doing)))
}

/// Whether `text` is about reading files and not about making any.
fn reads(text: &str) -> bool {
    says(text, &READING) && !says(text, &MAKING)
}

/// Whether `sentence` is about a file that is there already rather than one
/// to make, or about columns or keys not to keep, as its words before any
/// list or quote say.
fn given(sentence: &str) -> bool {
    // Not the list itself, which may well hold a column named `source`.
    let lead = sentence.split([':', '`']).next().unwrap_or(sentence);
    left_out(lead) || says(lead, &GIVEN) || says(lead, &NOT_KEPT)
}

/// Whether `text` says any of `words`, each whole.
fn says(text: &str, words: &[&str]) -> bool {
    let plain = plain(text);
    words
        .iter()
        .any(|word| plain.contains(&format!(" {word} ")))
}

/// `text`'s words in lower case, one space apart and one at each end, so a
/// phrase is found only whole: not "delete" in `delete_ids.txt`, nor any
/// word of a path, such as a workspace in `/home/me/output-tools`.
fn plain(text: &str) -> String {
    let text = text.to_lowercase().replace('\u{2019}', "'");
    let words: Vec<&str> = text
        .split_whitespace()
        .filter(|word| !path_like(word))
        .collect();
    let spaced: String = words
        .join(" ")
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || "_'".contains(c) {
                c
            } else {
                ' '
            }
        })
        .collect();
    format!(
        " {} ",
        spaced.split_whitespace().collect::<Vec<_>>().join(" ")
    )
}

/// Whether `word` is a path rather than words joined by a slash, as in
/// "read/write".
fn path_like(word: &str) -> bool {
    let word = word.trim_matches(|c: char| "`'\"()[]<>*,;:!?".contains(c));
    let parts = || word.split(['/', '\\']);
    parts().nth(1).is_some()
        && (word.starts_with(['/', '\\', '~', '.', '$'])
            || parts().any(|part| part.is_empty() || !part.chars().all(char::is_alphabetic)))
}

/// The words of `line` that could be paths or file names, with the
/// markdown and punctuation around them taken off.
fn words(line: &str) -> impl Iterator<Item = &str> {
    line.split(|c: char| c.is_whitespace() || "`\"'()[],;".contains(c))
        .map(|word| {
            word.trim_start_matches('*')
                .trim_end_matches(|c: char| "*:.!?".contains(c))
        })
        .filter(|word| !word.is_empty())
}

/// `word` as a path from the workspace, when it names a file of a known
/// kind under the workspace's `out/`.
fn output_path(word: &str, workspace: &Path) -> Option<String> {
    let word = word.replace('\\', "/");
    let word = word.strip_prefix("./").unwrap_or(&word);
    let path = if word.starts_with("out/") {
        word.to_owned()
    } else {
        let (at, _) = word
            .match_indices("/out/")
            .find(|(at, _)| same_folder(&word[..*at], workspace))?;
        word[at + 1..].to_owned()
    };
    known_file(path)
}

/// `path`, from the workspace, when it names one file of a known kind.
fn known_file(path: String) -> Option<String> {
    // A placeholder such as `<warehouse>` names no one file.
    let placeholder = path.contains(['<', '>', '{', '}', '$', '*', '?']);
    let climbs = path.split('/').any(|part| part.is_empty() || part == "..");
    (!placeholder && !climbs && KINDS.contains(&kind(&path).as_str())).then_some(path)
}

/// `word` as a path from the workspace, when it names a file of a known kind
/// outside `out/` by its whole path: a name alone, or one from a folder, may
/// mean `out/` or a folder named before it, which would be a guess.
fn elsewhere(word: &str, workspace: &Path) -> Option<String> {
    let word = word.replace('\\', "/");
    if !Path::new(&word).is_absolute() {
        return None;
    }
    let path = (word.match_indices('/'))
        .find(|(at, _)| same_folder(&word[..*at], workspace))
        .map(|(at, _)| word[at + 1..].to_owned())?;
    known_file(path).filter(|path| !path.starts_with("out/"))
}

/// Whether `sentence` asks outright for the file `path` names: it opens with
/// a making word, as "Write" or "Also create" do, that takes the path
/// straight after it or after "to", "as" or "into" ending a few words of
/// its own, and it holds no condition, as "when it is called" is.
fn asks_for(sentence: &str, path: &str) -> bool {
    let all: Vec<String> = words(sentence).map(str::to_lowercase).collect();
    let Some(at) = words(sentence).position(|word| word == path) else {
        return false;
    };
    // A list's mark, its number, or a word such as "also" before the verb.
    let opens = all.iter().position(|word| {
        word.chars().any(char::is_alphabetic) && !OPENERS.contains(&word.as_str())
    });
    // A heading such as "Read these to write the outputs:" makes its items
    // asked for though they are read, so a path elsewhere needs its own verb.
    let Some(verb) = opens.filter(|&verb| verb < at) else {
        return false;
    };
    if !MAKING.contains(&all[verb].as_str())
        || says(sentence, &["if", "when", "whenever", "unless"])
    {
        return false;
    }
    let between = &all[verb + 1..at];
    // "Write a.json and b.json" names both.
    if between
        .iter()
        .all(|word| word == "and" || word.contains('/'))
    {
        return true;
    }
    let Some((last, object)) = between.split_last() else {
        return true;
    };
    // "Write a script that saves ..." or "a tool to export ..." asks for
    // what makes the file, not the file.
    let ordinary = |word: &String| {
        word.chars().all(char::is_alphanumeric)
            && ![MAKING.as_slice(), &MAKES, &USING, &CHANGING]
                .iter()
                .any(|kind| kind.contains(&word.as_str()))
            && !["that", "which", "who", "so", "to"].contains(&word.as_str())
    };
    ["to", "as", "into"].contains(&last.as_str())
        && object.len() <= OBJECT
        && object.iter().all(ordinary)
}

/// Whether `given`, as a request writes it, is `workspace`, though either
/// may go through a link, as macOS's /var does to /private/var.
fn same_folder(given: &str, workspace: &Path) -> bool {
    let own = workspace.to_string_lossy().replace('\\', "/");
    if given == own.trim_end_matches('/') {
        return true;
    }
    // Opening a path on Windows can sign in to another computer, and this
    // one comes from the request's text.
    if crate::paths::unsafe_raw(given, Some(workspace)).is_some() {
        return false;
    }
    match (
        std::fs::canonicalize(given),
        std::fs::canonicalize(workspace),
    ) {
        (Ok(given), Ok(own)) => given == own,
        _ => false,
    }
}

/// Which output `word` names, by its path or by its name alone when only one
/// output has that name, or None for any other file, such as an input.
fn named(word: &str, outputs: &[Output], workspace: &Path) -> Option<usize> {
    if let Some(path) = output_path(word, workspace).or_else(|| elsewhere(word, workspace)) {
        return outputs.iter().position(|output| output.path == path);
    }
    if word.contains(['/', '\\']) {
        return None;
    }
    let mut same = outputs
        .iter()
        .enumerate()
        .filter(|(_, output)| output.path.rsplit('/').next() == Some(word));
    match (same.next(), same.next()) {
        (Some((i, _)), None) => Some(i),
        _ => None,
    }
}

/// The output a header or keys in `sentence` belong to: the one file of a
/// `group` kind it names, as in "The header of a.csv is: id,name", or `last`,
/// the one named last on its line or before, when it names none or several.
fn bound(
    sentence: &str,
    group: &[&str],
    last: Option<usize>,
    outputs: &[Output],
    workspace: &Path,
) -> Option<usize> {
    let mut files: Vec<Option<usize>> = words(sentence)
        .filter(|word| group.contains(&kind(word).as_str()))
        .map(|word| named(word, outputs, workspace))
        .collect();
    files.dedup();
    match files[..] {
        [one] => one,
        _ => last,
    }
}

/// The extension of `path`, in lower case, or nothing.
fn kind(path: &str) -> String {
    Path::new(path)
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// Where a header that line `at` leads to with a colon is: the next line not
/// blank, as typed, or the first in a code block opening there.
fn next_line<'a>(raw: &[&'a str], lines: &[&'a str], at: usize) -> Option<&'a str> {
    let next = (at + 1..raw.len()).find(|&i| !raw[i].trim().is_empty())?;
    // Left blank as typed, so the start of a code block or of a page; a
    // header in a block is still the request's, though a path there is not.
    let fence = raw[next].trim_start();
    let opens = lines[next].is_empty() && (fence.starts_with("```") || fence.starts_with("~~~"));
    if opens {
        raw.get(next + 1).copied()
    } else {
        Some(lines[next])
    }
}

/// The header `line` gives word for word, on the line itself or, when it
/// ends in a colon, on `next`.
fn header_in(line: &str, next: Option<&str>) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    let says = ["header", "columns", "first row", "first line"];
    // "Must include" names columns that are needed, not the whole header.
    let partial = lower.contains("include") || lower.contains("contain");
    if partial || !says.iter().any(|words| lower.contains(words)) {
        return None;
    }
    let mut candidates: Vec<&str> = line.split('`').skip(1).step_by(2).collect();
    candidates.extend(line.rsplit_once(':').map(|(_, rest)| rest));
    if line.trim_end().ends_with(':') {
        candidates.extend(next);
    }
    candidates
        .into_iter()
        .map(|candidate| {
            candidate
                .trim()
                .trim_matches(['`', '*'])
                .trim_end_matches('.')
        })
        .find(|candidate| {
            let fields: Vec<&str> = candidate.split(',').collect();
            fields.len() > 1
                && fields.iter().all(|field| {
                    !field.is_empty()
                        && field
                            .chars()
                            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
                })
        })
        .map(str::to_owned)
}

/// The keys `line` lists after "keys", up to its colon, as far as the items
/// are plain keys.
fn keys_in(line: &str) -> Vec<String> {
    let lower = line.to_ascii_lowercase();
    // As a word, so `idempotency_keys:` is not taken for a list of keys.
    let start = lower.match_indices("keys").find(|(at, _)| {
        lower[..*at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_')
    });
    let Some((_, list)) = start.and_then(|(at, _)| line[at..].split_once(':')) else {
        return Vec::new();
    };
    list.split(',')
        .map(|item| {
            let item = item.trim();
            let item = item.strip_prefix("and ").unwrap_or(item);
            item.trim_matches(|c: char| c.is_whitespace() || "`*\"'.".contains(c))
        })
        .take_while(|key| !key.is_empty() && key.chars().all(|c| c.is_alphanumeric() || c == '_'))
        .map(str::to_owned)
        .collect()
}

/// The stamp of `file` as it is now, or None when it is no file.
fn stamp(file: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(file).ok()?;
    meta.is_file().then(|| (meta.len(), meta.modified().ok()))
}

/// What is wrong with `output`, found at `file`.
fn wrong(output: &Output, file: &Path) -> Vec<String> {
    let name = &output.path;
    let size = match std::fs::metadata(file) {
        Ok(meta) if meta.is_file() => meta.len(),
        Ok(_) => return vec![format!("`{name}` is a folder, not a file.")],
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return vec![format!("`{name}` does not exist.")];
        }
        Err(e) => return vec![format!("`{name}` could not be read: {e}.")],
    };
    if size > LARGEST {
        return Vec::new();
    }
    let bytes = match std::fs::read(file) {
        Ok(bytes) => bytes,
        Err(e) => return vec![format!("`{name}` could not be read: {e}.")],
    };
    // A list in plain text or JSON Lines may rightly hold nothing.
    let may_be_empty = matches!(kind(name).as_str(), "txt" | "jsonl");
    if !may_be_empty && bytes.iter().all(u8::is_ascii_whitespace) {
        return vec![format!("`{name}` is empty.")];
    }
    match kind(name).as_str() {
        "json" | "jsonl" | "csv" => text(output, &bytes),
        "docx" => office(name, &bytes, "Word document", "word/document.xml"),
        "xlsx" => office(name, &bytes, "Excel workbook", "xl/workbook.xml"),
        "pptx" => office(name, &bytes, "PowerPoint file", "ppt/presentation.xml"),
        "png" if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") => {
            vec![format!("`{name}` is not a PNG image.")]
        }
        _ => Vec::new(),
    }
}

/// What is wrong with JSON, JSON Lines or CSV `output` holding `bytes`.
fn text(output: &Output, bytes: &[u8]) -> Vec<String> {
    let name = &output.path;
    let Ok(text) = std::str::from_utf8(bytes) else {
        return vec![format!("`{name}` is not UTF-8 text.")];
    };
    let mut problems = Vec::new();
    // Python reads plain UTF-8 unless told otherwise, and then the mark is a
    // character: json.loads refuses it, and csv puts it in the first name.
    let text = match text.strip_prefix('\u{FEFF}') {
        Some(rest) => {
            problems.push(format!(
                "`{name}` starts with a byte order mark, which readers take as part of its text; save it without one."
            ));
            rest
        }
        None => text,
    };
    match kind(name).as_str() {
        "json" => problems.extend(json(output, text)),
        "jsonl" => problems.extend(json_lines(name, text)),
        _ => problems.extend(csv(output, text)),
    }
    problems
}

/// What is wrong with JSON `output` holding `text`.
fn json(output: &Output, text: &str) -> Vec<String> {
    let name = &output.path;
    if text.trim_start().starts_with("```") {
        return vec![format!(
            "`{name}` is wrapped in a ``` fence; it must hold the JSON alone."
        )];
    }
    let value = match parsed(text) {
        Ok(value) => value,
        Err(e) => return vec![format!("`{name}` is not valid JSON: {e}.")],
    };
    // Anywhere in it, since the request may mean each item of a list or an
    // object inside: a key missing everywhere is wrong whichever it meant.
    let mut found = HashSet::new();
    keys_of(&value, &mut found);
    let missing: Vec<String> = output
        .keys
        .iter()
        .filter(|key| !found.contains(key.as_str()))
        .map(|key| format!("`{key}`"))
        .collect();
    if missing.is_empty() {
        return Vec::new();
    }
    vec![format!(
        "`{name}` lacks keys the request names: {}.",
        missing.join(", ")
    )]
}

/// Every key of every object in `value`, at any depth.
fn keys_of<'a>(value: &'a Value, found: &mut HashSet<&'a str>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                found.insert(key);
                keys_of(inner, found);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| keys_of(item, found)),
        _ => {}
    }
}

/// `text` as JSON, with the bare NaN, Infinity and -Infinity that Python's
/// json writes and reads back taken as numbers, or where it goes wrong.
fn parsed(text: &str) -> Result<Value, serde_json::Error> {
    // Retried only when it fails, so a file of plain JSON is read once, and
    // the retry's error is the one given, since it is past any such token.
    serde_json::from_str(text).or_else(|_| serde_json::from_str(&zeroed(text)))
}

/// `text` with each NaN, Infinity and -Infinity outside a string as a 0
/// padded to its width, so an error after it keeps its column.
fn zeroed(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let (mut quoted, mut escaped) = (false, false);
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        let bare = ["NaN", "Infinity", "-Infinity"]
            .into_iter()
            .find(|token| !quoted && rest.starts_with(token));
        if let Some(token) = bare {
            out.push_str(&format!("{:<1$}", 0, token.len()));
            rest = &rest[token.len()..];
            continue;
        }
        if escaped {
            escaped = false;
        } else if quoted && c == '\\' {
            escaped = true;
        } else if c == '"' {
            quoted = !quoted;
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// What is wrong with JSON Lines file `name` holding `text`: its first lines
/// that do not parse.
fn json_lines(name: &str, text: &str) -> Vec<String> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .filter_map(|(at, line)| {
            let e = parsed(line).err()?;
            Some(format!("`{name}` line {} is not valid JSON: {e}.", at + 1))
        })
        .take(LINES)
        .collect()
}

/// What is wrong with CSV `output` holding `text`: a header other than the
/// one asked for, and rows with more or fewer fields than the header.
fn csv(output: &Output, text: &str) -> Vec<String> {
    let name = &output.path;
    let records = match records(text) {
        Ok(records) => records,
        Err(line) => {
            return vec![format!(
                "`{name}` has a quote opened on line {line} that is never closed."
            )];
        }
    };
    let Some((head, rows)) = records.split_first() else {
        return Vec::new();
    };
    let mut problems = Vec::new();
    if let Some(header) = &output.header
        && head.fields.iter().map(String::as_str).ne(header.split(','))
    {
        problems.push(format!(
            "`{name}` has the header `{}` where the request gives `{header}`.",
            head.fields.join(",")
        ));
    }
    let width = head.fields.len();
    let uneven = rows.iter().filter(|row| row.fields.len() != width);
    problems.extend(uneven.take(LINES).map(|row| {
        let count = row.fields.len();
        let hint = if count > width {
            "; a field holding a comma must be in double quotes"
        } else {
            ""
        };
        format!(
            "`{name}` line {} has {count} fields where the header has {width}{hint}.",
            row.line
        )
    }));
    problems
}

/// What is wrong with `name`, which should be a `what`: a zip holding `part`.
fn office(name: &str, bytes: &[u8], what: &str, part: &str) -> Vec<String> {
    if !bytes.starts_with(b"PK\x03\x04") {
        return vec![format!(
            "`{name}` is not a real {what}: it must be a zip file, and it is not one."
        )];
    }
    // A zip stores each name as it is, so a scan finds it with no zip reader.
    if !bytes
        .windows(part.len())
        .any(|window| window == part.as_bytes())
    {
        return vec![format!("`{name}` is not a real {what}: it has no {part}.")];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::Scratch;

    /// The paths `contract` asks for, in order.
    fn paths(contract: &Contract) -> Vec<&str> {
        contract
            .outputs
            .iter()
            .map(|output| output.path.as_str())
            .collect()
    }

    /// What is wrong with the files `request` asks for, after `files` are
    /// written into a workspace of their own once it is read, as a run would.
    fn checked(request: &str, files: &[(&str, &[u8])]) -> Vec<String> {
        let dir = Scratch::new("contract");
        std::fs::create_dir_all(dir.join("out")).unwrap();
        let contract = from_request(request, &dir);
        for (path, bytes) in files {
            std::fs::write(dir.join(path), bytes).unwrap();
        }
        problems(&contract, &dir)
    }

    #[test]
    fn a_path_under_out_named_in_the_request_is_required() {
        let dir = Scratch::new("contract-paths");
        let workspace = dir.display().to_string().replace('\\', "/");
        let request = format!(
            "Read:\n- {workspace}/in/sales.csv\n\nCreate:\n- {workspace}/out/summary.json\n\
             - Write **`out/notes.md`** too, and `out/notes.md` again.\n\
             - Then out/sales_<region>.csv for each region, and /elsewhere/out/x.csv."
        );
        let contract = from_request(&request, &dir);
        assert_eq!(paths(&contract), ["out/summary.json", "out/notes.md"]);
    }

    #[test]
    fn a_file_elsewhere_counts_only_by_its_whole_path_where_asked_outright() {
        let dir = Scratch::new("contract-elsewhere");
        let workspace = dir.display().to_string().replace('\\', "/");
        let request = format!(
            "Also create `{workspace}/db/check_report.md` describing the duplicates.\n\
             - Write {workspace}/notes/a.json and {workspace}/notes/b.json.\n\
             Save the chart as {workspace}/docs/chart.png.\n\
             Write {workspace}/in/audit.json when the exporter runs.\n\
             Write a script that saves {workspace}/data/x.json.\n\
             Add a tool to generate {workspace}/data/y.json.\n\
             Create a summary of {workspace}/in/notes.md in out/summary.md.\n\
             Read these to write the outputs:\n- {workspace}/in/rules.md\n\n\
             Create progress.md and docs/plan.md. Do not write {workspace}/z.json."
        );
        let contract = from_request(&request, &dir);
        assert_eq!(
            paths(&contract),
            [
                "db/check_report.md",
                "notes/a.json",
                "notes/b.json",
                "docs/chart.png",
                "out/summary.md"
            ]
        );
    }

    #[test]
    fn a_file_asked_for_by_its_whole_path_is_checked_as_one_under_out_is() {
        let dir = Scratch::new("contract-elsewhere-checked");
        let workspace = dir.display().to_string().replace('\\', "/");
        let request = format!(
            "Write {workspace}/data/state.json with keys: run, done.\n\
             Also create {workspace}/docs/notes.md."
        );
        let contract = from_request(&request, &dir);
        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::write(dir.join("data/state.json"), br#"{"run": 1}"#).unwrap();
        assert_eq!(
            problems(&contract, &dir),
            [
                "`data/state.json` lacks keys the request names: `done`.",
                "`docs/notes.md` does not exist.",
            ]
        );
    }

    #[test]
    fn a_file_the_request_says_not_to_create_yet_is_not_required() {
        let request = "Create out/state.json and out/partial.json. \
                       Do not create out/final.json or out/ledger.csv yet.\n\
                       out/extra.md is optional.";
        let contract = from_request(request, Path::new("/workspace"));
        assert_eq!(paths(&contract), ["out/state.json", "out/partial.json"]);
    }

    #[test]
    fn a_file_named_to_be_read_removed_or_as_an_example_is_not_required() {
        let request = "Read out/state.json, out/tmp/draft.json and in/notice.json.\n\n\
                       Read:\n- out/partial.json\n\n\
                       Update/create:\n- out/state.json\n- out/report.md\n\
                       - remove out/tmp/draft.json\n\n\
                       Write every spam id to out/delete_ids.txt.\n\
                       Write each reply to out/replies/<id>.txt (e.g. out/replies/002.txt).";
        let contract = from_request(request, Path::new("/workspace"));
        assert_eq!(
            paths(&contract),
            ["out/state.json", "out/report.md", "out/delete_ids.txt"]
        );
    }

    #[test]
    fn json_wrapped_in_a_markdown_fence_is_reported() {
        let problems = checked(
            "Write out/answer.json.",
            &[("out/answer.json", b"```json\n{\"ok\": true}\n```\n")],
        );
        assert_eq!(
            problems,
            ["`out/answer.json` is wrapped in a ``` fence; it must hold the JSON alone."]
        );
    }

    #[test]
    fn a_csv_row_with_an_unquoted_comma_is_reported_with_its_line() {
        let csv =
            b"id,note\n1,\"quoted, so fine\"\n2,\"runs\nover a line\"\n3,not quoted, so wrong\n";
        let problems = checked("Write out/notes.csv.", &[("out/notes.csv", csv)]);
        assert_eq!(
            problems,
            [
                "`out/notes.csv` line 5 has 3 fields where the header has 2; a field holding a comma must be in double quotes."
            ]
        );
    }

    #[test]
    fn a_header_given_in_the_request_must_match_exactly() {
        let request = "Create:\n- out/audit.csv\n- out/rows.csv\n\n\
                       Output columns, in this exact order:\nid,name,amount\n\n\
                       Read in/source.csv, whose header: a,b\n\
                       audit.csv requirements:\n- Exact header:\n  id,name,amount\n";
        let contract = from_request(request, Path::new("/workspace"));
        let headers: Vec<Option<&str>> = contract
            .outputs
            .iter()
            .map(|output| output.header.as_deref())
            .collect();
        // The header after the list is rows.csv's, the CSV named last before
        // it, and the input's header is no output's.
        assert_eq!(headers, [Some("id,name,amount"), Some("id,name,amount")]);

        let wrong = b"id,amount,name\n1,2,x\n";
        let right = b"\"id\",name,amount\n1,x,2\n";
        let problems = checked(
            request,
            &[("out/audit.csv", wrong), ("out/rows.csv", right)],
        );
        assert_eq!(
            problems,
            [
                "`out/audit.csv` has the header `id,amount,name` where the request gives `id,name,amount`."
            ]
        );
    }

    #[test]
    fn json_keys_the_request_names_must_be_somewhere_in_the_file() {
        let request = "Create out/audit.json.\n\
                       audit.json requirements:\n\
                       - Valid JSON with keys: matched, `severity_by_metric`, and reasons.\n\
                       - idempotency_keys: one per item";
        let json = br#"{"matched": [], "reasons": [{"severity": "low"}]}"#;
        let problems = checked(request, &[("out/audit.json", json)]);
        assert_eq!(
            problems,
            ["`out/audit.json` lacks keys the request names: `severity_by_metric`."]
        );
    }

    #[test]
    fn a_docx_that_is_not_a_zip_is_reported() {
        let problems = checked(
            "Save the report as out/report.docx and the chart as out/chart.png.",
            &[
                ("out/report.docx", b"# Report\nWritten as markdown."),
                ("out/chart.png", b"\x89PNG\r\n\x1a\n...."),
            ],
        );
        assert_eq!(
            problems,
            [
                "`out/report.docx` is not a real Word document: it must be a zip file, and it is not one."
            ]
        );
    }

    #[test]
    fn a_missing_empty_or_broken_file_is_named_with_what_is_wrong() {
        let problems = checked(
            "Write out/a.json, out/b.md, out/c.jsonl, out/d.csv and out/e.txt.",
            &[
                ("out/b.md", b"  \n"),
                ("out/c.jsonl", b"{\"ok\": 1}\n{\"ok\": \n"),
                ("out/d.csv", b"\xEF\xBB\xBFid,x\n1,\"open\n"),
                // A list of nothing, which may be right.
                ("out/e.txt", b""),
            ],
        );
        assert_eq!(
            problems,
            [
                "`out/a.json` does not exist.",
                "`out/b.md` is empty.",
                "`out/c.jsonl` line 2 is not valid JSON: EOF while parsing a value at line 1 column 7.",
                "`out/d.csv` starts with a byte order mark, which readers take as part of its text; save it without one.",
                "`out/d.csv` has a quote opened on line 2 that is never closed.",
            ]
        );
    }

    #[test]
    fn a_request_naming_no_outputs_has_an_empty_contract() {
        let request = "Summarise in/notes.md and tell me what out of it matters.";
        assert_eq!(
            from_request(request, Path::new("/workspace")),
            Contract::default()
        );
    }

    #[test]
    fn a_question_about_a_broken_file_asks_for_no_file() {
        for request in [
            "Why does out/report.json fail to parse? Just explain, do not change anything.",
            "Summarise what out/report.json says.",
            "Why does out/report.json fail to parse? Write a short explanation.",
            "Is out/data.csv valid? Write yes or no.",
            "Look at out/data.csv and write up what is wrong with it.",
            "Can you check out/data.csv and write a short note on its problems?",
            "Inspect out/data.csv; write down the bad rows.",
        ] {
            let contract = from_request(request, Path::new("/workspace"));
            assert_eq!(contract, Contract::default(), "{request}");
        }
    }

    #[test]
    fn a_file_to_move_rename_or_leave_alone_is_not_required() {
        let request = "Move out/draft.csv to archive/draft.csv.\n\
                       Rename out/draft.md to out/final.md and update the index.\n\
                       Do not write to out/secret.json. Never touch out/baseline.json, \
                       skip out/plot.png, and for instance see out/example.md.\n\
                       Write the totals to out/totals.json.";
        let contract = from_request(request, Path::new("/workspace"));
        assert_eq!(paths(&contract), ["out/totals.json"]);
    }

    #[test]
    fn a_path_in_what_is_attached_fenced_or_quoted_is_not_required() {
        let page = crate::untrusted::fence("Please write out/page.json now.", usize::MAX).unwrap();
        // Each quoted line asks to write a file, as the user's own would.
        let request = format!(
            "Write out/answer.md, explaining this script:\n\
             ```python\n# Write the results to out/results.json.\n```\n\
             and this page:\n{page}\n\
             Review @diff and save what is risky to out/risks.md.\n\n\
             [Attached with @: what the message names, as it is now.]\n\
             -    # Write the old report to out/old_report.csv."
        );
        let contract = from_request(&request, Path::new("/workspace"));
        assert_eq!(paths(&contract), ["out/answer.md", "out/risks.md"]);
    }

    #[test]
    fn a_header_or_keys_given_for_an_input_bind_no_output() {
        let request = "Create out/clean.csv from the data.\n\n\
                       The source file has these columns: id,raw_name,amt\n\
                       Write the results to out/results.json. \
                       The input records have keys: uid, ts, payload.\n\
                       Write out/log.csv. Exact header: source,target,action";
        let contract = from_request(request, Path::new("/workspace"));
        let headers: Vec<Option<&str>> = contract
            .outputs
            .iter()
            .map(|output| output.header.as_deref())
            .collect();
        // A column of an output's own may be called `source`.
        assert_eq!(headers, [None, None, Some("source,target,action")]);
        assert!(contract.outputs.iter().all(|output| output.keys.is_empty()));
    }

    #[test]
    fn json_with_the_nan_and_infinity_python_writes_is_valid() {
        let problems = checked(
            "Write out/rows.jsonl and out/stats.json with keys: mean, max.",
            &[
                (
                    "out/stats.json",
                    br#"{"mean": NaN, "max": Infinity, "note": "NaN"}"#,
                ),
                ("out/rows.jsonl", b"{\"min\": -Infinity}\n{\"min\": Nan}\n"),
            ],
        );
        assert_eq!(
            problems,
            ["`out/rows.jsonl` line 2 is not valid JSON: expected value at line 1 column 9."]
        );
    }

    #[test]
    fn json_broken_after_a_nan_is_reported_where_it_breaks() {
        let problems = checked(
            "Write out/stats.json.",
            &[("out/stats.json", br#"{"a": NaN, "b": }"#)],
        );
        assert_eq!(
            problems,
            ["`out/stats.json` is not valid JSON: expected value at line 1 column 17."]
        );
    }

    #[test]
    fn a_list_under_a_heading_that_says_not_to_create_it_is_not_required() {
        let request = "Create out/state.json now.\n\n\
                       Do not create these yet:\n- out/final.json\n- out/ledger.csv\n\n\
                       Never write these files:\n- out/secret.json\n\n\
                       Don't create the following yet:\n1. out/later.json\n\n\
                       Outputs (create later, not now):\n- out/final2.json\n\n\
                       Create these in round 2, not now:\n- out/round2.json\n\n\
                       Outputs:\n- out/report.md";
        let contract = from_request(request, Path::new("/workspace"));
        assert_eq!(paths(&contract), ["out/state.json", "out/report.md"]);
    }

    #[test]
    fn a_file_the_request_says_not_to_make_in_other_words_is_not_required() {
        let request = "Create out/a.json. Don't make out/b.json yet.\n\
                       Do not generate out/c.md in this round. Never produce out/d.csv.\n\
                       Do not save anything to out/e.json, and do not overwrite out/f.json.\n\
                       Finish the run without creating out/g.json.\n\
                       Write out/x.csv without a BOM.\n\n\
                       Do not produce these files:\n- out/h.json\n- out/i.csv\n\n\
                       Outputs:\n- out/y.json\n\n\
                       **Do not create these yet:**\n- out/j.json\n\n\
                       Create:\n- out/z.json\n1. Do not create these yet:\n   - out/k.json\n\
                       - **Not yet** (round 2):\n  - out/l.json\n- out/w.json";
        let contract = from_request(request, Path::new("/workspace"));
        assert_eq!(
            paths(&contract),
            [
                "out/a.json",
                "out/x.csv",
                "out/y.json",
                "out/z.json",
                "out/w.json"
            ]
        );
    }

    #[test]
    fn a_header_or_keys_under_another_outputs_heading_bind_no_earlier_file() {
        let request = "Create:\n- out/ledger.csv\n- out/state.json\n- out/report.md\n\n\
                       report.md requirements:\n\
                       - End with a table whose header row is: metric,value\n\
                       - Start with a YAML front-matter block with keys: title, date.";
        let contract = from_request(request, Path::new("/workspace"));
        assert_eq!(
            paths(&contract),
            ["out/ledger.csv", "out/state.json", "out/report.md"]
        );
        assert!(
            contract
                .outputs
                .iter()
                .all(|output| output.header.is_none())
        );
        assert!(contract.outputs.iter().all(|output| output.keys.is_empty()));
    }

    #[test]
    fn a_header_or_keys_given_for_another_file_bind_no_earlier_one() {
        let request = "Create:\n- out/ledger.csv: one row per invoice\n\
                       - out/state.json with keys: run, done\n\
                       - out/notes.txt listing keys: x, y\n\
                       - out/report.md: a markdown table with columns: metric,value, \
                       and YAML front matter with keys: title, date";
        let contract = from_request(request, Path::new("/workspace"));
        assert_eq!(contract.outputs[1].keys, ["run", "done"]);
        let problems = checked(
            request,
            &[
                ("out/ledger.csv", b"invoice,amount\n1,2\n"),
                ("out/state.json", br#"{"run": 1, "done": true}"#),
                ("out/notes.txt", b"x\n"),
                ("out/report.md", b"---\ntitle: t\n---\n"),
            ],
        );
        assert_eq!(problems, Vec::<String>::new());

        for request in [
            "Create out/results.csv. Also update docs/README.md with a table whose header row is: name,value",
            "Create out/results.csv. Also add a table to the README whose header row is: name,value",
        ] {
            let contract = from_request(request, Path::new("/workspace"));
            assert_eq!(contract.outputs[0].header, None, "{request}");
        }
    }

    #[test]
    fn a_file_only_read_by_what_is_made_is_not_required() {
        let request = "Write a function that parses out/data.csv.\n\
                       Create a test that loads out/fixture.json.\n\
                       Write a README explaining what out/report.json contains.\n\
                       Make the plot script handle empty input; it reads out/raw.csv.\n\
                       Read out/state.json and write out/summary.json.";
        let contract = from_request(request, Path::new("/workspace"));
        assert_eq!(paths(&contract), ["out/summary.json"]);
    }

    #[test]
    fn the_load_report_or_what_a_script_writes_is_still_required() {
        let request = "Write the load report to out/load.json.\n\
                       Write the parse errors to out/errors.json and the read counts to out/reads.csv.\n\
                       Make a script; it reads out/data.csv and writes out/plot.png.";
        let contract = from_request(request, Path::new("/workspace"));
        assert_eq!(
            paths(&contract),
            [
                "out/load.json",
                "out/errors.json",
                "out/reads.csv",
                "out/plot.png"
            ]
        );
    }

    #[test]
    fn a_header_in_a_code_block_after_its_colon_is_read() {
        for fence in ["```", "~~~"] {
            let request = format!(
                "Create out/sales.csv with this header:\n{fence}csv\nregion,total\n{fence}"
            );
            let contract = from_request(&request, Path::new("/workspace"));
            assert_eq!(paths(&contract), ["out/sales.csv"], "{fence}");
            assert_eq!(contract.outputs[0].header.as_deref(), Some("region,total"));
        }
    }

    #[test]
    fn a_file_there_before_the_run_is_checked_only_once_it_changes() {
        let dir = Scratch::new("contract-before");
        std::fs::create_dir_all(dir.join("out")).unwrap();
        std::fs::write(dir.join("out/data.csv"), "id,note\n1,a, b\n").unwrap();
        // Loose wording this reading takes for a request to make the file.
        let request = "Go over out/data.csv and write up what is wrong with it.";
        let contract = from_request(request, &dir);
        assert_eq!(paths(&contract), ["out/data.csv"]);
        assert_eq!(problems(&contract, &dir), Vec::<String>::new());

        std::fs::write(dir.join("out/data.csv"), "id,note\n1,a, b, c\n").unwrap();
        assert_eq!(
            problems(&contract, &dir),
            [
                "`out/data.csv` line 2 has 4 fields where the header has 2; a field holding a comma must be in double quotes."
            ]
        );
    }

    #[test]
    fn a_file_left_out_in_more_words_is_not_required() {
        let request = "Create out/a.json but not out/b.json, and out/c.json, not out/d.json.\n\
                       Write out/e.json only; no out/f.json.\n\
                       It is forbidden to write out/g.json. Dont create out/h.json.\n\
                       Do not, under any circumstances, write out/i.json.\n\
                       Do not ever write out/j.json. Do not try to create out/k.json.\n\
                       Finish while avoiding writing out/l.json. No need to create out/m.json.\n\
                       Do not forget to write out/n.json.\n\n\
                       Create:\n- out/o.json\n- Later:\n  - out/p.json\n- out/q.json\n\n\
                       Create these later:\n- out/r.json\n\n\
                       Write out/s.md, explaining:\n~~~\nWrite out/t.json\n~~~";
        let contract = from_request(request, Path::new("/workspace"));
        assert_eq!(
            paths(&contract),
            [
                "out/a.json",
                "out/c.json",
                "out/e.json",
                "out/n.json",
                "out/o.json",
                "out/q.json",
                "out/s.md"
            ]
        );
    }

    #[test]
    fn columns_or_keys_to_drop_or_group_by_are_no_outputs_header_or_keys() {
        let request = "Write out/clean.csv and out/clean.json.\n\
                       Drop the columns: tmp,debug\n\
                       Group by the columns: region,month\n\
                       Exclude the keys: password, ssn\n\
                       Do not include the keys: debug, tmp";
        let contract = from_request(request, Path::new("/workspace"));
        assert_eq!(paths(&contract), ["out/clean.csv", "out/clean.json"]);
        assert_eq!(contract.outputs[0].header, None);
        assert!(contract.outputs[1].keys.is_empty());
    }

    #[test]
    fn a_header_or_keys_bind_to_the_one_file_their_sentence_names() {
        let request = "Write out/a.csv and out/b.csv. The header of a.csv is: id,name. \
                       The header of b.csv is: x,y\n\
                       Write out/a.json and out/b.json. The keys of a.json are: id, name. \
                       b.json has keys: x, y";
        let contract = from_request(request, Path::new("/workspace"));
        let headers: Vec<Option<&str>> = contract
            .outputs
            .iter()
            .map(|output| output.header.as_deref())
            .collect();
        assert_eq!(headers, [Some("id,name"), Some("x,y"), None, None]);
        assert_eq!(contract.outputs[2].keys, ["id", "name"]);
        assert_eq!(contract.outputs[3].keys, ["x", "y"]);
    }
}
