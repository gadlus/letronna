//! Files a request says to leave as they are, and whether a run did.
//!
//! A run nobody watches -- `-p`, a schedule, a benchmark's task -- is judged
//! on the files it was told to leave alone as well as on those it made: a
//! test it must not edit to get green, a validator, the inputs of a batch.
//! Refusing writes to them would also refuse edits the same request asks for
//! elsewhere, and a command changes files no gate sees, so nothing is
//! refused: a session's own run hashes each such file as it starts and,
//! before it ends, names any that changed or went, once, to be put back if
//! the request does say so.
//!
//! A file kept wrongly is one the run is sent back to undo, where a file
//! missed costs only this check, so a file is kept only where the request
//! says plainly not to change it, and nothing is where the reading is
//! unsure. A ban is said in one of four ways: "do not modify `a.py`", with
//! edit, change, touch, alter, delete, rename, move or remove, and "don't",
//! "never", "you must not" or "without modifying"; "`a.py` must not be
//! modified" or "must remain unchanged"; "leave `a.py` unchanged",
//! "untouched", "intact" or "as is"; and "treat `a/` as read-only". What it
//! keeps is said with paths and the few plain words of `PLAIN`, as "any
//! files under `a/`" or "the assertions in the test file `test_a.py`" are;
//! any other word, as "for", "that", "until", "by hand" or the "other" of
//! "other than", and it keeps nothing. Nor does any ban in a sentence that
//! says what may change, with "except", "apart from", "unless" or the like,
//! or "only" or "other" beside a word for changing, and no file such a
//! sentence names is kept at all. A ban ends where its sentence goes on to
//! ask for a change or a read, as in "do not modify `a.py`, just fix
//! `b.py`"; a file a sentence asks to edit, fix, update or write, after the
//! word that asks it, is never kept; and a ban on a folder keeps no file in
//! it the request names anywhere else, other than to read or run. "Do not
//! modify the input files" or "the fixtures", naming none, keeps what the
//! request gives to read in a list's item or a paragraph that asks for no
//! change, and nothing where a sentence asks to update files only by what
//! they are, as "update both outputs" does.
//!
//! So a ban said loosely is missed: "since `a.py` is the spec, do not modify
//! it", "the bug is in `b.py` and `a.py` must not be modified", "do not
//! modify `a.py` or change `b.py`", or a test file in "do not modify the tests
//! in `lib.rs`", which may hold the code to fix as well. "Rewrite", "replace"
//! and "overwrite" make no ban, since "do not overwrite `log.txt`; append to
//! it" leaves edits to make; a ban only on deleting, renaming, moving or
//! removing keeps only a file's being there; and nothing a code block or an
//! `@` attachment holds counts, since a "do not edit" there is a file's own.

use std::ops::Range;
use std::path::{Path, PathBuf};

/// The verbs a ban keeps a file with, as said after "do not", after
/// "without" and after "must not be", each with whether it bans only the
/// file's going, as "do not delete or rename `a.sql`" does.
const VERBS: [(&str, &str, &str, bool); 10] = [
    ("modify", "modifying", "modified", false),
    ("edit", "editing", "edited", false),
    ("change", "changing", "changed", false),
    ("touch", "touching", "touched", false),
    ("alter", "altering", "altered", false),
    ("mutate", "mutating", "mutated", false),
    ("delete", "deleting", "deleted", true),
    ("rename", "renaming", "renamed", true),
    ("move", "moving", "moved", true),
    ("remove", "removing", "removed", true),
];

/// Words that, after "you" and before "not", make a ban, as in "you must not
/// modify"; and before "not be", as in "`a.py` must not be modified".
const MODALS: [&str; 6] = ["must", "should", "shall", "may", "can", "will"];

/// `MODALS` with their "not" in them.
const SHORT: [&str; 5] = ["cannot", "can't", "mustn't", "shouldn't", "won't"];

/// Words that may come between the start of a clause and a ban, as
/// "absolutely" does in "absolutely do not modify".
const LEAD: [&str; 10] = [
    "please",
    "absolutely",
    "also",
    "and",
    "but",
    "so",
    "then",
    "just",
    "simply",
    "strictly",
];

/// Words that, like a comma, end a ban when a verb asking for a change or a
/// read comes next, as "and" does in "do not modify `a.py` and fix `b.py`".
const BREAKS: [&str; 9] = [
    "and", "then", "but", "so", "just", "instead", "while", "when", "also",
];

/// Words that join the items of a list, or the verbs of a ban.
const LISTING: [&str; 3] = ["and", "or", "nor"];

/// The words a ban may say what it keeps in, beside paths: any other leaves
/// the reading unsure, as "for" in "do not modify the tests for `a.py`" does.
const PLAIN: [&str; 46] = [
    "the",
    "a",
    "an",
    "any",
    "all",
    "both",
    "either",
    "each",
    "this",
    "these",
    "those",
    "following",
    "existing",
    "original",
    "provided",
    "given",
    "listed",
    "above",
    "entire",
    "whole",
    "file",
    "files",
    "folder",
    "folders",
    "directory",
    "directories",
    "dir",
    "contents",
    "anything",
    "test",
    "tests",
    "code",
    "assertions",
    "input",
    "inputs",
    "fixture",
    "fixtures",
    "in",
    "of",
    "under",
    "inside",
    "within",
    "and",
    "or",
    "nor",
    "also",
];

/// Words of `PLAIN` that place what follows in what comes before, as "in"
/// does in "any files in `a/`".
const PLACING: [&str; 5] = ["in", "of", "under", "inside", "within"];

/// Words that take the whole of a place after one of `PLACING`, as "files"
/// does in "any files under `a/`".
const HOLDING: [&str; 13] = [
    "file",
    "files",
    "folder",
    "folders",
    "directory",
    "directories",
    "dir",
    "contents",
    "anything",
    "any",
    "all",
    "each",
    "both",
];

/// Words for a part of a file, so a ban on them in a file keeps it only when
/// the file is a test's, as in "the assertions in the test file
/// `test_a.py`", where "the tests in `lib.rs`" may hold the code to fix.
const PARTS: [&str; 4] = ["test", "tests", "code", "assertions"];

/// Words that make a ban naming no file one on what the request gives to
/// read, as in "do not modify input files".
const INPUTS: [&str; 4] = ["input", "inputs", "fixture", "fixtures"];

/// Words that make a sentence say what may change, so it keeps nothing, and
/// no file it names is kept anywhere.
const ALLOW: [&str; 18] = [
    "except",
    "excepting",
    "excepted",
    "exception",
    "exceptions",
    "excluding",
    "exclude",
    "excluded",
    "besides",
    "barring",
    "apart",
    "aside",
    "unless",
    "allowed",
    "permitted",
    "otherwise",
    "writable",
    "editable",
];

/// Words that do as `ALLOW` does in a sentence that speaks of changing, as
/// "other" does in "do not modify files other than `a.py`", where in
/// "include only updates in the window `w.json` gives" they say nothing of
/// what may change.
const ALLOW_IN_CHANGE: [&str; 6] = ["other", "only", "beyond", "outside", "else", "rest"];

/// Words that ask for a file to be changed or made, so a file named after
/// one, in its sentence, is the run's to change.
const ACTING: [&str; 65] = [
    "edit",
    "edits",
    "editing",
    "fix",
    "fixes",
    "fixing",
    "update",
    "updates",
    "updating",
    "write",
    "writes",
    "writing",
    "modify",
    "modifying",
    "change",
    "changes",
    "changing",
    "alter",
    "altering",
    "touch",
    "repair",
    "repairing",
    "patch",
    "patching",
    "rewrite",
    "rewriting",
    "overwrite",
    "replace",
    "create",
    "creating",
    "save",
    "add",
    "adding",
    "append",
    "insert",
    "rename",
    "move",
    "delete",
    "remove",
    "refactor",
    "refactoring",
    "correct",
    "correcting",
    "adjust",
    "implement",
    "implementing",
    "produce",
    "generate",
    "generating",
    "make",
    "complete",
    "finish",
    "fill",
    "migrate",
    "upgrade",
    "bump",
    "convert",
    "extend",
    "record",
    "store",
    "put",
    "emit",
    "revise",
    "revising",
    "amend",
];

