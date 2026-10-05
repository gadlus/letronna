//! The `search` tool's engine: a walk that honours `.gitignore` and a regex
//! run over each file's bytes. It is single-threaded on purpose: the time
//! and file budgets stop it long before more cores would pay, and one thread
//! walking in name order gives the same page when a page is asked for twice.
//!
//! What the model may never read is dropped in the walk's own filter, which
//! no ignore rule or glob can overrule.
//!
//! PDF, Word, Excel, PowerPoint and OpenDocument files are searched as
//! read_file shows them, and only when the type, the glob or the path names
//! them: reading one takes far longer than a text file, so a plain search
//! passes them by and says how many it passed. A match in one shows the
//! heading of the page, sheet or slide it is on, and the lines are fenced,
//! being someone else's words as a page of one read is. Office files from
//! before 2007 and rich text files are not read, and are counted, as
//! read_file says how to convert them.

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::overrides::OverrideBuilder;
use ignore::types::{Types, TypesBuilder};
use ignore::{DirEntry, WalkBuilder};
use regex::bytes::{Regex, RegexBuilder};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{MAIN_SEPARATOR_STR, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// A search stops after this long or this many files, and says so, rather
/// than walking a whole home folder while the session waits on it.
pub(crate) const SEARCH_TIME: Duration = Duration::from_secs(15);
pub(crate) const SEARCH_FILES: usize = 20_000;
/// Bigger files are skipped: no page can show more than a sliver of one,
/// and reading gigabytes to find that out is how a tool hangs.
const SEARCH_FILE_BYTES: u64 = 1_000_000;
/// Never worth searching, even outside a Git repository where no
/// `.gitignore` says so: version control internals and installed packages.
const SKIP_DIRS: &[&str] = &[".git", ".hg", ".svn", "node_modules"];
/// Build output, skipped by name only outside a repository, where no
/// `.gitignore` leaves it out and a `build/` walked before `src/` could use
/// up the file budget; a glob that names one searches it all the same.
const BUILD_DIRS: &[&str] = &["target", "dist", "build", ".next"];
/// Caches a repository's root `.gitignore` leaves out in every package of
/// it, which the note on that `.gitignore` leaves unnamed as it does build
/// output: they hold no source, and in a monorepo's package naming them
/// would lengthen every search.
const CACHE_DIRS: &[&str] = &[
    "__pycache__",
    ".venv",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".tox",
    ".turbo",
    "coverage",
];
/// The most a compiled pattern may take: twenty Unicode `\w` with case
/// folding fit, and a bigger pattern is the kind that makes matching slow.
const REGEX_BYTES: usize = 1 << 20;
/// The lazy DFA's cache for one search, regex's own default: a smaller one
/// gives up sooner and falls back to the slow engine, a bigger one buys
/// little.
const DFA_BYTES: usize = 2 << 20;
/// A file is matched this many bytes at a time, to the next line end, with
/// the deadline checked in between: the slowest pattern `REGEX_BYTES` allows
/// takes about 0.4 s over this much, so one big file cannot hold a search
/// far past its budget.
const CHUNK_BYTES: usize = 16 << 10;
/// A longer line is cut to this many characters around its match, so one
/// minified file cannot fill the page; the match starts `LEAD_CHARS` in.
const LINE_CHARS: usize = 300;
const LEAD_CHARS: usize = 100;
/// Results per page when the model names no limit, and the most it may name;
/// the tool's output cap cuts in well before 500 long lines anyway.
const LIMIT: usize = 100;
const MAX_LIMIT: usize = 500;
/// Lines of context either side of a match; more turns a page of matches
/// into a page of files.
const MAX_CONTEXT: usize = 5;
/// A page holds at most this many bytes as shown: under the chat's cap on a
/// tool result, with 1,000 bytes for the notes around it, some 450 with a
/// pattern of usual length, so a full page reaches the model whole and
/// paging on from its offset skips nothing. A page's first match is shown
/// whatever its size, which with five lines of multi-byte context either
/// side comes to ~14k.
const PAGE_BYTES: usize = crate::chat::MAX_TOOL_CHARS - 1_000;
/// The most folders the note on a `.gitignore` above the project names: a
/// `*` there, as a home folder kept in Git has, would otherwise list every
/// folder in the project.
const LEFT_OUT_NAMES: usize = 10;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Mode {
    /// Matching lines, grouped under their file.
    Content,
    /// The files that match, newest first.
    Files,
    /// How many lines match in each file.
    Count,
}

/// One search, as the model asked for it.
pub(crate) struct Query<'a> {
    pub root: &'a Path,
    /// The root as the model wrote it: paths outside the workspace are shown
    /// under it, which is how read_file finds them again.
    pub given: &'a str,
    /// The workspace, when the search is inside it: paths are shown relative
    /// to it, and hidden files are searched there.
    pub workspace: Option<&'a Path>,
    /// Letronna's own folder, canonical, which is never searched.
    pub data: Option<PathBuf>,
    pub pattern: Option<&'a str>,
    pub mode: Mode,
    pub glob: Option<&'a str>,
    pub kind: Option<&'a str>,
    pub context: usize,
    pub case_sensitive: bool,
    pub limit: usize,
    pub offset: usize,
    /// Whether PDF and Office files are searched: the type, the glob or the
    /// path names them.
    pub documents: bool,
}

impl<'a> Query<'a> {
    /// Reads the tool arguments; `query` still names the pattern, as it did
    /// before patterns were regexes.
    pub(crate) fn from_args(
        args: &'a Value,
        root: &'a Path,
        workspace: Option<&'a Path>,
    ) -> Result<Self, String> {
        let text = |key: &str| args[key].as_str().map(str::trim).filter(|s| !s.is_empty());
        let number = |key: &str| args[key].as_u64().map(|n| n as usize);
        let mode = match text("mode") {
            None | Some("content") => Mode::Content,
            Some("files" | "files_with_matches") => Mode::Files,
            Some("count") => Mode::Count,
            Some(other) => {
                return Err(format!(
                    "`{other}` is not a mode; use content, files or count."
                ));
            }
        };
        let pattern = [&args["pattern"], &args["query"]]
            .into_iter()
            .find_map(|v| v.as_str().filter(|s| !s.is_empty()));
        if pattern.is_none() && mode != Mode::Files {
            return Err("Pattern is empty.".into());
        }
        // A glob that leaves them out names them only to say so.
        let names_documents = |text: &str| {
            !text.starts_with('!')
                && text
                    .split(|c: char| !c.is_ascii_alphanumeric())
                    .any(crate::docs::is_extension)
        };
        let documents = text("type").is_some_and(names_documents)
            || text("glob").is_some_and(names_documents)
            || (root.is_file() && crate::docs::named(root));
        Ok(Self {
            root,
            given: args["path"].as_str().unwrap_or("."),
            workspace,
            // Once per search, not once per folder as the old walk did.
            data: std::fs::canonicalize(crate::chat::data_dir()).ok(),
            pattern,
            mode,
            glob: text("glob"),
            kind: text("type"),
            context: number("context").unwrap_or(0).min(MAX_CONTEXT),
            case_sensitive: args["case_sensitive"].as_bool().unwrap_or(false),
            limit: number("limit").unwrap_or(LIMIT).clamp(1, MAX_LIMIT),
            offset: number("offset").unwrap_or(0),
            documents,
        })
    }
}

/// One page of results before the notes around it are added.
#[derive(Default)]
struct Page {
    /// The entries shown, each ending in a newline; empty when none matched.
    body: String,
    /// Where the next page starts, or the totals.
    tail: Option<String>,
    /// Whether anything matched at all, on this page or another.
    matched: bool,
    /// Whether the body holds lines of a document.
    from_documents: bool,
    walked: Walked,
}

/// What the walk passed over, for the notes under the results.
#[derive(Default)]
struct Walked {
    scanned: usize,
    stopped: bool,
    secrets: usize,
    /// Folders a `.gitignore` above the workspace hid, in path order.
    left_out: Vec<PathBuf>,
    /// Whether those rules also hide files in a folder named, so naming it
    /// as the path would not search all of it.
    hidden_inside: bool,
    /// PDF and Office files passed by, as nothing named them.
    documents: usize,
    /// PDF and Office files that could not be read.
    unread: usize,
    /// Office files from before 2007 and rich text files passed by.
    older: usize,
}

/// Runs `query` until `deadline` or `files` files, and renders the page;
/// with whether it shows lines of a document.
pub(crate) fn search(
    query: &Query,
    deadline: Instant,
    files: usize,
) -> Result<(String, bool), String> {
    let mut note = None;
    let regex = match query.pattern {
        None => None,
        Some(pattern) => Some(build(pattern, query.case_sensitive).or_else(|e| {
            note = Some(format!(
                "`{pattern}` is not a regex this search can run ({}), so it was searched as \
                 plain text.",
                why(&e)
            ));
            literal(pattern, query.case_sensitive)
        })?),
    };
    let mut page = find(query, regex.as_ref(), deadline, files)?;
    // A model often means `(` or `$` literally; one more pass saves it a turn.
    if !page.matched
        && !page.walked.stopped
        && note.is_none()
        && let Some(pattern) = query.pattern.filter(|p| regex::escape(p) != *p)
    {
        let again = find(
            query,
            Some(&literal(pattern, query.case_sensitive)?),
            deadline,
            files,
        )?;
        if again.matched {
            note = Some(format!(
                "Nothing matches `{pattern}` as a regex; these match it as plain text."
            ));
            page = again;
        }
    }
    let from_documents = page.from_documents;
    Ok((render(query, page, note)?, from_documents))
}