/// Words of `ACTING` that ask to change what is already there, where
/// "create" or "write" may ask for files not yet made.
const REVISING: [&str; 16] = [
    "update",
    "updating",
    "edit",
    "editing",
    "modify",
    "modifying",
    "change",
    "changing",
    "fix",
    "fixing",
    "revise",
    "rewrite",
    "overwrite",
    "amend",
    "append",
    "extend",
];

/// Words that give the files after them to read.
const READING: [&str; 14] = [
    "read", "reads", "reading", "load", "loads", "parse", "parses", "open", "review", "inspect",
    "examine", "consult", "use", "using",
];

/// Words that give the files after them to run, which leaves them as they
/// are but makes them no inputs.
const RUNNING: [&str; 6] = ["run", "runs", "running", "execute", "executes", "invoke"];

/// Words that end what a verb before them asks of the files after them, as
/// "so" does in "fix `a.py` so `b.py` passes".
const ENDING: [&str; 3] = ["so", "until", "against"];

/// Words before a verb that undo it, as "not" does in "do not modify".
const NOT: [&str; 10] = [
    "not",
    "don't",
    "dont",
    "never",
    "without",
    "cannot",
    "can't",
    "mustn't",
    "shouldn't",
    "won't",
];

/// Words that make a list's heading one of files to make, beside `ACTING`.
const MAKING: [&str; 4] = ["output", "outputs", "deliverables", "artifacts"];

/// Words that make a list's heading one of files to read.
const HEADS: [&str; 3] = ["read", "input", "inputs"];

/// Files hashed for one request at most, so a folder of thousands holds up
/// neither the run's start nor its end.
const FILES: usize = 2_000;

/// Bytes hashed for one request at most, 256 MiB: SHA-256 gets through that
/// in about a second at worst, where a folder of videos would hold the run up.
const BYTES: u64 = 256 << 20;

/// The files a request says to leave unchanged, as they were when the run began.
#[derive(Debug, Default)]
pub(crate) struct Unchanged {
    files: Vec<Kept>,
}

/// One file to leave unchanged.
#[derive(Debug)]
struct Kept {
    /// As the notice names it: from the workspace, with forward slashes.
    name: String,
    path: PathBuf,
    /// Its SHA-256, in hex, as the run began; None when only its going is banned.
    hash: Option<String>,
}

/// What a ban keeps of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Keep {
    /// That it is still there, as "do not delete or rename" asks.
    Place,
    /// Its bytes, as "do not modify" asks.
    Bytes,
}

/// What a request names, before any of it is looked up.
#[derive(Default)]
struct Named<'a> {
    /// Paths a ban keeps, with what of them.
    kept: Vec<(&'a str, Keep)>,
    /// What a ban naming no file keeps of the files given to read.
    inputs: Option<Keep>,
    /// Paths given to read in a paragraph or an item that asks for no change.
    read: Vec<&'a str>,
    /// Paths given to read in the paragraph or item being sorted.
    reading: Vec<&'a str>,
    /// Whether the paragraph or item being sorted asks for a change.
    changing: bool,
    /// Whether a sentence asks to change files it names by what they are,
    /// as "update both outputs" does, which may be files given to read.
    vague: bool,
    /// Paths a sentence asks to change or make.
    edited: Vec<&'a str>,
    /// Paths a sentence that says what may change names.
    allowed: Vec<&'a str>,
    /// Paths named other than in a ban.
    mentioned: Vec<&'a str>,
    /// Paths given to read or run.
    used: Vec<&'a str>,
    /// Every path, for the folders others may be named from.
    all: Vec<&'a str>,
}

/// What a list's heading makes of its items.
#[derive(Clone, Copy, PartialEq)]
enum Heading {
    Reading,
    Making,
    /// A ban whose items are what it keeps, as "Do not modify these files:".
    Keeping(Keep),
}

/// One ban, as found in a sentence's tokens.
struct Ban<'a> {
    /// The paths it keeps.
    paths: Vec<&'a str>,
    /// Whether it names no path but the input files or the fixtures.
    inputs: bool,
    keep: Keep,
    /// Its tokens, from its first word or its subject's to its end.
    range: Range<usize>,
}

/// What the words of a sentence ask of the paths after them.
#[derive(Clone, Copy, PartialEq)]
enum Asks {
    Nothing,
    Change,
    Use { read: bool },
}

/// The form a ban's verb is in.
#[derive(Clone, Copy)]
enum Form {
    /// "Do not modify".
    Bare,
    /// "Without modifying".
    Ing,
    /// "Must not be modified".
    Done,
}

/// One word of a sentence: a path as written, a plain word in lower case, a
/// comma, dash or bracket, or a colon.
enum Token<'a> {
    Path(&'a str),
    Word(String),
    Comma,
    Colon,
}

/// The files in `workspace` that `text` says to leave unchanged, hashed now.
pub(crate) fn from_request(text: &str, workspace: &Path) -> Unchanged {
    let named = named(text);
    // Each with whether a ban names it, or keeps it as an input.
    let mut kept: Vec<(&str, Keep, bool)> = (named.kept.iter())
        .map(|(path, keep)| (*path, *keep, true))
        .collect();
    // Files named to change only by what they are may be ones given to
    // read, so none of those is taken for an input.
    if let Some(keep) = named.inputs.filter(|_| !named.vague) {
        kept.extend(named.read.iter().map(|path| (*path, keep, false)));
    }
    // Most requests name nothing to keep, and need nothing looked up.
    if kept.is_empty() {
        return Unchanged::default();
    }
    let Ok(workspace) = std::fs::canonicalize(workspace) else {
        return Unchanged::default();
    };
    let mut bases: Vec<PathBuf> = Vec::new();
    for place in named.all.iter().flat_map(|w| places(w, &workspace, &[])) {
        let folder = if place.is_dir() {
            place
        } else {
            place.parent().map(Path::to_owned).unwrap_or(place)
        };
        if !bases.contains(&folder) {
            bases.push(folder);
        }
    }
    let look_up = |words: &[&str]| -> Vec<PathBuf> {
        words
            .iter()
            .flat_map(|word| places(word, &workspace, &bases))
            .collect()
    };
    let edited = look_up(&named.edited);
    let allowed = look_up(&named.allowed);
    let used = look_up(&named.used);
    let loose: Vec<PathBuf> = look_up(&named.mentioned)
        .into_iter()
        .filter(|place| !used.contains(place))
        .collect();
    // A file named to change is the run's to change, and so is one in a
    // folder named to change within the folder kept; a file a ban names in a
    // folder named to change is kept, as said more closely, but an input is
    // not. A place a sentence allows a change in is the run's however
    // named, and a folder kept covers nothing in it named elsewhere other
    // than to read or run.
    let within = |file: &Path, place: &Path, folder: Option<&Path>| {
        folder.is_some_and(|folder| file.starts_with(place) && place.starts_with(folder))
    };
    let free = |file: &Path, folder: Option<&Path>, said: bool| {
        allowed.iter().any(|place| file.starts_with(place))
            || edited.iter().any(|place| {
                place == file || within(file, place, folder) || !said && file.starts_with(place)
            })
            || (loose.iter())
                .any(|place| Some(place.as_path()) != folder && within(file, place, folder))
    };
    let mut files: Vec<(PathBuf, Keep)> = Vec::new();
    for (word, keep, said) in kept {
        // A name two files could have keeps neither, not knowing which.
        let [place] = &places(word, &workspace, &bases)[..] else {
            continue;
        };
        let (found, folder) = if place.is_dir() {
            (files_in(place), Some(place.as_path()))
        } else if place.is_file() {
            (vec![place.clone()], None)
        } else {
            continue;
        };
        for file in found.into_iter().filter(|file| !free(file, folder, said)) {
            // One sentence keeping a file's bytes keeps them, whatever another says.
            match files.iter_mut().find(|(known, _)| *known == file) {
                Some((_, known)) => *known = keep.max(*known),
                None => files.push((file, keep)),
            }
        }
    }
    let mut unchanged = Unchanged::default();
    let mut bytes = 0;
    for (path, keep) in files.into_iter().take(FILES) {
        let hash = match keep {
            Keep::Place => None,
            Keep::Bytes => {
                let Ok(meta) = std::fs::metadata(&path) else {
                    continue;
                };
                if bytes + meta.len() > BYTES {
                    continue;
                }
                bytes += meta.len();
                let Ok(hash) = crate::update::sha256_file(&path) else {
                    continue;
                };
                Some(hash)
            }
        };
        let name = path.strip_prefix(&workspace).unwrap_or(&path);
        let name = name.to_string_lossy().replace('\\', "/");
        unchanged.files.push(Kept { name, path, hash });
    }
    unchanged
}