fn build(pattern: &str, case_sensitive: bool) -> Result<Regex, regex::Error> {
    RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .multi_line(true)
        .crlf(true)
        .size_limit(REGEX_BYTES)
        .dfa_size_limit(DFA_BYTES)
        .build()
}

/// Why a pattern would not build, in a few words the model can act on, such
/// as "look-around ... is not supported" for a PCRE habit.
fn why(error: &regex::Error) -> String {
    match error {
        regex::Error::CompiledTooBig(limit) => format!("it compiles past {limit} bytes"),
        other => {
            let text = other.to_string();
            let last = text.lines().last().unwrap_or_default();
            last.trim_start_matches("error: ").to_owned()
        }
    }
}

fn literal(pattern: &str, case_sensitive: bool) -> Result<Regex, String> {
    build(&regex::escape(pattern), case_sensitive).map_err(|e| e.to_string())
}

/// The notes around a page: how the pattern was read, and whatever the walk
/// could not finish or would not show.
fn render(query: &Query, page: Page, note: Option<String>) -> Result<String, String> {
    let mut out = note.map(|n| n + "\n").unwrap_or_default();
    match (page.body.is_empty(), query.offset) {
        // Fenced whole, the notes after it Letronna's own.
        (false, _) if page.from_documents => out.push_str(&crate::untrusted::fence(
            page.body.trim_end(),
            crate::chat::MAX_TOOL_CHARS,
        )?),
        (false, _) => out.push_str(page.body.trim_end()),
        (true, 0) => out.push_str("No matches."),
        (true, offset) => out.push_str(&format!("Nothing past offset {offset}.")),
    }
    if let Some(tail) = page.tail {
        out.push_str(&format!("\n… {tail}"));
    }
    let walked = page.walked;
    if walked.stopped {
        out.push_str(&format!(
            "\n… Stopped early after {} files; search a narrower folder, or with a \
             simpler pattern, for complete results.",
            walked.scanned
        ));
    }
    if walked.secrets > 0 {
        out.push_str(&format!(
            "\n… Secret files and folders left out: {}. Ask the user to paste \
             what you need from them.",
            walked.secrets
        ));
    }
    if !walked.left_out.is_empty() {
        let left_out = &walked.left_out;
        let (named, more) = left_out.split_at(left_out.len().min(LEFT_OUT_NAMES));
        let mut names = named
            .iter()
            .map(|dir| shown(dir, query) + MAIN_SEPARATOR_STR)
            .collect::<Vec<_>>()
            .join(", ");
        if !more.is_empty() {
            names += &format!(" and {} more", more.len());
        }
        let though = if walked.hidden_inside {
            ", though the same rules hide files inside too; list_dir shows all it holds"
        } else {
            ""
        };
        out.push_str(&format!(
            "\n… Left out by a .gitignore above the project: {names}. Name a folder as \
             the path to search it{though}."
        ));
    }
    if walked.documents > 0 {
        let files = match walked.documents {
            1 => "1 PDF/Office file was".to_owned(),
            n => format!("{n} PDF/Office files were"),
        };
        out.push_str(&format!(
            "\n… {files} not searched; name them with type or glob."
        ));
    }
    if walked.older > 0 {
        let files = match walked.older {
            1 => "1 file in an old Office format or RTF was not searched; read_file says how \
                  to convert it"
                .to_owned(),
            n => format!(
                "{n} files in old Office formats or RTF were not searched; read_file says how \
                 to convert them"
            ),
        };
        out.push_str(&format!("\n… {files}."));
    }
    if walked.unread > 0 {
        out.push_str(&format!(
            "\n… PDF/Office files that could not be read: {}; read_file says why.",
            walked.unread
        ));
    }
    Ok(out)
}

/// One pass over the tree with `regex`, rendered the way `query.mode` asks.
fn find(
    query: &Query,
    regex: Option<&Regex>,
    deadline: Instant,
    files: usize,
) -> Result<Page, String> {
    let mut page = Page::default();
    let mut passed = Passed::default();
    match (query.mode, regex) {
        (Mode::Content, Some(regex)) => {
            let (mut seen, end) = (0, query.offset.saturating_add(query.limit));
            page.walked = walk(query, deadline, files, &mut |entry| {
                let (bytes, document) = match content(entry, query, deadline, &mut passed) {
                    Content::Text(bytes) => (bytes, false),
                    Content::Document(bytes) => (bytes, true),
                    Content::Passed => return Next::More,
                    Content::Late => return Next::Late,
                };
                let Some(hits) = hits(&bytes, regex, deadline) else {
                    return Next::Late;
                };
                page.matched |= !hits.is_empty();
                let (mut kept, mut first) = (Vec::new(), None);
                for &n in &hits {
                    if seen == end {
                        page.tail = Some(more(end));
                        break;
                    }
                    if seen >= query.offset {
                        first.get_or_insert(seen);
                        kept.push(n);
                    }
                    seen += 1;
                }
                if let Some(first) = first {
                    let mut file = shown(entry.path(), query) + "\n";
                    let room = PAGE_BYTES.saturating_sub(page.body.len());
                    let (lines, context) = (lines(&bytes), query.context);
                    let places = match document {
                        true => crate::docs::places(entry.path(), &lines),
                        false => Vec::new(),
                    };
                    let found = Lines {
                        lines: &lines,
                        hits: &hits,
                        places: &places,
                    };
                    let done = block(&mut file, room, &found, &kept, context, regex);
                    // Measured as it will be shown: with multi-byte text one
                    // match and its context can run to many kilobytes.
                    if file.len() > room && !page.body.is_empty() {
                        page.tail = Some(more(first));
                        return Next::Full;
                    }
                    page.body += &file;
                    page.from_documents |= document;
                    if done < kept.len() {
                        page.tail = Some(more(first + done));
                    }
                }
                if page.tail.is_none() {
                    Next::More
                } else {
                    Next::Full
                }
            })?;
        }
        (Mode::Files, _) => {
            let mut found = Vec::new();
            page.walked = walk(query, deadline, files, &mut |entry| {
                if let Some(regex) = regex {
                    let bytes = match content(entry, query, deadline, &mut passed) {
                        Content::Text(bytes) | Content::Document(bytes) => bytes,
                        Content::Passed => return Next::More,
                        Content::Late => return Next::Late,
                    };
                    let Some(hits) = hits(&bytes, regex, deadline) else {
                        return Next::Late;
                    };
                    if hits.is_empty() {
                        return Next::More;
                    }
                }
                let modified = entry.metadata().ok().and_then(|m| m.modified().ok());
                found.push((modified, shown(entry.path(), query)));
                Next::More
            })?;
            // Newest first: the file just worked on is usually the one wanted.
            found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            let total = found.len();
            page.matched = total > 0;
            let mut end = query.offset;
            for (_, path) in found.iter().skip(end).take(query.limit) {
                let line = format!("{path}\n");
                if !fits(&page.body, &line) {
                    break;
                }
                page.body += &line;
                end += 1;
            }
            page.tail = paged(query.offset, end, total);
        }
        (Mode::Count, Some(regex)) => {
            let mut found = Vec::new();
            page.walked = walk(query, deadline, files, &mut |entry| {
                let bytes = match content(entry, query, deadline, &mut passed) {
                    Content::Text(bytes) | Content::Document(bytes) => bytes,
                    Content::Passed => return Next::More,
                    Content::Late => return Next::Late,
                };
                let Some(hits) = hits(&bytes, regex, deadline) else {
                    return Next::Late;
                };
                if !hits.is_empty() {
                    found.push((shown(entry.path(), query), hits.len()));
                }
                Next::More
            })?;
            page.matched = !found.is_empty();
            let mut end = query.offset;
            for (path, n) in found.iter().skip(end).take(query.limit) {
                let line = format!("{path}: {n}\n");
                if !fits(&page.body, &line) {
                    break;
                }
                page.body += &line;
                end += 1;
            }
            let lines: usize = found.iter().map(|(_, n)| n).sum();
            let totals = format!("{lines} matching lines in {} files.", found.len());
            page.tail = match paged(query.offset, end, found.len()) {
                Some(more) => Some(format!("{totals} {more}")),
                None if found.is_empty() => None,
                None => Some(totals),
            };
        }
        (_, None) => return Err("Pattern is empty.".into()),
    }
    (page.walked.documents, page.walked.unread) = (passed.documents, passed.unread);
    page.walked.older = passed.older;
    Ok(page)
}

/// The PDF and Office files a pass went by: those nothing named, those it
/// could not read, and those in a format from before 2007 or RTF.
#[derive(Default)]
struct Passed {
    documents: usize,
    unread: usize,
    older: usize,
}

/// What of a file a search reads.
enum Content {
    Text(Vec<u8>),
    /// A document's view, as read_file shows it.
    Document(Vec<u8>),
    /// Nothing: a binary, a file too large, or a document nothing named.
    Passed,
    /// Nothing, as the time ran out reading it.
    Late,
}

/// What a search reads of `entry`: a document's view where the query names
/// documents, else the file's text, counting in `passed` what it goes by.
fn content(entry: &DirEntry, query: &Query, deadline: Instant, passed: &mut Passed) -> Content {
    let path = entry.path();
    // Its bytes or markup would match where its words do not.
    if crate::docs::older(path).is_some() {
        passed.older += 1;
        return Content::Passed;
    }
    if crate::docs::named(path) {
        if !query.documents {
            passed.documents += 1;
            return Content::Passed;
        }
        match crate::docs::view_by(path, deadline) {
            Ok(Some(view)) => return Content::Document(view.into_bytes()),
            // Not one after all, as its first bytes say: read as any file.
            Ok(None) => {}
            Err(why) if crate::docs::ran_late(&why) || Instant::now() >= deadline => {
                return Content::Late;
            }
            Err(_) => {
                passed.unread += 1;
                return Content::Passed;
            }
        }
    }
    match searchable(entry) {
        Some(bytes) => Content::Text(bytes),
        None => Content::Passed,
    }
}

/// Whether `line` goes on the page: it keeps the page within `PAGE_BYTES`,
/// or it is the first, which a page always shows.
fn fits(page: &str, line: &str) -> bool {
    page.is_empty() || page.len() + line.len() <= PAGE_BYTES
}

fn more(offset: usize) -> String {
    format!("More matches follow; pass offset {offset} for the next page.")
}

/// The paging note for files shown from `offset` up to `end`, when there
/// are more than that.
fn paged(offset: usize, end: usize, total: usize) -> Option<String> {
    (end < total).then(|| {
        format!(
            "Showing {}-{end} of {total} files; pass offset {end} for the next page.",
            offset + 1
        )
    })
}

/// What a pass tells the walk after a file.
enum Next {
    More,
    /// The page is full.
    Full,
    /// The deadline passed, or would have, inside the file.
    Late,
}

/// Walks `query.root` and hands `f` each file until it stops the walk or the
/// budget runs out. The budget is checked on folders too, or a deep tree of
/// them would run unbounded.
fn walk(
    query: &Query,
    deadline: Instant,
    files: usize,
    f: &mut dyn FnMut(&DirEntry) -> Next,
) -> Result<Walked, String> {
    let secrets = Arc::new(AtomicUsize::new(0));
    let above = ignores_above(query);
    let (mut walked, mut dirs, mut full) = (Walked::default(), BTreeSet::new(), false);
    for entry in walker(query, secrets.clone())?.filter_map(Result::ok) {
        if walked.scanned >= files || Instant::now() >= deadline {
            walked.stopped = true;
            break;
        }
        // A link is never followed: reading through it would reach whatever
        // it points at, a secret or Letronna's own folder included.
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            if !above.is_empty() && entry.file_type().is_some_and(|t| t.is_dir()) {
                dirs.insert(entry.into_path());
            }
            continue;
        }
        walked.scanned += 1;
        match f(&entry) {
            Next::More => {}
            Next::Full => {
                full = true;
                break;
            }
            Next::Late => {
                walked.stopped = true;
                break;
            }
        }
    }
    walked.secrets = secrets.load(Ordering::Relaxed);
    // Only a walk that went through the whole tree knows what it passed over.
    if !full && !walked.stopped {
        (walked.left_out, walked.hidden_inside) = left_out(query, &above, &dirs, deadline);
    }
    Ok(walked)
}

/// The `.gitignore` files above the workspace that the walk honours, nearest
/// first, when the repository's root lies above the workspace, as a
/// benchmark's sandbox or a monorepo's package does; none otherwise.
fn ignores_above(query: &Query) -> Vec<Gitignore> {
    let Some(workspace) = query.workspace else {
        return Vec::new();
    };
    let top = query.root.ancestors().find(|dir| dir.join(".git").exists());
    let Some(top) = top.filter(|top| *top != workspace && workspace.starts_with(top)) else {
        return Vec::new();
    };
    workspace
        .ancestors()
        .skip(1)
        .take_while(|dir| dir.starts_with(top))
        .map(|dir| Gitignore::new(dir.join(".gitignore")).0)
        .filter(|ignore| !ignore.is_empty())
        .collect()
}