/// Each file of `unchanged` that is no longer as it was, said in a line of
/// the run's check naming it; the line asks to put it back only if the
/// request does say so, since the reading may have taken an allowed change
/// for a banned one.
pub(crate) fn changed(unchanged: &Unchanged) -> Vec<String> {
    let mut lines = Vec::new();
    for kept in &unchanged.files {
        let what = match &kept.hash {
            None => match std::fs::metadata(&kept.path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => "is gone".to_owned(),
                _ => continue,
            },
            Some(hash) => match crate::update::sha256_file(&kept.path) {
                Ok(now) if &now == hash => continue,
                Ok(_) => "was changed".to_owned(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => "is gone".to_owned(),
                Err(e) => format!("can no longer be read ({e})"),
            },
        };
        let left = if kept.hash.is_some() {
            "as it was"
        } else {
            "in place"
        };
        let name = &kept.name;
        lines.push(format!(
            "`{name}` {what}, though the request seems to say to leave it {left}; if it does, put it back."
        ));
    }
    lines
}

/// The paths `text` names, sorted by what its sentences and lists say of them.
fn named(text: &str) -> Named<'_> {
    let mut named = Named::default();
    let mut heading = None;
    for line in typed(text) {
        let trimmed = line.trim();
        let (body, item) = unmarked(trimmed);
        let indented = line.starts_with(char::is_whitespace);
        // A list's item, and a paragraph, is one place a file given to read
        // may be asked to change, as in "Read `a.json`. Update it."
        if trimmed.is_empty() || item {
            named.inputs_so_far();
        }
        // An indented line says more of the item above it, such as a field
        // of an output, and names no file of the list.
        let under = heading.filter(|_| item && !indented);
        if let Some(Heading::Keeping(keep)) = under {
            let tokens = tokens(body);
            named.all.extend(paths(&tokens));
            // An item of paths and plain words is what the ban keeps; any
            // other is read as a sentence of its own.
            if plain(&tokens) && !paths(&tokens).is_empty() {
                named
                    .kept
                    .extend(paths(&tokens).into_iter().map(|path| (path, keep)));
                continue;
            }
        }
        for sentence in sentences(body) {
            let tokens = tokens(sentence);
            named.all.extend(paths(&tokens));
            let asks = under.filter(|heading| !matches!(heading, Heading::Keeping(_)));
            sort(&tokens, asks, &mut named);
        }
        if item {
            named.inputs_so_far();
        }
        if !item && !indented && !trimmed.is_empty() {
            heading = heading_of(trimmed);
        }
    }
    named.inputs_so_far();
    named
}

impl Named<'_> {
    /// Takes the files given to read since the last paragraph or item ended
    /// as inputs, unless a change was asked for there too.
    fn inputs_so_far(&mut self) {
        let reading = std::mem::take(&mut self.reading);
        if !std::mem::take(&mut self.changing) {
            self.read.extend(reading);
        }
    }
}

/// `trimmed` without the bullet or number that makes it a list's item, and
/// whether it had one.
fn unmarked(trimmed: &str) -> (&str, bool) {
    if let Some(rest) = ["- ", "* ", "+ "]
        .iter()
        .find_map(|bullet| trimmed.strip_prefix(bullet))
    {
        return (rest.trim_start(), true);
    }
    let digits = trimmed.len()
        - trimmed
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .len();
    let rest = &trimmed[digits..];
    match rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
        Some(rest) if digits > 0 => (rest.trim_start(), true),
        _ => (trimmed, false),
    }
}

/// The sentences of `line`: a path holds dots, but never one before a space.
fn sentences(line: &str) -> Vec<&str> {
    let mut sentences = Vec::new();
    let mut start = 0;
    let mut chars = line.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let ends = chars.peek().is_some_and(|(_, next)| next.is_whitespace());
        if ".;!?".contains(c) && ends {
            sentences.push(&line[start..at]);
            start = at + 1;
        }
    }
    sentences.push(&line[start..]);
    sentences
}

/// Sorts the paths of one sentence's `tokens` into `named`, as an item of a
/// list under `heading` when there is one.
fn sort<'a>(tokens: &[Token<'a>], heading: Option<Heading>, named: &mut Named<'a>) {
    // A sentence that says what may change, anywhere in it, is not plain
    // about what may not, and what it names is the run's.
    if allows(tokens) {
        named.allowed.extend(paths(tokens));
        named.changing = true;
        return;
    }
    let bans = bans(tokens);
    for ban in &bans {
        named
            .kept
            .extend(ban.paths.iter().map(|path| (*path, ban.keep)));
        if ban.inputs && ban.paths.is_empty() {
            named.inputs = named.inputs.max(Some(ban.keep));
        }
    }
    let mut asks = match heading {
        Some(Heading::Making) => Asks::Change,
        Some(Heading::Reading) => Asks::Use { read: true },
        _ => Asks::Nothing,
    };
    let mut changes = asks == Asks::Change;
    let mut making = false;
    let mut read = Vec::new();
    for (n, token) in tokens.iter().enumerate() {
        if bans.iter().any(|ban| ban.range.contains(&n)) {
            continue;
        }
        match token {
            Token::Word(word) => {
                let word = word.as_str();
                if ACTING.contains(&word) && !negated(tokens, n) {
                    asks = Asks::Change;
                    changes = true;
                    making = word == "make";
                    named.vague |= REVISING.contains(&word) && vague(&tokens[n + 1..]);
                } else if READING.contains(&word) {
                    asks = Asks::Use { read: true };
                } else if RUNNING.contains(&word) {
                    asks = Asks::Use { read: false };
                } else if ENDING.contains(&word) {
                    asks = Asks::Nothing;
                }
            }
            Token::Path(path) => {
                named.mentioned.push(path);
                // "Make `tests/x.py` pass" runs the test, to change other code.
                let runs = making && test_file(path) && says(&tokens[n..], &["pass", "passes"]);
                match asks {
                    Asks::Change if runs => named.used.push(path),
                    Asks::Change => named.edited.push(path),
                    Asks::Use { read: true } => {
                        named.used.push(path);
                        read.push(*path);
                    }
                    Asks::Use { read: false } => named.used.push(path),
                    Asks::Nothing => {}
                }
            }
            Token::Comma | Token::Colon => {}
        }
    }
    // A file given to read where a change is asked for may be the one to
    // change, as in "read `a.py` and fix the bug in it".
    named.reading.extend(read);
    named.changing |= changes;
}

/// Whether `tokens`, after a word asking for a change, name what to change
/// by what it is rather than by a path, as "both outputs" or "the existing
/// files" do, where "the existing `a.json`" names it.
fn vague(tokens: &[Token]) -> bool {
    const WHAT: [&str; 7] = [
        "both", "them", "existing", "previous", "earlier", "output", "outputs",
    ];
    let words = tokens
        .iter()
        .take_while(|token| matches!(token, Token::Word(_)))
        .count();
    says(&tokens[..words], &WHAT) && !matches!(tokens.get(words), Some(Token::Path(_)))
}

/// The bans in `tokens`, in order.
fn bans<'a>(tokens: &[Token<'a>]) -> Vec<Ban<'a>> {
    let mut found = Vec::new();
    let mut at = 0;
    while at < tokens.len() {
        match ban(tokens, at) {
            Some(ban) => {
                at = ban.range.end.max(at + 1);
                found.push(ban);
            }
            None => at += 1,
        }
    }
    found
}