/// The folders the walk passed over inside those it walked, `dirs`, that a
/// `.gitignore` in `above` left out, and whether those rules hide files in
/// the first named too; nothing once `deadline` passes.
fn left_out(
    query: &Query,
    above: &[Gitignore],
    dirs: &BTreeSet<PathBuf>,
    deadline: Instant,
) -> (Vec<PathBuf>, bool) {
    // Folders are gathered only when a `.gitignore` lies above the workspace.
    let Some(workspace) = query.workspace.filter(|_| !dirs.is_empty()) else {
        return (Vec::new(), false);
    };
    // The project's own ignore files, starting with those between it and a
    // searched folder inside it, which the walk read without walking theirs.
    let mut inner: Vec<(PathBuf, Gitignore)> = query
        .root
        .ancestors()
        .skip(1)
        .take_while(|dir| dir.starts_with(workspace))
        .map(|dir| (dir.to_path_buf(), own_ignores(dir)))
        .collect();
    let (mut found, mut repos) = (Vec::new(), Vec::<&Path>::new());
    // In path order, which puts a repository's root before what is in it,
    // and a folder's ignore files before the folders they reach.
    for dir in dirs {
        if Instant::now() >= deadline {
            return (Vec::new(), false);
        }
        if repos.iter().any(|repo| dir.starts_with(repo)) {
            continue;
        }
        let Ok(children) = std::fs::read_dir(dir) else {
            continue;
        };
        let children: Vec<_> = children.filter_map(Result::ok).collect();
        // Git reads no `.gitignore` above a repository's own root, so in a
        // repository inside the workspace those above it hide nothing.
        if children.iter().any(|child| child.file_name() == ".git") {
            repos.push(dir);
            continue;
        }
        if children
            .iter()
            .any(|child| child.file_name() == ".gitignore" || child.file_name() == ".ignore")
        {
            inner.push((dir.clone(), own_ignores(dir)));
        }
        for child in children {
            let (name, path) = (child.file_name(), child.path());
            // A folder walked was let back in by an ignore file inside the
            // project; skipped and secret folders stay out whatever any
            // `.gitignore` says; and build output and caches are ignored as
            // a matter of course, so naming them in a monorepo's package
            // would only lengthen every search.
            if !child.file_type().is_ok_and(|t| t.is_dir())
                || dirs.contains(&path)
                || SKIP_DIRS
                    .iter()
                    .chain(BUILD_DIRS)
                    .chain(CACHE_DIRS)
                    .any(|skip| name == *skip)
                || crate::tools::secret(&path)
            {
                continue;
            }
            if hidden_above(&path, true, &inner, above) {
                found.push(path);
            }
        }
    }
    found.sort();
    // A `*` or `*.csv` above hides what is in a folder as well as the
    // folder, and a search of it would then leave that out without a word.
    let mut hidden_inside = false;
    for dir in found.iter().take(LEFT_OUT_NAMES) {
        if Instant::now() >= deadline {
            return (Vec::new(), false);
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let entries: Vec<_> = entries.filter_map(Result::ok).collect();
        // A repository's own root, searched, reads nothing above it.
        if entries.iter().any(|entry| entry.file_name() == ".git") {
            continue;
        }
        // Searched, the folder's own ignore files count for what is in it.
        inner.push((dir.clone(), own_ignores(dir)));
        hidden_inside = entries.iter().any(|entry| {
            let (name, path) = (entry.file_name(), entry.path());
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            // What no search reads anyway is not worth the warning.
            !SKIP_DIRS.iter().any(|skip| name == *skip)
                && !crate::tools::secret(&path)
                && hidden_above(&path, is_dir, &inner, above)
        });
        if hidden_inside {
            break;
        }
    }
    (found, hidden_inside)
}

/// Whether a rule in `above`, and none in the project's own ignore files in
/// `inner`, leaves `path` out of the walk.
fn hidden_above(
    path: &Path,
    is_dir: bool,
    inner: &[(PathBuf, Gitignore)],
    above: &[Gitignore],
) -> bool {
    // The nearest ignore file decides, and an `.ignore` outranks every
    // `.gitignore`, so a match inside the project either way settles it there.
    if inner
        .iter()
        .any(|(dir, ignore)| path.starts_with(dir) && !ignore.matched(path, is_dir).is_none())
    {
        return false;
    }
    above
        .iter()
        .map(|ignore| ignore.matched(path, is_dir))
        .find(|rule| !rule.is_none())
        .is_some_and(|rule| rule.is_ignore())
}

/// The rules in `dir`'s own `.gitignore` and `.ignore`, as one matcher.
fn own_ignores(dir: &Path) -> Gitignore {
    let mut builder = GitignoreBuilder::new(dir);
    // A file that is missing or will not read adds nothing, as in the walk.
    for name in [".gitignore", ".ignore"] {
        let _ = builder.add(dir.join(name));
    }
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

/// Every file and folder a search of `root` would walk, relative to it with
/// `/` between names and a folder ending in one, in walk order, until
/// `deadline` or `most` of them.
pub(crate) fn entries(root: &Path, deadline: Instant, most: usize) -> Result<Vec<String>, String> {
    let query = Query {
        root,
        given: ".",
        workspace: Some(root),
        data: std::fs::canonicalize(crate::chat::data_dir()).ok(),
        pattern: None,
        mode: Mode::Files,
        glob: None,
        kind: None,
        context: 0,
        case_sensitive: false,
        limit: 0,
        offset: 0,
        // Only names are walked; nothing is read.
        documents: false,
    };
    let mut found = Vec::new();
    for entry in walker(&query, Arc::default())?.filter_map(Result::ok) {
        if found.len() >= most || Instant::now() >= deadline {
            break;
        }
        // A link is left out, as the search leaves it: it leads elsewhere.
        let Some(kind) = entry.file_type().filter(|t| t.is_file() || t.is_dir()) else {
            continue;
        };
        let Ok(inside) = entry.path().strip_prefix(root) else {
            continue;
        };
        if entry.depth() == 0 {
            continue;
        }
        let mut name: Vec<String> = inside
            .iter()
            .map(|part| part.to_string_lossy().into_owned())
            .collect();
        if kind.is_dir() {
            name.push(String::new());
        }
        found.push(name.join("/"));
    }
    Ok(found)
}

/// The walk: `.gitignore` and `.ignore` honoured, even outside a Git
/// repository; names in order so pages are stable. What must stay out goes
/// in the filter, which no ignore rule, type or glob outranks: Letronna's own
/// folder, `SKIP_DIRS`, the secrets, and hidden entries outside the
/// workspace, since elsewhere in the home folder they are other programs'
/// settings and history, often holding tokens. The glob goes there too: as
/// an ignore override it would outrank `.gitignore`, and `*` would bring
/// back every ignored folder.
fn walker(query: &Query, secrets: Arc<AtomicUsize>) -> Result<ignore::Walk, String> {
    let repo = in_repo(query.root);
    let mut builder = WalkBuilder::new(query.root);
    // Inside a repository `.gitignore`s count from its root down, as git
    // reads them; outside one only the searched folder's own count, or a `*`
    // in a `.gitignore` above it would empty every search.
    builder
        .hidden(false)
        .require_git(repo)
        .parents(repo)
        .sort_by_file_name(|a, b| a.cmp(b));
    let glob = match query.glob {
        None => None,
        Some(glob) => {
            let mut overrides = OverrideBuilder::new(query.root);
            overrides
                .add(glob)
                .map_err(|e| format!("`{glob}` is not a glob: {e}"))?;
            Some(overrides.build().map_err(|e| e.to_string())?)
        }
    };
    if let Some(kind) = query.kind {
        builder.types(types(kind)?);
    }
    let builds: Vec<&str> = BUILD_DIRS
        .iter()
        .copied()
        .filter(|dir| !repo && !query.glob.is_some_and(|glob| names(glob, dir)))
        .collect();
    let (data, outside) = (query.data.clone(), query.workspace.is_none());
    builder.filter_entry(move |entry| {
        let (path, name) = (entry.path(), entry.file_name());
        if data.as_ref().is_some_and(|data| path.starts_with(data))
            || SKIP_DIRS.iter().chain(&builds).any(|skip| name == *skip)
            || (outside && hidden(path))
        {
            return false;
        }
        let dir = entry.file_type().is_some_and(|t| t.is_dir());
        if glob
            .as_ref()
            .is_some_and(|g| g.matched(path, dir).is_ignore())
        {
            return false;
        }
        if crate::tools::secret(path) {
            secrets.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        true
    });
    Ok(builder.build())
}

/// Whether `root` lies in a Git repository: a `.git` in it or above it, the
/// folder or the file a worktree has instead.
fn in_repo(root: &Path) -> bool {
    root.ancestors().any(|dir| dir.join(".git").exists())
}

/// Whether `glob` names `folder` as one of its parts, as `build/**` does.
fn names(glob: &str, folder: &str) -> bool {
    glob.split('/').any(|part| part == folder)
}

/// Whether a file or folder is hidden: its name starts with a dot, or on
/// Windows it has the hidden attribute, as AppData does.
pub(crate) fn hidden(path: &Path) -> bool {
    let dot = path
        .file_name()
        .is_some_and(|name| name.as_encoded_bytes().starts_with(b"."));
    dot || hidden_attribute(path)
}

#[cfg(windows)]
fn hidden_attribute(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    /// `FILE_ATTRIBUTE_HIDDEN` in the Windows API.
    const HIDDEN: u32 = 0x2;
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_attributes() & HIDDEN != 0)
}

#[cfg(not(windows))]
fn hidden_attribute(_: &Path) -> bool {
    false
}

/// Whether `root`, or a folder on the way down to it from `base`, is hidden;
/// `root` lies under `base`.
pub(crate) fn hidden_below(base: &Path, root: &Path) -> bool {
    root.ancestors().take_while(|dir| *dir != base).any(hidden)
}

/// A type filter: ripgrep's names (rust, py, ts, go...), or a bare extension
/// the list does not know, such as `rs`.
fn types(kind: &str) -> Result<Types, String> {
    let kind = kind.to_ascii_lowercase();
    let mut types = TypesBuilder::new();
    types.add_defaults();
    if !types.definitions().iter().any(|d| d.name() == kind) {
        // `all` would select every type, not files ending `.all`.
        if kind == "all" || !kind.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(format!(
                "`{kind}` is not a file type; name one such as rust, py, ts or go, or use glob."
            ));
        }
        types
            .add(&kind, &format!("*.{kind}"))
            .map_err(|e| e.to_string())?;
    }
    types.select(&kind);
    types.build().map_err(|e| e.to_string())
}

/// A file worth reading: small enough, and not binary. The size comes from
/// the metadata so a huge file is never opened; a NUL byte anywhere marks a
/// binary, once UTF-16 text, whose every other byte is one, is re-encoded.
fn searchable(entry: &DirEntry) -> Option<Vec<u8>> {
    if entry.metadata().ok()?.len() > SEARCH_FILE_BYTES {
        return None;
    }
    let bytes = std::fs::read(entry.path()).ok()?;
    let bytes = utf16(&bytes).unwrap_or(bytes);
    (!bytes.contains(&0)).then_some(bytes)
}

/// UTF-16 text with a byte-order mark, as Windows PowerShell 5 writes it,
/// as UTF-8; None for anything else.
fn utf16(bytes: &[u8]) -> Option<Vec<u8>> {
    let (rest, little) = match bytes {
        [0xFF, 0xFE, rest @ ..] => (rest, true),
        [0xFE, 0xFF, rest @ ..] => (rest, false),
        _ => return None,
    };
    let units = rest.as_chunks::<2>().0.iter().map(|&pair| {
        if little {
            u16::from_le_bytes(pair)
        } else {
            u16::from_be_bytes(pair)
        }
    });
    let text: String = char::decode_utf16(units)
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect();
    Some(text.into_bytes())
}

/// A file's lines without their `\n` or `\r\n`; an empty file has none.
fn lines(bytes: &[u8]) -> Vec<&[u8]> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    bytes
        .split(|&b| b == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
        .collect()
}

/// The indexes of the lines in `bytes` that `regex` matches, or None when
/// the deadline passes first. The file goes a chunk at a time, so the
/// deadline is checked inside it, and a chunk without a match, as most are,
/// is never split into lines.
fn hits(bytes: &[u8], regex: &Regex, deadline: Instant) -> Option<Vec<usize>> {
    let (mut hits, mut line, mut rest) = (Vec::new(), 0, bytes);
    while !rest.is_empty() {
        let end = rest
            .get(CHUNK_BYTES..)
            .and_then(|tail| tail.iter().position(|&b| b == b'\n'))
            .map_or(rest.len(), |at| CHUNK_BYTES + at + 1);
        let (chunk, tail) = rest.split_at(end);
        if Instant::now() >= deadline || too_slow(chunk, regex, deadline) {
            return None;
        }
        if regex.is_match(chunk) {
            let found = lines(chunk).into_iter().enumerate();
            hits.extend(
                found
                    .filter(|(_, l)| regex.is_match(l))
                    .map(|(n, _)| line + n),
            );
        }
        // Every chunk but the last ends a line, so its newlines are its lines.
        line += chunk.iter().filter(|&&b| b == b'\n').count();
        rest = tail;
    }
    Some(hits)
}

/// Whether matching `chunk` would run past the deadline, when one long line
/// makes it much bigger than a chunk: timed on its first chunk and scaled,
/// since the slow engine's cost grows with the bytes it reads.
fn too_slow(chunk: &[u8], regex: &Regex, deadline: Instant) -> bool {
    if chunk.len() <= 2 * CHUNK_BYTES {
        return false;
    }
    let start = Instant::now();
    // Only the time it takes counts, not the answer.
    let _ = regex.is_match(&chunk[..CHUNK_BYTES]);
    let whole = start
        .elapsed()
        .mul_f64(chunk.len() as f64 / CHUNK_BYTES as f64);
    start.checked_add(whole).is_none_or(|end| end > deadline)
}

/// A path as the model should pass it back: relative to the workspace when
/// inside it, and otherwise under the root as the model wrote it, rather
/// than canonical, which on Windows starts `\\?\`.
fn shown(path: &Path, query: &Query) -> String {
    if let Some(inside) = query.workspace.and_then(|w| path.strip_prefix(w).ok()) {
        return inside.display().to_string();
    }
    match path.strip_prefix(query.root) {
        Ok(rel) if !rel.as_os_str().is_empty() => Path::new(query.given).join(rel),
        _ => Path::new(query.given).to_path_buf(),
    }
    .display()
    .to_string()
}

/// A file's lines as a search shows them: which match, and for a document
/// the lines that say where in it the others are.
struct Lines<'a> {
    lines: &'a [&'a [u8]],
    hits: &'a [usize],
    places: &'a [usize],
}