/// The ban that starts at, or whose verb is at, `at` of `tokens`, if it is
/// said plainly.
fn ban<'a>(tokens: &[Token<'a>], at: usize) -> Option<Ban<'a>> {
    // What it keeps after its verbs: "do not modify `a.py`".
    if let Some((after, form)) = negation(tokens, at) {
        let (from, going) = verbs(tokens, after, form)?;
        let (object, end) = object_end(tokens, from)?;
        return Ban::new(tokens, from..object, at..end, going);
    }
    // What it keeps before them: "`a.py` must not be modified".
    if let Some(after) = passive(tokens, at) {
        let (end, going) = verbs(tokens, after, Form::Done)?;
        let from = subject(tokens, at);
        return Ban::new(tokens, from..at, from..closes(tokens, end)?, going);
    }
    if let Some(end) = staying(tokens, at) {
        let from = subject(tokens, at);
        return Ban::new(tokens, from..at, from..closes(tokens, end)?, false);
    }
    // What it keeps between them: "leave `a.py` unchanged", "treat `a/` as
    // read-only".
    let treat = match word_at(tokens, at) {
        "leave" | "keep" => false,
        "treat" => true,
        _ => return None,
    };
    if !opens(tokens, at) {
        return None;
    }
    let mut end = at + 1;
    let still = loop {
        if let Some(still) = still(tokens, end, treat) {
            break still;
        }
        match tokens.get(end)? {
            Token::Word(word) if !PLAIN.contains(&word.as_str()) => return None,
            _ => end += 1,
        }
    };
    Ban::new(tokens, at + 1..end, at..closes(tokens, end + still)?, false)
}

impl<'a> Ban<'a> {
    /// The ban over `range` of `tokens` that keeps what `object` says, if
    /// that is said in paths and plain words.
    fn new(
        tokens: &[Token<'a>],
        object: Range<usize>,
        range: Range<usize>,
        going: bool,
    ) -> Option<Self> {
        let object = &tokens[object];
        if !plain(object) {
            return None;
        }
        Some(Ban {
            paths: paths(object),
            inputs: says(object, &INPUTS) && !says(object, &["test", "tests"]),
            keep: if going { Keep::Place } else { Keep::Bytes },
            range,
        })
    }
}

/// Where the verbs of a ban start, and in what form, when `at` of `tokens`
/// opens one: "do not", "don't", "never" or "you must not" at the start of a
/// clause, or "without" anywhere.
fn negation(tokens: &[Token], at: usize) -> Option<(usize, Form)> {
    let word = |n: usize| word_at(tokens, n);
    let after = match word(at) {
        "without" => return Some((at + 1, Form::Ing)),
        "don't" | "dont" | "never" => at + 1,
        "do" if word(at + 1) == "not" => at + 2,
        "you" if MODALS.contains(&word(at + 1)) && matches!(word(at + 2), "not" | "never") => {
            at + 3
        }
        "you" if SHORT.contains(&word(at + 1)) => at + 2,
        _ => return None,
    };
    let ever = usize::from(word(after) == "ever");
    opens(tokens, at).then_some((after + ever, Form::Bare))
}

/// Where the verbs of "`a.py` must not be modified" start, when its "must"
/// is at `at` of `tokens`.
fn passive(tokens: &[Token], at: usize) -> Option<usize> {
    let word = |n: usize| word_at(tokens, n);
    match word(at) {
        modal if MODALS.contains(&modal) && matches!(word(at + 1), "not" | "never") => {
            (word(at + 2) == "be").then_some(at + 3)
        }
        modal if SHORT.contains(&modal) => (word(at + 1) == "be").then_some(at + 2),
        "is" | "are" if [word(at + 1), word(at + 2), word(at + 3)] == ["not", "to", "be"] => {
            Some(at + 4)
        }
        _ => None,
    }
}

/// Where "`a.py` must remain unchanged", "should stay untouched", "must be
/// left as is" or "is read-only" ends, when it starts at `at` of `tokens`.
fn staying(tokens: &[Token], at: usize) -> Option<usize> {
    let word = |n: usize| word_at(tokens, n);
    let mut n = at + usize::from(MODALS.contains(&word(at)));
    match word(n) {
        "remain" | "remains" | "stay" | "stays" => n += 1,
        "be" if matches!(word(n + 1), "left" | "kept") => n += 2,
        "is" | "are" | "be" if matches!(word(n + 1), "read-only" | "readonly") => n += 1,
        _ => return None,
    }
    Some(n + still(tokens, n, false)?)
}

/// How many tokens from `at` of `tokens` say a file is to stay as it is, as
/// "unchanged" or "as is" do, or "as read-only" after "treat".
fn still(tokens: &[Token], at: usize, treat: bool) -> Option<usize> {
    let word = |n: usize| word_at(tokens, n);
    if treat {
        let read_only = matches!(word(at + 1), "read-only" | "readonly" | "immutable");
        return (word(at) == "as" && read_only).then_some(2);
    }
    match word(at) {
        "unchanged" | "untouched" | "intact" | "unmodified" | "as-is" | "alone" | "read-only"
        | "readonly" => Some(1),
        "as" if word(at + 1) == "is" => Some(2),
        "as" if matches!([word(at + 1), word(at + 2)], ["it", "is"] | ["they", "are"]) => Some(3),
        _ => None,
    }
}

/// Where the verbs from `at` of `tokens` end, in `form`, and whether each
/// bans only a file's going; None when no verb of a ban is there. Verbs
/// listed together are one ban's, as in "do not delete, rename or modify".
fn verbs(tokens: &[Token], at: usize, form: Form) -> Option<(usize, bool)> {
    let mut going = verb(word_at(tokens, at), form)?;
    let mut end = at + 1;
    loop {
        let mut next = end;
        while matches!(tokens.get(next), Some(Token::Comma))
            || LISTING.contains(&word_at(tokens, next))
        {
            next += 1;
        }
        let Some(more) = verb(word_at(tokens, next), form) else {
            return Some((end, going));
        };
        going &= more;
        end = next + 1;
    }
}

/// Whether `word` is a verb of a ban in `form`, and if so whether it bans
/// only a file's going.
fn verb(word: &str, form: Form) -> Option<bool> {
    VERBS
        .iter()
        .find(|(bare, ing, done, _)| {
            word == match form {
                Form::Bare => *bare,
                Form::Ing => *ing,
                Form::Done => *done,
            }
        })
        .map(|verb| verb.3)
}

/// Where the clause holding `at` of `tokens` starts, for the subject of a
/// ban said after it: after the last colon, as a label's, past any of
/// `LEAD`, as in "**Important**: `a.py` must not be modified".
fn subject(tokens: &[Token], at: usize) -> usize {
    let mut from = (0..at)
        .rev()
        .find(|&n| matches!(tokens[n], Token::Colon))
        .map_or(0, |n| n + 1);
    while from < at && LEAD.contains(&word_at(tokens, from)) {
        from += 1;
    }
    from
}

/// Whether a clause starts at `at` of `tokens`, past any of `LEAD`: at the
/// sentence's start, after a comma or a colon, or after one of `BREAKS`, as
/// in "fix `a.py` and do not modify `b.py`", where "I don't edit `b.py`"
/// says how things are.
fn opens(tokens: &[Token], at: usize) -> bool {
    let mut n = at;
    while n > 0 && LEAD.contains(&word_at(tokens, n - 1)) {
        if BREAKS.contains(&word_at(tokens, n - 1)) {
            return true;
        }
        n -= 1;
    }
    n == 0 || matches!(tokens[n - 1], Token::Comma | Token::Colon)
}

/// Where the object of a ban starting at `from` of `tokens` ends, and where
/// the ban does, past any words that stress it: at the sentence's end, or
/// where another clause starts; None at a word that is not plain, where the
/// reading is unsure.
fn object_end(tokens: &[Token], from: usize) -> Option<(usize, usize)> {
    let mut n = from;
    loop {
        if let Some(end) = closes(tokens, n) {
            return Some((n, end));
        }
        if let Token::Word(word) = &tokens[n]
            && !PLAIN.contains(&word.as_str())
        {
            return None;
        }
        n += 1;
    }
}

/// Where a ban whose last word is before `at` of `tokens` ends, when nothing
/// but its sentence's end or another clause comes next; None otherwise, as
/// for "`a.py` must not be modified by hand".
fn closes(tokens: &[Token], at: usize) -> Option<usize> {
    let n = past_stress(tokens, at);
    (n >= tokens.len() || next_clause(tokens, n)).then_some(n.min(tokens.len()))
}

/// `at` of `tokens`, or past the words there that only stress a ban, as "in
/// any way" or "under any circumstances" do.
fn past_stress(tokens: &[Token], at: usize) -> usize {
    let stresses: [&[&str]; 6] = [
        &["in", "any", "way"],
        &["at", "all"],
        &["whatsoever"],
        &["under", "any", "circumstances"],
        &["for", "any", "reason"],
        &["either"],
    ];
    let said = |words: &[&str]| (0..words.len()).all(|k| word_at(tokens, at + k) == words[k]);
    match stresses.iter().find(|words| said(words)) {
        Some(words) => past_stress(tokens, at + words.len()),
        None => at,
    }
}

/// Whether another clause starts at `at` of `tokens`: a comma, a colon or
/// one of `BREAKS`, then a verb asking for a change or a read, or another
/// ban, as in "do not modify `a.py`, just fix `b.py`".
fn next_clause(tokens: &[Token], at: usize) -> bool {
    let breaks = |n: usize| {
        matches!(tokens.get(n), Some(Token::Comma | Token::Colon))
            || BREAKS.contains(&word_at(tokens, n))
    };
    if !breaks(at) {
        return false;
    }
    let mut n = at;
    while breaks(n) {
        n += 1;
    }
    let word = word_at(tokens, n);
    ACTING.contains(&word)
        || READING.contains(&word)
        || RUNNING.contains(&word)
        || negation(tokens, n).is_some()
}

/// Whether `tokens`, what a ban keeps, are said plainly: in paths and plain
/// words, where a place after one of `PLACING` is taken whole, as in "any
/// files under `a/`", or is a test file of which part is named, as in "the
/// assertions in the test file `test_a.py`".
fn plain(tokens: &[Token]) -> bool {
    (0..tokens.len()).all(|n| {
        let word = word_at(tokens, n);
        if !PLACING.contains(&word) {
            return !matches!(&tokens[n], Token::Word(_)) || PLAIN.contains(&word);
        }
        let before = n.checked_sub(1).map_or("", |n| word_at(tokens, n));
        let places = paths(&tokens[n + 1..]);
        let tests = !places.is_empty() && places.iter().all(|path| test_file(path));
        HOLDING.contains(&before) || PARTS.contains(&before) && tests
    })
}

/// Whether `path` names a test file or a folder of them, by its name.
fn test_file(path: &str) -> bool {
    path.to_lowercase()
        .split(['/', '\\'])
        .any(|part| part.contains("test") || part.contains("spec"))
}

/// Whether `tokens` say what may change, as "except" does, or "other" in a
/// sentence that speaks of changing.
fn allows(tokens: &[Token]) -> bool {
    let changing = tokens.iter().any(|token| match token {
        Token::Word(word) => {
            let word = word.as_str();
            ACTING.contains(&word)
                || VERBS
                    .iter()
                    .any(|(bare, ing, done, _)| [*bare, *ing, *done].contains(&word))
        }
        _ => false,
    });
    says(tokens, &ALLOW) || changing && says(tokens, &ALLOW_IN_CHANGE)
}

/// Whether the `ACTING` word at `n` of `tokens` is undone by a `NOT` before
/// it, past the verbs listed with it, as "remove" is in "do not modify, move
/// or remove".
fn negated(tokens: &[Token], n: usize) -> bool {
    for token in tokens[..n].iter().rev() {
        match token {
            Token::Comma => {}
            Token::Word(word) => {
                let word = word.as_str();
                if NOT.contains(&word) {
                    return true;
                }
                if !(LISTING.contains(&word) || ACTING.contains(&word) || word == "ever") {
                    return false;
                }
            }
            Token::Path(_) | Token::Colon => return false,
        }
    }
    false
}

/// The word at `n` of `tokens`, or nothing for anything else or past their end.
fn word_at<'t>(tokens: &'t [Token], n: usize) -> &'t str {
    match tokens.get(n) {
        Some(Token::Word(word)) => word.as_str(),
        _ => "",
    }
}