/// One file's `kept` matches with `context` lines around them, the way
/// ripgrep shows them: `12:` on a matching line, `13-` on context, and `--`
/// between runs that do not touch. In a document each match follows the
/// heading of its page, sheet or slide, shown as context unless it is shown
/// already. Adds matches while `out` stays within `room` bytes, the first
/// whatever its size, and says how many it added.
fn block(
    out: &mut String,
    room: usize,
    file: &Lines,
    kept: &[usize],
    context: usize,
    regex: &Regex,
) -> usize {
    let mut last: Option<usize> = None;
    for (done, &n) in kept.iter().enumerate() {
        let from = n
            .saturating_sub(context)
            .max(last.map_or(0, |last| last + 1));
        let to = (n + context).min(file.lines.len() - 1);
        let place = file.places[..file.places.partition_point(|&at| at <= n)]
            .last()
            .copied()
            .filter(|&at| at < from && last.is_none_or(|last| at > last));
        let apart = |at: usize| context > 0 && last.is_some_and(|last| at > last + 1);
        let mut run = String::new();
        if let Some(at) = place {
            if apart(at) {
                run.push_str("  --\n");
            }
            run.push_str(format!("  {}- {}", at + 1, clip(file.lines[at], None)).trim_end());
            run.push('\n');
            if context > 0 && from > at + 1 {
                run.push_str("  --\n");
            }
        } else if apart(from) {
            run.push_str("  --\n");
        }
        for (at, line) in file.lines.iter().enumerate().take(to + 1).skip(from) {
            let hit = file.hits.binary_search(&at).is_ok();
            let mark = if hit { ':' } else { '-' };
            let text = clip(line, hit.then_some(regex));
            // A blank line of context keeps no trailing space.
            run.push_str(format!("  {}{mark} {text}", at + 1).trim_end());
            run.push('\n');
        }
        if done > 0 && out.len() + run.len() > room {
            return done;
        }
        out.push_str(&run);
        last = Some(last.map_or(to, |last| last.max(to)));
    }
    kept.len()
}