/// The paths among `tokens`.
fn paths<'a>(tokens: &[Token<'a>]) -> Vec<&'a str> {
    tokens
        .iter()
        .filter_map(|token| match token {
            Token::Path(path) => Some(*path),
            _ => None,
        })
        .collect()
}

/// Whether `tokens` say any of `words`.
fn says(tokens: &[Token], words: &[&str]) -> bool {
    tokens
        .iter()
        .any(|token| matches!(token, Token::Word(word) if words.contains(&word.as_str())))
}

/// What the line `trimmed`, if it is a heading, makes of the list under it.
fn heading_of(trimmed: &str) -> Option<Heading> {
    let line = trimmed.trim_end_matches('*').strip_suffix(':')?;
    let tokens = tokens(line);
    if allows(&tokens) {
        return None;
    }
    // "Do not modify these files:" or "Must not be modified:", with nothing
    // else said, as "edit the following" would be, or "Read-only files:".
    let lead = |ban: &Ban| (0..ban.range.start).all(|n| LEAD.contains(&word_at(&tokens, n)));
    if let [ban] = &bans(&tokens)[..]
        && ban.paths.is_empty()
        && ban.range.end == tokens.len()
        && lead(ban)
    {
        return Some(Heading::Keeping(ban.keep));
    }
    if matches!(word_at(&tokens, 0), "read-only" | "readonly") && plain(&tokens[1..]) {
        return Some(Heading::Keeping(Keep::Bytes));
    }
    // "Update/create:" is two words here, not a path.
    let lower = line.to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).collect();
    let said = |list: &[&str]| words.iter().any(|word| list.contains(word));
    if said(&ACTING) || said(&MAKING) {
        Some(Heading::Making)
    } else if said(&HEADS) {
        Some(Heading::Reading)
    } else {
        None
    }
}

/// The words, paths, commas and colons of `sentence`, with the markdown and
/// other punctuation around them taken off; a dash or a bracket between
/// words is a comma.
fn tokens(sentence: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let pieces = sentence.split(|c: char| c.is_whitespace() || "`\"[]".contains(c));
    let pieces = pieces.flat_map(|piece| {
        piece
            .split([',', '\u{2013}', '\u{2014}', '(', ')'])
            .enumerate()
    });
    for (n, raw) in pieces {
        if n > 0 && !tokens.is_empty() {
            tokens.push(Token::Comma);
        }
        let word = raw
            .trim_start_matches(['*', '\'', '\u{2018}'])
            .trim_end_matches(|c: char| "*:.;!?'\u{2019}".contains(c));
        // "`a.py`'s tests" names `a.py`, and a part of it, which is no
        // plain word, so a ban on them keeps nothing.
        let owner = ["'s", "\u{2019}s"]
            .iter()
            .find_map(|s| word.strip_suffix(s))
            .filter(|path| path_like(path));
        if let Some(path) = owner {
            tokens.push(Token::Path(path));
            tokens.push(Token::Word("'s".to_owned()));
        } else if path_like(word) {
            tokens.push(Token::Path(word));
        } else {
            let lower = word.to_lowercase().replace('\u{2019}', "'");
            // Hyphens hold "read-only" together, and a dash on its own is no word.
            let parts = lower
                .split(|c: char| !c.is_alphanumeric() && !"'_-".contains(c))
                .map(|part| part.trim_matches('-'));
            tokens.extend(
                parts
                    .filter(|part| !part.is_empty())
                    .map(|part| Token::Word(part.to_owned())),
            );
        }
        if tokens.is_empty() {
            continue;
        }
        if raw.trim_end_matches('*').ends_with(':') {
            tokens.push(Token::Colon);
        } else if matches!(raw, "-" | "--") {
            tokens.push(Token::Comma);
        }
    }
    tokens
}

/// Whether `word` could name a file or folder: it holds a slash, or ends in
/// an extension with a letter in it, which a number such as 0.65 does not.
fn path_like(word: &str) -> bool {
    if word.contains(['/', '\\']) {
        return true;
    }
    let Some((stem, extension)) = word.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && (1..=8).contains(&extension.len())
        && extension.chars().all(|c| c.is_ascii_alphanumeric())
        && extension.chars().any(|c| c.is_ascii_alphabetic())
}

/// The lines of `text` the user typed, without what `@` attached and without
/// the lines of a fenced code block, whose words are a file's and not the
/// request's.
fn typed(text: &str) -> Vec<&str> {
    let mut fenced = false;
    let mut lines = Vec::new();
    for line in crate::references::typed(text).lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced {
            lines.push(line);
        }
    }
    lines
}

/// The files and folders in `workspace`, which is canonical, that `word` may
/// name: in full, or from the workspace or one of `bases`.
fn places(word: &str, workspace: &Path, bases: &[PathBuf]) -> Vec<PathBuf> {
    // Opening a path on Windows can sign in to another computer, and this
    // one comes from the request's text.
    if crate::paths::unsafe_raw(word, Some(workspace)).is_some() {
        return Vec::new();
    }
    let word = word.replace('\\', "/");
    // A glob in the last part, as in `in/sources/*.txt`, stands for its folder.
    let word = match word.rsplit_once('/') {
        Some((folder, last)) if last.contains('*') => folder.to_owned(),
        _ => word,
    };
    // A `~` is the home folder only where a word starts, as a shell has it;
    // inside one it is part of a name, as in Windows' short `RUNNER~1`.
    if word.is_empty()
        || word.starts_with('~')
        || word.contains(['*', '?', '<', '>', '{', '}', '$'])
    {
        return Vec::new();
    }
    let given = Path::new(&word);
    let tries: Vec<PathBuf> = if given.is_absolute() {
        vec![given.to_owned()]
    } else {
        std::iter::once(workspace)
            .chain(bases.iter().map(PathBuf::as_path))
            .map(|base| base.join(given))
            .collect()
    };
    let mut found = Vec::new();
    for path in tries {
        let Ok(real) = std::fs::canonicalize(&path) else {
            continue;
        };
        // The workspace itself is where the run works, never a file to keep.
        if real.starts_with(workspace) && real != workspace && !found.contains(&real) {
            found.push(real);
        }
    }
    found
}