/// A line as text, cut to `LINE_CHARS` around its first match.
fn clip(line: &[u8], regex: Option<&Regex>) -> String {
    let text = String::from_utf8_lossy(line);
    let text = text.trim_end();
    let total = text.chars().count();
    if total <= LINE_CHARS {
        return text.to_owned();
    }
    let at = regex.and_then(|regex| regex.find(line)).map_or(0, |m| {
        String::from_utf8_lossy(&line[..m.start()]).chars().count()
    });
    let start = at.saturating_sub(LEAD_CHARS);
    let out: String = text.chars().skip(start).take(LINE_CHARS).collect();
    // Either end may be partway through a key, which can be told only here.
    let mut out = crate::redact::scrub_cut(&out, false);
    if start > 0 {
        out.insert(0, '…');
    }
    if start + LINE_CHARS < total {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A fresh folder holding `files`, canonical as `resolve` hands roots over.
    fn tree(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("letronna-grep-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (path, text) in files {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        std::fs::canonicalize(dir).unwrap()
    }

    /// Searches `dir` as the workspace with the full budget.
    fn run(dir: &Path, args: Value) -> String {
        let query = Query::from_args(&args, dir, Some(dir)).unwrap();
        search(&query, Instant::now() + SEARCH_TIME, SEARCH_FILES)
            .unwrap()
            .0
    }

    fn sep(text: &str) -> String {
        text.replace('/', std::path::MAIN_SEPARATOR_STR)
    }

    #[test]
    fn a_search_hits_text_on_a_pdf_page_with_its_heading() {
        let dir = tree("documents", &[("notes.txt", "The board met.")]);
        let pdf = crate::docs::tests::text_pdf(&[&["Minutes of the board"], &["Adjourned"]]);
        std::fs::write(dir.join("minutes.pdf"), pdf).unwrap();
        // Named by type, by glob, or as the path, it is read as read_file
        // reads it, each match under the heading of its page, and fenced.
        let named = |args: Value| {
            let file = dir.join("minutes.pdf");
            let query = Query::from_args(&args, &file, Some(&dir)).unwrap();
            search(&query, Instant::now() + SEARCH_TIME, SEARCH_FILES)
                .unwrap()
                .0
        };
        for args in [
            json!({"pattern": "adjourned|board", "type": "pdf"}),
            json!({"pattern": "adjourned|board", "glob": "*.{pdf,docx}"}),
            json!({"pattern": "adjourned|board", "path": "minutes.pdf"}),
        ] {
            let found = match args["path"].is_string() {
                true => named(args.clone()),
                false => run(&dir, args.clone()),
            };
            let (fenced, rest) = found.split_once('\n').unwrap();
            assert!(fenced.starts_with("<untrusted-content-"), "{args}: {found}");
            assert!(
                rest.starts_with(
                    "minutes.pdf\n  1- ## Page 1 of 2\n  2: Minutes of the board\n  \
                     4- ## Page 2 of 2\n  5: Adjourned\n</untrusted-content-"
                ),
                "{args}: {found}"
            );
        }
        // Context shows the lines between, the heading among them.
        let found = run(
            &dir,
            json!({"pattern": "adjourned", "type": "pdf", "context": 2}),
        );
        assert!(
            found.contains("minutes.pdf\n  3-\n  4- ## Page 2 of 2\n  5: Adjourned\n"),
            "{found}"
        );
        // One that does not read is counted, not shown.
        std::fs::write(dir.join("broken.pdf"), b"%PDF-1.7 and nothing more").unwrap();
        let found = run(
            &dir,
            json!({"pattern": "board", "mode": "count", "type": "pdf"}),
        );
        assert_eq!(
            found,
            "minutes.pdf: 1\n… 1 matching lines in 1 files.\n\
             … PDF/Office files that could not be read: 1; read_file says why."
        );
    }

    #[test]
    fn a_plain_search_names_the_documents_it_skipped() {
        let dir = tree("skipped", &[("notes.txt", "budget for May")]);
        let pdf = crate::docs::tests::text_pdf(&[&["The budget"]]);
        std::fs::write(dir.join("budget.pdf"), &pdf).unwrap();
        std::fs::write(dir.join("old.pdf"), &pdf).unwrap();
        // Unread, and unfenced, as only the user's own text is shown.
        assert_eq!(
            run(&dir, json!({"pattern": "budget"})),
            "notes.txt\n  1: budget for May\n\
             … 2 PDF/Office files were not searched; name them with type or glob."
        );
        // A glob that leaves them out does not name them.
        assert_eq!(
            run(&dir, json!({"pattern": "budget", "glob": "!*.pdf"})),
            "notes.txt\n  1: budget for May"
        );
        // Listing files reads none of them.
        assert_eq!(
            run(&dir, json!({"mode": "files", "glob": "*.pdf"}))
                .lines()
                .count(),
            2
        );
    }

    #[test]
    fn a_search_counts_old_office_and_rtf_files_and_names_one_searched_alone() {
        let dir = tree(
            "older",
            &[
                ("notes.txt", "budget for May"),
                ("letter.rtf", r"{\rtf1\ansi budget\par}"),
                // Text merely named as an old file is searched as text.
                ("plain.doc", "budget as plain text"),
            ],
        );
        std::fs::write(
            dir.join("old.xls"),
            b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1\0budget",
        )
        .unwrap();
        assert_eq!(
            run(&dir, json!({"pattern": "budget"})),
            "notes.txt\n  1: budget for May\nplain.doc\n  1: budget as plain text\n\
             … 2 files in old Office formats or RTF were not searched; read_file says how to \
             convert them."
        );
        assert_eq!(
            run(&dir, json!({"pattern": "budget", "glob": "*.rtf"})),
            "No matches.\n… 1 file in an old Office format or RTF was not searched; read_file \
             says how to convert it."
        );
        let ctx = crate::tools::ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        let alone = crate::tools::run("search", r#"{"pattern":"budget","path":"old.xls"}"#, &ctx)
            .unwrap_err();
        assert!(
            alone.starts_with(
                "`old.xls` is an Excel 97-2003 workbook, which read_file cannot read."
            ) && alone.contains("--convert-to xlsx --outdir . old.xls`"),
            "{alone}"
        );
    }

    #[test]
    fn a_search_reads_opendocument_files_under_their_headings() {
        let dir = tree("opendocument", &[("notes.txt", "Rent is due")]);
        let odf = crate::docs::tests::odf;
        let sheet = r#"<table:table table:name="May"><table:table-row><table:table-cell><text:p>Rent</text:p></table:table-cell><table:table-cell><text:p>900</text:p></table:table-cell></table:table-row></table:table>"#;
        std::fs::write(dir.join("budget.ods"), odf("spreadsheet", "", sheet)).unwrap();
        let text =
            r#"<text:h text:outline-level="1">Lease</text:h><text:p>Rent is due monthly.</text:p>"#;
        std::fs::write(dir.join("lease.odt"), odf("text", "", text)).unwrap();
        // Passed by unless named, as any document is.
        assert_eq!(
            run(&dir, json!({"pattern": "rent"})),
            "notes.txt\n  1: Rent is due\n\
             … 2 PDF/Office files were not searched; name them with type or glob."
        );
        let found = run(
            &dir,
            json!({"pattern": "rent", "glob": "*.{ods,odt}", "context": 1}),
        );
        assert!(
            found.contains(
                "budget.ods\n  1- ## Sheet \"May\" (1 rows x 2 cols)\n  2: Rent\t900\n\
                 lease.odt\n  1- # Lease\n  2: Rent is due monthly.\n</untrusted-content-"
            ),
            "{found}"
        );
        let found = run(
            &dir,
            json!({"pattern": "rent", "type": "odt", "mode": "count"}),
        );
        assert_eq!(found, "lease.odt: 1\n… 1 matching lines in 1 files.");
    }

    #[test]
    fn search_stays_bounded() {
        let dir = tree(
            "bounded",
            &[
                ("ok.txt", "a needle here"),
                (".hidden/a.txt", "needle"),
                (".hidden.txt", "needle"),
            ],
        );
        std::fs::write(dir.join("bin.dat"), b"needle\0binary").unwrap();
        let big = format!("needle\n{}", "x".repeat(SEARCH_FILE_BYTES as usize));
        std::fs::write(dir.join("big.txt"), big).unwrap();
        // A link back up the tree would recurse until paths ran out.
        #[cfg(unix)]
        std::os::unix::fs::symlink(&dir, dir.join("loop")).unwrap();

        // Outside the workspace hidden files are other programs' settings,
        // and paths come back under the folder as it was given.
        let args = json!({"pattern": "needle", "path": "../elsewhere"});
        let query = Query::from_args(&args, &dir, None).unwrap();
        let far = Instant::now() + SEARCH_TIME;
        assert_eq!(
            search(&query, far, SEARCH_FILES).unwrap().0,
            format!(
                "{}\n  1: a needle here",
                Path::new("../elsewhere").join("ok.txt").display()
            )
        );
        // Out of budget: stops at once and says why, rather than wandering on.
        for out in [
            search(&query, far, 0).unwrap().0,
            search(&query, Instant::now(), SEARCH_FILES).unwrap().0,
        ] {
            assert!(out.starts_with("No matches."), "{out}");
            assert!(out.contains("Stopped early after 0 files"), "{out}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn matches_are_grouped_under_their_file_in_path_order() {
        let dir = tree(
            "lines",
            &[
                ("none.txt", "nothing\nto see\nhere"),
                ("some.txt", "one\ntwo\nThe Needle\nfour\nneedle again"),
                ("a/b.txt", "needle\r\n"),
            ],
        );
        assert_eq!(
            run(&dir, json!({"pattern": "needle"})),
            sep("a/b.txt\n  1: needle\nsome.txt\n  3: The Needle\n  5: needle again")
        );
        assert_eq!(
            run(&dir, json!({"pattern": "Needle$", "case_sensitive": true})),
            "some.txt\n  3: The Needle"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ignored_files_stay_out_but_the_workspaces_hidden_folders_do_not() {
        let dir = tree(
            "ignore",
            &[
                (".gitignore", "generated/\n*.log\n"),
                (
                    "src/main.rs",
                    "fn main() {}\nfn helper_one() {}\nlet f = fn_x();\n",
                ),
                ("generated/out.rs", "fn main() {}\n"),
                ("debug.log", "fn main() {}\n"),
                (".git/config", "fn main() {}\n"),
                ("node_modules/x/index.rs", "fn main() {}\n"),
                (".github/ci.yml", "run: fn main()\n"),
            ],
        );
        assert_eq!(
            run(&dir, json!({"pattern": r"^fn \w+\(\)"})),
            sep("src/main.rs\n  1: fn main() {}\n  2: fn helper_one() {}")
        );
        assert_eq!(
            run(&dir, json!({"pattern": "run: fn"})),
            sep(".github/ci.yml\n  1: run: fn main()")
        );
        // A glob narrows the search; it never brings ignored files back.
        assert_eq!(
            run(&dir, json!({"pattern": r"^fn main", "glob": "*"})),
            sep("src/main.rs\n  1: fn main() {}")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn secret_files_stay_out_whatever_the_glob_says() {
        let dir = tree(
            "secrets",
            &[
                ("app/.ssh/id_rsa", "TOKEN=1"),
                ("app/.env.local", "TOKEN=2"),
                (".env", "TOKEN=3"),
                (".git-credentials", "https://me:TOKEN@host"),
                (".env.example", "TOKEN="),
            ],
        );
        // A link is never read through, whatever it points at.
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join(".env"), dir.join("notes.txt")).unwrap();
        let left_out = |n| {
            format!(
                "… Secret files and folders left out: {n}. Ask the user to paste what \
                 you need from them."
            )
        };
        // The folder counts once; nothing inside it is looked at.
        assert_eq!(
            run(&dir, json!({"pattern": "token"})),
            format!(".env.example\n  1: TOKEN=\n{}", left_out(4))
        );
        assert_eq!(
            run(&dir, json!({"pattern": "token", "glob": ".env*"})),
            format!(".env.example\n  1: TOKEN=\n{}", left_out(3))
        );
        assert_eq!(
            run(
                &dir,
                json!({"pattern": "token", "mode": "count", "glob": "*"})
            ),
            format!(
                ".env.example: 1\n… 1 matching lines in 1 files.\n{}",
                left_out(4)
            )
        );
        let listed = run(&dir, json!({"mode": "files", "glob": "**/.ssh/**"}));
        assert_eq!(listed, format!("No matches.\n{}", left_out(1)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn globs_and_types_narrow_the_files() {
        let dir = tree(
            "kinds",
            &[
                ("src/a.rs", "needle"),
                ("src/b.ts", "needle"),
                ("src/c.tsx", "needle"),
                ("docs/d.md", "needle"),
            ],
        );
        let rust = sep("src/a.rs\n  1: needle");
        assert_eq!(
            run(&dir, json!({"pattern": "needle", "glob": "*.rs"})),
            rust
        );
        assert_eq!(
            run(&dir, json!({"pattern": "needle", "type": "rust"})),
            rust
        );
        assert_eq!(run(&dir, json!({"pattern": "needle", "type": "rs"})), rust);
        assert_eq!(
            run(
                &dir,
                json!({"pattern": "needle", "glob": "src/*.{ts,tsx}", "mode": "count"})
            ),
            sep("src/b.ts: 1\nsrc/c.tsx: 1\n… 2 matching lines in 2 files.")
        );
        assert_eq!(
            run(
                &dir,
                json!({"pattern": "needle", "glob": "!*.md", "mode": "count"})
            ),
            sep("src/a.rs: 1\nsrc/b.ts: 1\nsrc/c.tsx: 1\n… 3 matching lines in 3 files.")
        );
        for (args, refusal) in [
            (
                json!({"pattern": "x", "type": "c++!"}),
                "is not a file type",
            ),
            (json!({"pattern": "x", "glob": "a[b"}), "is not a glob"),
            (json!({"pattern": "x", "type": "all"}), "is not a file type"),
        ] {
            let query = Query::from_args(&args, &dir, Some(&dir)).unwrap();
            let error = search(&query, Instant::now() + SEARCH_TIME, 1).unwrap_err();
            assert!(error.contains(refusal), "{error}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn files_come_newest_first_and_need_no_pattern() {
        let dir = tree(
            "files",
            &[
                ("old.rs", "needle"),
                ("new.rs", "needle"),
                ("other.rs", "nothing"),
            ],
        );
        let now = std::time::SystemTime::now();
        for (name, age) in [("new.rs", 0), ("other.rs", 60), ("old.rs", 3600)] {
            let file = std::fs::File::options()
                .write(true)
                .open(dir.join(name))
                .unwrap();
            file.set_modified(now - Duration::from_secs(age)).unwrap();
        }
        assert_eq!(
            run(&dir, json!({"pattern": "needle", "mode": "files"})),
            "new.rs\nold.rs"
        );
        assert_eq!(
            run(&dir, json!({"mode": "files", "glob": "*.rs", "limit": 2})),
            "new.rs\nother.rs\n… Showing 1-2 of 3 files; pass offset 2 for the next page."
        );
        let args = json!({"glob": "*.rs"});
        let refused = Query::from_args(&args, &dir, Some(&dir)).err();
        assert_eq!(refused.as_deref(), Some("Pattern is empty."));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn context_lines_surround_matches_and_separate_runs_are_marked() {
        let dir = tree(
            "context",
            &[("f.txt", "1\n2\nhit\n\n5\n6\n7\nhit\nhit\n10")],
        );
        assert_eq!(
            run(&dir, json!({"pattern": "hit", "context": 1})),
            "f.txt\n  2- 2\n  3: hit\n  4-\n  --\n  7- 7\n  8: hit\n  9: hit\n  10- 10"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pages_follow_on_from_the_offset() {
        let dir = tree("pages", &[("a.txt", "x\nx\nx"), ("b.txt", "x\nx")]);
        let page = |offset: usize| run(&dir, json!({"pattern": "x", "limit": 2, "offset": offset}));
        assert_eq!(
            page(0),
            "a.txt\n  1: x\n  2: x\n… More matches follow; pass offset 2 for the next page."
        );
        assert_eq!(
            page(2),
            "a.txt\n  3: x\nb.txt\n  1: x\n… More matches follow; pass offset 4 for the next page."
        );
        assert_eq!(page(4), "b.txt\n  2: x");
        assert_eq!(page(9), "Nothing past offset 9.");
        assert_eq!(
            page(usize::MAX),
            format!("Nothing past offset {}.", usize::MAX)
        );
        assert_eq!(
            run(&dir, json!({"pattern": "x", "mode": "count", "limit": 1})),
            "a.txt: 3\n… 5 matching lines in 2 files. Showing 1-1 of 2 files; pass offset 1 \
             for the next page."
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_full_page_stops_where_the_next_one_starts() {
        let line = format!("x{}\n", "y".repeat(LINE_CHARS));
        let dir = tree("full", &[("a.txt", &line.repeat(200)), ("b.txt", "x")]);
        let first = run(&dir, json!({"pattern": "x", "limit": 500}));
        let shown = first.lines().filter(|l| l.contains(": x")).count();
        assert!(first.len() < 24_000, "{}", first.len());
        assert!(
            first.ends_with(&format!("… {}", more(shown))),
            "{}",
            &first[first.len() - 80..]
        );
        // Measured as shown: multi-byte context comes to kilobytes a match.
        let wide = "😀".repeat(LINE_CHARS);
        let text: String = (0..200)
            .map(|n| {
                if n % 11 == 0 {
                    "needle\n".into()
                } else {
                    format!("{wide}\n")
                }
            })
            .collect();
        std::fs::write(dir.join("c.txt"), text).unwrap();
        let page = run(&dir, json!({"pattern": "needle", "context": 5}));
        let (body, note) = page.rsplit_once('\n').unwrap();
        assert!(body.len() <= PAGE_BYTES, "{}", body.len());
        assert_eq!(note, format!("… {}", more(1)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_full_search_page_is_never_cut_in_the_middle() {
        // Long names and long lines fill every page, which carries every
        // note it can: the pattern searched as plain text, the paging note
        // and the secret left out.
        let text = format!("x({}\n", "y".repeat(LINE_CHARS)).repeat(3);
        let names: Vec<String> = (0..120)
            .map(|n| format!("{n:03}-{}.txt", "long".repeat(20)))
            .collect();
        let mut files: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), &*text)).collect();
        files.push((".env", "x("));
        let dir = tree("never-cut", &files);
        for (mode, total) in [("content", 360), ("files", 120), ("count", 120)] {
            let entry = |line: &str| match mode {
                "content" => line.contains(": x("),
                "files" => line.ends_with(".txt"),
                _ => line.ends_with(".txt: 3"),
            };
            let (mut offset, mut pages, mut shown) = (0, 0, 0);
            loop {
                let args = json!({"pattern": "x(", "mode": mode, "limit": 500, "offset": offset});
                let page = run(&dir, args);
                // A tool result this long goes to the model whole, where a
                // longer one loses its middle while the offset skips past it.
                assert!(
                    page.len() <= crate::chat::MAX_TOOL_CHARS,
                    "{mode}: {}",
                    page.len()
                );
                assert!(page.starts_with("`x(` is not a regex"), "{page}");
                assert!(
                    page.ends_with("left out: 1. Ask the user to paste what you need from them.")
                );
                shown += page.lines().filter(|l| entry(l)).count();
                pages += 1;
                let next = page.split_once("pass offset ").map(|(_, rest)| {
                    let digits = rest.split(' ').next().unwrap();
                    digits.parse::<usize>().unwrap()
                });
                let Some(next) = next else {
                    break;
                };
                offset = next;
            }
            assert!(pages > 1, "{mode} came in one page");
            assert_eq!(shown, total, "{mode}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_pattern_meant_as_plain_text_is_searched_as_such() {
        let dir = tree("literal", &[("f.rs", "call(a)\nprice: $cost\n")]);
        let plain = |pattern: &str, why: &str| {
            format!(
                "`{pattern}` is not a regex this search can run ({why}), so it was searched as \
                 plain text."
            )
        };
        assert_eq!(
            run(&dir, json!({"pattern": "call("})),
            plain("call(", "unclosed group") + "\nf.rs\n  1: call(a)"
        );
        assert_eq!(
            run(&dir, json!({"pattern": "$cost"})),
            "Nothing matches `$cost` as a regex; these match it as plain text.\nf.rs\n  2: \
             price: $cost"
        );
        assert_eq!(run(&dir, json!({"query": "c.ll"})), "f.rs\n  1: call(a)");
        assert_eq!(
            run(&dir, json!({"pattern": "c.ll\\("})),
            "f.rs\n  1: call(a)"
        );
        assert_eq!(
            run(&dir, json!({"pattern": "nowhere("})),
            plain("nowhere(", "unclosed group") + "\nNo matches."
        );
        // The model is told why, so it can write the pattern another way.
        let out = run(&dir, json!({"pattern": "(?=call)"}));
        assert!(out.contains("look-around"), "{out}");
        let out = run(&dir, json!({"pattern": r"\w{100}"}));
        let too_big = format!("it compiles past {REGEX_BYTES} bytes");
        assert!(out.starts_with(&plain(r"\w{100}", &too_big)), "{out}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_long_line_is_cut_around_its_match() {
        let line = format!("{}needle{}", "a".repeat(1000), "b".repeat(1000));
        let dir = tree("long", &[("min.js", &line)]);
        let out = run(&dir, json!({"pattern": "needle"}));
        let tail = "b".repeat(LINE_CHARS - LEAD_CHARS - "needle".len());
        assert_eq!(
            out,
            format!("min.js\n  1: …{}needle{tail}…", "a".repeat(LEAD_CHARS))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hidden_entries_outside_the_workspace_stay_out_whatever_asks_for_them() {
        let dir = tree(
            "hidden",
            &[
                ("notes.txt", "token here"),
                (".zshrc", "export API_KEY=token"),
                (".config/gh/hosts.yml", "oauth_token: x"),
                ("proj/.tokenrc", "token"),
                (".ignore", "!.config/\n!.tokenrc\n!.zshrc\n"),
            ],
        );
        let outside = |args: Value| {
            let query = Query::from_args(&args, &dir, None).unwrap();
            search(&query, Instant::now() + SEARCH_TIME, SEARCH_FILES)
                .unwrap()
                .0
        };
        let given = Path::new(".").join("notes.txt").display().to_string();
        // Neither a type that lists dotfiles nor an ignore file's `!` brings
        // them back.
        assert_eq!(
            outside(json!({"pattern": "token|api_key"})),
            format!("{given}\n  1: token here")
        );
        assert_eq!(
            outside(json!({"pattern": "token", "type": "sh"})),
            "No matches."
        );
        assert_eq!(
            outside(json!({"mode": "files", "type": "zsh"})),
            "No matches."
        );
        // Inside the workspace they are the project's own.
        assert_eq!(
            run(&dir, json!({"pattern": "api_key", "type": "sh"})),
            ".zshrc\n  1: export API_KEY=token"
        );
        // The walk never filters its root; the tool refuses one like this.
        assert!(hidden_below(&dir, &dir.join(".config").join("gh")));
        assert!(hidden_below(&dir, &dir.join(".zshrc")));
        assert!(!hidden_below(&dir, &dir.join("proj")));
        assert!(!hidden_below(&dir, &dir));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ignore_files_count_from_the_repository_or_the_searched_folder_down() {
        let dir = tree(
            "parents",
            &[
                (".gitignore", "*\n"),
                ("loose/src/a.rs", "needle"),
                ("repo/.gitignore", "gen/\n*.log\n"),
                ("repo/src/a.rs", "needle"),
                ("repo/src/b.log", "needle"),
                ("repo/gen/b.rs", "needle"),
            ],
        );
        std::fs::create_dir(dir.join("repo/.git")).unwrap();
        // A `*` above the searched folder or the repository empties nothing.
        let loose = dir.join("loose");
        assert_eq!(
            run(&loose, json!({"pattern": "needle"})),
            sep("src/a.rs\n  1: needle")
        );
        let repo = dir.join("repo");
        assert_eq!(
            run(&repo, json!({"pattern": "needle"})),
            sep("src/a.rs\n  1: needle")
        );
        // Below a repository's root, its `.gitignore` still counts.
        let src = repo.join("src");
        assert_eq!(run(&src, json!({"pattern": "needle"})), "a.rs\n  1: needle");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_folder_hidden_by_a_parent_gitignore_is_named_in_the_footer() {
        let dir = tree(
            "above",
            &[
                (
                    ".gitignore",
                    "data/\nscripts/\nlogs/\nbuild/\ncache/\ntmp/\n__pycache__/\n",
                ),
                ("run/in/z.txt", "needle"),
                ("run/in/data/d.txt", "needle"),
                ("run/in/cache/q.txt", "needle"),
                ("run/data/x.csv", "needle"),
                ("run/scripts/y.py", "needle"),
                ("run/.gitignore", "!logs/\ncache/\n"),
                ("run/.ignore", "tmp/\n"),
                ("run/logs/l.txt", "needle"),
                ("run/cache/c.txt", "needle"),
                ("run/tmp/t.txt", "needle"),
                ("run/__pycache__/m.txt", "needle"),
                ("run/build/b.txt", "needle"),
                ("run/vendor/.gitignore", "data/\n"),
                ("run/vendor/data/v.txt", "needle"),
            ],
        );
        std::fs::create_dir(dir.join(".git")).unwrap();
        std::fs::create_dir(dir.join("run/vendor/.git")).unwrap();
        let run_dir = dir.join("run");
        let named = |folders: &str| {
            format!(
                "… Left out by a .gitignore above the project: {}. Name a folder as the \
                 path to search it.",
                sep(folders)
            )
        };
        // The project's own ignore files let logs/ back in and leave cache/
        // and tmp/ out themselves, a repository inside it reads only its own,
        // and build output and caches are not worth a name.
        let hidden = "data/, in/data/, scripts/";
        assert_eq!(
            run(&run_dir, json!({"pattern": "needle"})),
            sep("in/z.txt\n  1: needle\nlogs/l.txt\n  1: needle\n") + &named(hidden)
        );
        assert_eq!(
            run(&run_dir, json!({"mode": "files", "glob": "*.txt"}))
                .lines()
                .last(),
            Some(named(hidden).as_str())
        );
        // The project's `.gitignore` above a folder searched inside it counts
        // all the same.
        let args = json!({"pattern": "needle", "path": "in"});
        let inside = run_dir.join("in");
        let query = Query::from_args(&args, &inside, Some(&run_dir)).unwrap();
        assert_eq!(
            search(&query, Instant::now() + SEARCH_TIME, SEARCH_FILES)
                .unwrap()
                .0,
            sep("in/z.txt\n  1: needle\n") + &named("in/data/")
        );
        // Named as the path, the folder is searched.
        let args = json!({"pattern": "needle", "path": "data"});
        let data = run_dir.join("data");
        let query = Query::from_args(&args, &data, Some(&run_dir)).unwrap();
        assert_eq!(
            search(&query, Instant::now() + SEARCH_TIME, SEARCH_FILES)
                .unwrap()
                .0,
            sep("data/x.csv\n  1: needle")
        );
        // At the repository's root the `.gitignore` is the project's own.
        assert_eq!(
            run(&dir, json!({"pattern": "needle", "mode": "count"})),
            sep("run/in/z.txt: 1\nrun/logs/l.txt: 1\n… 2 matching lines in 2 files.")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_folder_whose_files_the_rules_above_hide_too_comes_with_a_warning() {
        let dir = tree(
            "inside",
            &[(".gitignore", "*\n"), ("run/f00/a.txt", "needle")],
        );
        std::fs::create_dir(dir.join(".git")).unwrap();
        assert_eq!(
            run(&dir.join("run"), json!({"pattern": "needle"})),
            sep(
                "No matches.\n… Left out by a .gitignore above the project: f00/. Name a \
                 folder as the path to search it, though the same rules hide files inside \
                 too; list_dir shows all it holds."
            )
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn nothing_is_named_once_the_time_is_up() {
        let dir = tree(
            "late",
            &[(".gitignore", "data/\n"), ("run/data/x.csv", "needle")],
        );
        std::fs::create_dir(dir.join(".git")).unwrap();
        let run_dir = dir.join("run");
        let args = json!({"pattern": "needle"});
        let query = Query::from_args(&args, &run_dir, Some(&run_dir)).unwrap();
        let (above, dirs) = (ignores_above(&query), BTreeSet::from([run_dir.clone()]));
        let later = Instant::now() + SEARCH_TIME;
        assert_eq!(
            left_out(&query, &above, &dirs, later),
            (vec![run_dir.join("data")], false)
        );
        assert_eq!(
            left_out(&query, &above, &dirs, Instant::now()),
            (Vec::new(), false)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn only_the_first_few_hidden_folders_are_named() {
        let mut files = vec![(".gitignore", "*/\n")];
        let names: Vec<String> = (0..LEFT_OUT_NAMES + 2)
            .map(|n| format!("run/f{n:02}/a.txt"))
            .collect();
        files.extend(names.iter().map(|name| (name.as_str(), "needle")));
        let dir = tree("many", &files);
        std::fs::create_dir(dir.join(".git")).unwrap();
        let out = run(&dir.join("run"), json!({"pattern": "needle"}));
        let shown: Vec<String> = (0..LEFT_OUT_NAMES)
            .map(|n| sep(&format!("f{n:02}/")))
            .collect();
        assert_eq!(
            out,
            format!(
                "No matches.\n… Left out by a .gitignore above the project: {} and 2 more. \
                 Name a folder as the path to search it.",
                shown.join(", ")
            )
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn build_folders_are_skipped_outside_a_repository_unless_the_glob_names_them() {
        let files = [
            ("src/a.js", "needle"),
            ("build/a.js", "needle"),
            ("dist/a.js", "needle"),
            ("target/a.js", "needle"),
            (".next/a.js", "needle"),
        ];
        let dir = tree("builds", &files);
        assert_eq!(
            run(&dir, json!({"pattern": "needle", "mode": "files"})),
            sep("src/a.js")
        );
        assert_eq!(
            run(
                &dir,
                json!({"pattern": "needle", "mode": "files", "glob": "build/**"})
            ),
            sep("build/a.js")
        );
        // In a repository `.gitignore` decides, and here it says nothing.
        std::fs::create_dir(dir.join(".git")).unwrap();
        let all = run(&dir, json!({"pattern": "needle", "mode": "count"}));
        assert!(all.ends_with("5 matching lines in 5 files."), "{all}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn letronnas_own_folder_is_never_searched() {
        let dir = tree("data", &[("a.txt", "needle"), ("data/keys.json", "needle")]);
        let args = json!({"pattern": "needle", "glob": "*"});
        let mut query = Query::from_args(&args, &dir, Some(&dir)).unwrap();
        query.data = Some(dir.join("data"));
        assert_eq!(
            search(&query, Instant::now() + SEARCH_TIME, SEARCH_FILES)
                .unwrap()
                .0,
            "a.txt\n  1: needle"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_slow_pattern_is_stopped_inside_a_file() {
        // The review's pattern, kept small: `[ab]{60}` never outgrows the
        // fast engine, so the test is quick; the big ones cost seconds a MB.
        let regex = build(r"a[ab]{60}[^ab\n]", false).unwrap();
        let (plain, hit) = ("ab".repeat(50), format!("a{}c", "b".repeat(60)));
        let mut lines = vec![plain; 1000];
        for n in [3, 162, 163, 999] {
            lines[n] = hit.clone();
        }
        let bytes = lines.join("\n").into_bytes();
        assert!(bytes.len() > 4 * CHUNK_BYTES);
        // Lines 162 and 163 sit either side of the first chunk's end.
        let far = Instant::now() + SEARCH_TIME;
        assert_eq!(hits(&bytes, &regex, far), Some(vec![3, 162, 163, 999]));
        assert_eq!(hits(&bytes, &regex, Instant::now()), None);
        // One line many chunks long is timed on its first chunk and left
        // when the rest would not finish in time.
        let long = "ab".repeat(4 * CHUNK_BYTES);
        assert!(!too_slow(long.as_bytes(), &regex, far));
        assert!(too_slow(long.as_bytes(), &regex, Instant::now()));
        assert!(!too_slow(&bytes[..CHUNK_BYTES], &regex, Instant::now()));
    }

    #[test]
    fn utf16_text_is_searched_and_a_late_nul_marks_a_binary() {
        let dir = tree("utf16", &[]);
        let mut utf16 = vec![0xFF, 0xFE];
        utf16.extend(
            "first\r\nthe needle\r\n"
                .encode_utf16()
                .flat_map(u16::to_le_bytes),
        );
        std::fs::write(dir.join("ps.log"), utf16).unwrap();
        let mut late = format!("needle\n{}", "x".repeat(10_000)).into_bytes();
        late.push(0);
        std::fs::write(dir.join("late.bin"), late).unwrap();
        assert_eq!(
            run(&dir, json!({"pattern": "needle"})),
            "ps.log\n  2: the needle"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