/// The files in `folder` and below, as a search lists them, in name order:
/// hidden and ignored ones are left out, and so are Python's caches, which
/// running the code there rewrites.
fn files_in(folder: &Path) -> Vec<PathBuf> {
    let mut walk = ignore::WalkBuilder::new(folder);
    walk.sort_by_file_name(|a, b| a.cmp(b))
        .filter_entry(|entry| entry.file_name() != "__pycache__");
    walk.build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(ignore::DirEntry::into_path)
        .take(FILES)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::Scratch;

    /// A workspace holding each of `files`, by its path from the workspace.
    fn workspace(name: &str, files: &[&str]) -> Scratch {
        let dir = Scratch::new(&format!("unchanged-{name}"));
        for file in files {
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, format!("the text of {file}")).unwrap();
        }
        dir
    }

    /// The names of the files `text` keeps in `dir`.
    fn kept(text: &str, dir: &Path) -> Vec<String> {
        let unchanged = from_request(text, dir);
        unchanged.files.into_iter().map(|kept| kept.name).collect()
    }

    /// A workspace for the tables: code to fix, tests, inputs and outputs.
    fn project(name: &str) -> Scratch {
        workspace(
            name,
            &[
                "src/app.py",
                "src/util.py",
                "src/client.rs",
                "src/lib.rs",
                "tests/test_app.py",
                "tests/x.py",
                "config.yaml",
                "main.py",
                "notes.txt",
                "Cargo.toml",
                "vendor/lib.rs",
                "in/a.csv",
                "in/batch.json",
                "out/state.json",
            ],
        )
    }

    /// Checks each request of `table` keeps the files beside it in `dir`.
    fn check(table: &[(&str, &[&str])], dir: &Path) {
        for (request, files) in table {
            assert_eq!(kept(request, dir), *files, "{request}");
        }
    }

    #[test]
    fn a_plain_ban_keeps_the_files_it_names() {
        let dir = project("plain");
        let test = &["tests/x.py"][..];
        let config = &["config.yaml"][..];
        let inputs = &["in/a.csv", "in/batch.json"][..];
        check(
            &[
                ("Do not modify `tests/x.py`.", test),
                ("- DO NOT EDIT tests/x.py!", test),
                ("Don\u{2019}t touch tests/x.py.", test),
                ("1. Never change tests/x.py.", test),
                ("You must not modify tests/x.py.", test),
                ("You cannot alter tests/x.py.", test),
                ("Please do not ever modify tests/x.py.", test),
                (
                    "Do not edit tests/x.py or config.yaml.",
                    &["tests/x.py", "config.yaml"],
                ),
                (
                    "Don't touch config.yaml, and do not delete main.py.",
                    &["config.yaml", "main.py"],
                ),
                ("Never edit files in vendor/.", &["vendor/lib.rs"]),
                (
                    "Do not modify anything under `vendor/`.",
                    &["vendor/lib.rs"],
                ),
                (
                    "Do not modify anything under `vendor/`. The vendor/ folder holds a library.",
                    &["vendor/lib.rs"],
                ),
                (
                    "**File Integrity**: Do not delete, rename, or modify any original files in the `in/` directory.",
                    inputs,
                ),
                ("Do not modify tests/x.py in any way.", test),
                (
                    "Do not modify `tests/x.py`, `tests`, or `config.yaml`.",
                    &["tests/x.py", "config.yaml"],
                ),
                ("Do not modify the test file (`tests/x.py`).", test),
                ("Do not modify tests/x.py but read it carefully.", test),
                ("Fix src/app.py without modifying tests/x.py.", test),
                ("`tests/x.py` must not be modified.", test),
                (
                    "**Important**: config.yaml should never be changed.",
                    config,
                ),
                ("config.yaml is not to be edited.", config),
                (
                    "The test files tests/x.py and tests/test_app.py must not be modified.",
                    &["tests/x.py", "tests/test_app.py"],
                ),
                ("config.yaml must remain unchanged.", config),
                ("config.yaml must be left as is.", config),
                ("`in/` is read-only.", inputs),
                ("Leave config.yaml untouched.", config),
                ("Keep config.yaml intact, edit main.py.", config),
                (
                    "Leave `vendor/` as it is and update src/client.rs.",
                    &["vendor/lib.rs"],
                ),
                ("Treat `in/` as read-only.", inputs),
                (
                    "Do not modify the following files:\n- tests/x.py\n- config.yaml",
                    &["tests/x.py", "config.yaml"],
                ),
                ("Read-only:\n- `in/`", inputs),
                // A heading that goes on to ask for a change keeps nothing.
                ("Do not touch these, edit the following:\n- tests/x.py", &[]),
            ],
            &dir,
        );
        // The same file a test is run from, named as the run would name it.
        let request = "Fix the bug in src/app.py and run `pytest tests/test_app.py`.\n\
                       - **No cheating**: Absolutely do not modify any code or assertions in the test file `test_app.py`.";
        assert_eq!(kept(request, &dir), ["tests/test_app.py"]);
    }

    #[test]
    fn a_ban_ends_where_its_sentence_asks_for_a_change() {
        let dir = project("ends");
        let test = &["tests/x.py"][..];
        let tests = &["tests/test_app.py"][..];
        let none = &[][..];
        check(
            &[
                ("Do not modify tests/x.py, fix src/app.py instead.", test),
                (
                    "Fix the bug in src/app.py and do not modify tests/x.py.",
                    test,
                ),
                (
                    "Fix src/app.py and never edit files in vendor/.",
                    &["vendor/lib.rs"],
                ),
                (
                    "Fix src/app.py and leave config.yaml untouched.",
                    &["config.yaml"],
                ),
                ("Do not modify tests/x.py; fix src/app.py.", test),
                (
                    "Do not modify tests/x.py and fix the bug in src/app.py.",
                    test,
                ),
                ("Do not modify tests/x.py, just fix src/app.py.", test),
                ("Do not modify tests/x.py while fixing src/app.py.", test),
                ("Don't modify tests/x.py\u{2014}fix src/app.py.", test),
                ("Don't touch the tests - edit src/app.py.", none),
                (
                    "Never edit files in vendor/ -- instead, update src/client.rs.",
                    &["vendor/lib.rs"],
                ),
                (
                    "`tests/x.py` must not be modified or deleted, fix src/app.py.",
                    test,
                ),
                (
                    "Leave config.yaml unchanged and update main.py.",
                    &["config.yaml"],
                ),
                // A file read before the change is asked for is still kept.
                (
                    "Read notes.txt and write what it says to out/state.json. Do not modify `notes.txt`.",
                    &["notes.txt"],
                ),
                (
                    "Read tests/test_app.py and fix src/app.py so it passes. Do not modify tests/test_app.py.",
                    tests,
                ),
                (
                    "Run tests/test_app.py and fix src/app.py so that it passes. tests/test_app.py must not be modified.",
                    tests,
                ),
                (
                    "Read tests/test_app.py, then update src/app.py until it passes. Do not modify tests/test_app.py.",
                    tests,
                ),
                (
                    "Load config.yaml and update main.py to use it. Do not change config.yaml.",
                    &["config.yaml"],
                ),
                (
                    "Read in/a.csv and write a summary of it to out/state.json. Do not modify in/a.csv.",
                    &["in/a.csv"],
                ),
                (
                    "Fix src/app.py so tests/test_app.py passes. Do not modify tests/test_app.py.",
                    tests,
                ),
                (
                    "Make `python tests/test_app.py` pass. Do not edit `tests/test_app.py`.",
                    tests,
                ),
                // Where "make" asks to change the file after it, it does.
                (
                    "Make src/app.py pass the tests. Do not modify src/app.py or tests/x.py.",
                    &["tests/x.py"],
                ),
            ],
            &dir,
        );
    }

    #[test]
    fn a_file_the_request_allows_to_change_is_never_kept() {
        let dir = project("allows");
        let none = &[][..];
        let rest = &["src/client.rs", "src/lib.rs", "src/util.py"][..];
        check(
            &[
                ("Do not modify any files other than `src/app.py`.", none),
                (
                    "Make the tests pass. Do not modify any file other than src/app.py.",
                    none,
                ),
                ("Do not modify any other files than src/app.py.", none),
                ("Do not modify anything apart from `src/app.py`.", none),
                ("Do not touch any file aside from src/app.py.", none),
                ("Do not change any files except `src/app.py`.", none),
                ("Do not modify any files excluding src/app.py.", none),
                ("Do not modify anything besides src/app.py.", none),
                ("Do not modify anything but `src/app.py`.", none),
                ("Do not modify anything not in `src/`.", none),
                ("Do not modify any file that is not src/app.py.", none),
                ("Do not modify anything beyond `src/app.py`.", none),
                ("Do not modify anything outside `out/`.", none),
                ("Do not modify anything other than `out/`.", none),
                (
                    "Do not modify anything with the exception of src/app.py.",
                    none,
                ),
                (
                    "With the exception of src/app.py, do not modify any files.",
                    none,
                ),
                ("Do not modify any file (src/app.py excepted).", none),
                ("Do not modify any file (only src/app.py may change).", none),
                ("Leave every file other than src/app.py unchanged.", none),
                ("Files other than src/app.py must not be modified.", none),
                ("Everything except `out/` must stay unchanged.", none),
                ("Everything but `out/` must stay unchanged.", none),
                ("Do not modify src/, except src/app.py.", none),
                (
                    "Do not modify `src/` (but `src/app.py` is yours to fix).",
                    none,
                ),
                (
                    "Do not modify Cargo.toml other than bumping the version.",
                    none,
                ),
                (
                    "`Cargo.toml` must not be changed except for the version.",
                    none,
                ),
                (
                    "Never modify tests/x.py unless the test itself is wrong.",
                    none,
                ),
                ("Do not modify tests/x.py, except for adding imports.", none),
                (
                    "Fix the parse function. Do not modify anything else in src/app.py.",
                    none,
                ),
                (
                    "Only change `parse`; do not modify the rest of src/app.py.",
                    none,
                ),
                // A file the request says may change, or names to change,
                // is left out of a folder it keeps.
                (
                    "Do not modify src/. The only exception is src/app.py.",
                    rest,
                ),
                ("Do not modify src/. src/app.py has the bug.", rest),
                ("Do not modify src/. Fix src/app.py's parser.", rest),
                ("Leave `src/` untouched. `src/app.py` is writable.", rest),
                (
                    "Do not modify anything under `src/`. Repair `src/app.py`.",
                    rest,
                ),
                ("Do not touch src/ and fix src/app.py.", rest),
                (
                    "Do not edit src/app.py or src/util.py.\nUpdate `src/util.py` to log its errors.",
                    &["src/app.py"],
                ),
                // A file named to keep in a folder named to change is kept,
                // as said more closely.
                (
                    "Fix the bug in src/. Do not touch src/util.py.",
                    &["src/util.py"],
                ),
            ],
            &dir,
        );
    }

    #[test]
    fn a_ban_said_loosely_keeps_nothing() {
        let dir = project("loosely");
        for request in [
            // The file to fix stands before a ban on something else.
            "The bug is in src/app.py, you should not modify the tests.",
            "Since src/app.py is buggy, you must not modify the tests.",
            "src/app.py has the bug, you can't touch the tests.",
            "Limit your edits to src/app.py, do not touch the tests.",
            "Make src/app.py pass the tests, but do not modify the tests.",
            // Or after one, in a clause of its own.
            "Do not modify the tests, and src/app.py must pass them.",
            "Don't modify tests/test_app.py and src/app.py should be fixed.",
            "Do not modify tests/test_app.py, which imports src/app.py.",
            "Don't touch tests/test_app.py, the bug is in src/app.py.",
            "Do not modify the tests (the bug is in src/app.py).",
            "Do not modify the tests as the bug is in src/app.py.",
            "Do not modify the tests because the bug lives in src/app.py.",
            "Do not modify the tests to make src/app.py pass.",
            "Do not change any test expectations in order to make src/app.py pass.",
            // Or as what the ban is about.
            "Do not modify the tests for src/app.py.",
            "Do not modify the tests that cover src/app.py.",
            "Do not modify tests/test_app.py which tests src/app.py.",
            "Do not modify the tests in src/lib.rs.",
            "Do not modify the tests in tests/x.py or src/app.py.",
            "Fix the parser without changing the signature of parse in src/lib.rs.",
            "Fix the bug without changing the public interface of src/app.py.",
            "Do not change the column names in in/a.csv.",
            "Leave src/app.py's tests unchanged.",
            // A ban on a manner or a time, or one that leaves edits to make.
            "Do not edit src/app.py by hand.",
            "Do not modify tests/x.py until it fails.",
            "Do not overwrite out/state.json; append to it.",
            "Do not rewrite src/app.py from scratch.",
            "Do not replace config.yaml.",
            "tests/x.py must not be modified by your patch.",
            // A reminder, or a ban on leaving a file as it is.
            "Don't forget to change the version in Cargo.toml.",
            "Don't hesitate to modify src/app.py if needed.",
            "src/app.py should not be left unchanged.",
            "Don't leave src/app.py unchanged.",
            "Keep src/app.py working and leave the rest untouched.",
            "I don't edit config.yaml often; refactor main.py.",
            // A ban whose object is only a pronoun.
            "Since tests/x.py is the spec, do not modify it.",
            "Look at tests/x.py, but do not modify it.",
            "`tests/x.py`: do not modify.",
            // Two clauses with nothing plain between them.
            "The bug is in src/app.py and tests/test_app.py must not be modified.",
            "Do not modify tests/x.py or change config.yaml.",
            "Treat `in/` as read-only and `out/` as writable.",
        ] {
            assert!(kept(request, &dir).is_empty(), "{request}");
        }
    }

    #[test]
    fn a_ban_on_modifying_the_inputs_covers_the_files_listed_to_read() {
        let dir = workspace(
            "inputs",
            &[
                "in/batch.json",
                "in/rules.md",
                "in/updates/1.json",
                "out/state.json",
            ],
        );
        std::fs::create_dir_all(dir.join("in/updates/__pycache__")).unwrap();
        std::fs::write(dir.join("in/updates/__pycache__/x.pyc"), "cache").unwrap();
        let request = "Read:\n\
                       - in/batch.json\n\
                       - in/rules.md\n\
                       - out/state.json\n\
                       - JSON files appearing under in/updates/\n\
                       \n\
                       Create:\n\
                       - out/results.json\n\
                       - update out/state.json\n\
                       \n\
                       Requirements:\n\
                       - Do not modify input files. Do not use network access.";
        assert_eq!(
            kept(request, &dir),
            ["in/batch.json", "in/rules.md", "in/updates/1.json"]
        );
        let fixtures = request.replace("modify input files", "modify fixtures");
        assert_eq!(kept(&fixtures, &dir).len(), 3);
        // Files only listed to read, with no word against changing them, are not kept.
        assert!(kept(&request.replace("Do not modify input files. ", ""), &dir).is_empty());
        // Nor are they where the ban is on the input's format, not its files,
        // or on the test fixtures.
        let format = request.replace("modify input files", "change the input format");
        assert!(kept(&format, &dir).is_empty());
        let tests = request.replace("modify input files", "modify the test fixtures");
        assert!(kept(&tests, &dir).is_empty());
        // Nor is a file given to read in a sentence that asks for a change,
        // or to run.
        for request in [
            "Read in/rules.md and fix the typo in it. Do not modify the input files.",
            "Read in/rules.md and fix the typo. Do not modify the input files.",
            "Load in/batch.json, then patch the timeout value. Leave the fixtures untouched.",
            "Run `python in/rules.md` and make it pass. Do not modify input files.",
        ] {
            assert!(kept(request, &dir).is_empty(), "{request}");
        }
        let read = "Read in/rules.md and in/batch.json. Do not modify the input files.";
        assert_eq!(kept(read, &dir), ["in/rules.md", "in/batch.json"]);
        // A paragraph that asks for a change, in any sentence, gives no
        // inputs, nor does a file to update or in a folder to clean.
        let outputs = "Read the existing out/state.json, then read in/batch.json. Update both \
                       outputs.\n\nDo not modify input files.";
        assert!(kept(outputs, &dir).is_empty());
        let apart = "Read:\n- out/state.json\n- in/batch.json\n\nUpdate the existing outputs.\n\n\
                     Do not modify input files.";
        assert!(kept(apart, &dir).is_empty());
        let named = apart.replace("existing outputs", "existing out/state.json");
        assert_eq!(kept(&named, &dir), ["in/batch.json"]);
        let update = "Read in/batch.json and out/state.json.\n\nUpdate/create:\n- out/state.json\n\n\
                      Do not modify input files.";
        assert_eq!(kept(update, &dir), ["in/batch.json"]);
        let clean = "Read in/batch.json and in/updates/1.json.\n\nRemove the files under \
                     in/updates/.\n\nDo not modify input files.";
        assert_eq!(kept(clean, &dir), ["in/batch.json"]);
    }

    #[test]
    fn a_file_named_from_the_folder_it_is_run_in_is_found() {
        let dir = workspace(
            "protects",
            &["in/app/config_manager.py", "in/app/test_config.py"],
        );
        let request = format!(
            "Fix the bug in `{0}/in/app/config_manager.py`.\n\
             Run `pytest {0}/in/app/test_config.py`.\n\
             - **No cheating**: Absolutely do not modify any code or assertions in the test file `test_config.py`.",
            dir.display()
        );
        assert_eq!(kept(&request, &dir), ["in/app/test_config.py"]);
        let request = format!(
            "Run `python tools/check.py` from `{}/in/app` and make it pass.\n\
             Do not modify `tools/check.py`.",
            dir.display()
        );
        std::fs::create_dir_all(dir.join("in/app/tools")).unwrap();
        std::fs::write(dir.join("in/app/tools/check.py"), "print('ok')").unwrap();
        assert_eq!(kept(&request, &dir), ["in/app/tools/check.py"]);
    }

    #[test]
    fn a_tilde_inside_a_folder_s_name_is_part_of_it_and_only_one_in_front_is_home() {
        // As Windows names a folder in its short form, `RUNNER~1`.
        let dir = workspace("short", &["RUNNER~1/in/app/test_config.py"]);
        let request = format!(
            "Run `pytest {}/RUNNER~1/in/app/test_config.py`.\n\
             Do not modify the test file `test_config.py`.",
            dir.display()
        );
        assert_eq!(kept(&request, &dir), ["RUNNER~1/in/app/test_config.py"]);
        let home = "Do not modify `~/RUNNER~1/in/app/test_config.py`.";
        assert!(kept(home, &dir).is_empty());
    }

    #[test]
    fn what_a_code_block_or_another_rule_says_keeps_nothing() {
        let dir = workspace(
            "loose",
            &["gen/api.rs", "README.md", "out/report.md", "src/app.py"],
        );
        let block = "Explain this file.\n```rust\n// Do not edit gen/api.rs by hand.\n```";
        assert!(kept(block, &dir).is_empty());
        let block = "Explain this file.\n```\nDo not edit gen/api.rs.\n```";
        assert!(kept(block, &dir).is_empty());
        let unrelated = "Do not run `docker compose up`, and edit README.md to say so.\n\
                         out/report.md must have sections named Added, Changed and Unchanged.";
        assert!(kept(unrelated, &dir).is_empty());
    }

    #[test]
    fn a_kept_file_that_changed_or_went_is_named() {
        let dir = workspace("changed", &["in/a.csv", "in/b.csv", "in/c.csv"]);
        let unchanged = from_request("Do not modify in/a.csv, in/b.csv or in/c.csv.", &dir);
        assert!(changed(&unchanged).is_empty());
        std::fs::write(dir.join("in/a.csv"), "x,y\n").unwrap();
        std::fs::remove_file(dir.join("in/b.csv")).unwrap();
        // Written again with the same bytes, it is as it was.
        std::fs::write(dir.join("in/c.csv"), "the text of in/c.csv").unwrap();
        let says =
            "though the request seems to say to leave it as it was; if it does, put it back.";
        assert_eq!(
            changed(&unchanged),
            [
                format!("`in/a.csv` was changed, {says}"),
                format!("`in/b.csv` is gone, {says}")
            ]
        );
    }

    #[test]
    fn a_file_banned_only_from_going_may_change_but_not_go() {
        let dir = workspace("going", &["migrations/001.sql", "in/a.csv"]);
        let request = "Do not delete or rename migrations/001.sql; update it to add the column.\n\
                       Do not modify or delete in/a.csv.";
        let unchanged = from_request(request, &dir);
        std::fs::write(dir.join("migrations/001.sql"), "ALTER TABLE t ADD c;").unwrap();
        std::fs::write(dir.join("in/a.csv"), "x,y\n").unwrap();
        let bytes =
            "though the request seems to say to leave it as it was; if it does, put it back.";
        assert_eq!(
            changed(&unchanged),
            [format!("`in/a.csv` was changed, {bytes}")]
        );
        std::fs::remove_file(dir.join("migrations/001.sql")).unwrap();
        let place =
            "though the request seems to say to leave it in place; if it does, put it back.";
        assert_eq!(
            changed(&unchanged),
            [
                format!("`migrations/001.sql` is gone, {place}"),
                format!("`in/a.csv` was changed, {bytes}")
            ]
        );
        // A file one sentence keeps the bytes of keeps them, whatever another says.
        let both = "Do not delete in/a.csv. Leave in/a.csv untouched.";
        let unchanged = from_request(both, &dir);
        std::fs::write(dir.join("in/a.csv"), "x,y,z\n").unwrap();
        assert_eq!(
            changed(&unchanged),
            [format!("`in/a.csv` was changed, {bytes}")]
        );
    }
}
