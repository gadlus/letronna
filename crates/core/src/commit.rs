//! Staging and committing the project's changes, for the Changes tab.
//!
//! `git.rs` only looks; this changes the index and the history, so it is
//! kept apart. Every call is git as a child process with a deadline, which
//! a front end runs off its own thread: a hook that hangs, or a remote that
//! waits for a password, ends in an error that says so rather than a window
//! that stops. Outside a git work tree each call refuses before running
//! anything, and the tab keeps to what the session changed.
//!
//! Reading and staging run with the read-only guards (`command::GUARDS`), as
//! `git.rs` does. Committing and pushing do not: they are what the user
//! would type themselves, so the repository's hooks and the user's signing
//! run as they would in a terminal. A push goes through the approval gate
//! as the command it is, and is never forced.

use crate::chat::{Connection, ToolCall};
use crate::gate::{self, Gate, Outcome};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long a look or a stage may take: these touch only the index.
const LOCAL: Duration = Duration::from_secs(30);

/// How long a commit or a push may take, hooks included.
const SLOW: Duration = Duration::from_secs(120);

/// The most of what git prints that is kept: status and one file's diff.
const OUTPUT_CAP: usize = 4 * 1024 * 1024;

/// The most of the staged diff a suggestion reads. A message is about the
/// shape of a change, which fits in far less than a vendored tree.
const SUGGEST_CAP: usize = 48 * 1024;

/// Where a commit message's lines are wrapped, as `git log` reads well.
const WRAP: usize = 72;

/// What happened to a file, as git's status letters say it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
    /// Both sides changed it in a merge; staging it marks it resolved.
    Conflicted,
}

impl Kind {
    /// The one letter the tab shows beside the file.
    pub fn letter(self) -> &'static str {
        match self {
            Kind::Added => "A",
            Kind::Modified => "M",
            Kind::Deleted => "D",
            Kind::Renamed => "R",
            Kind::Copied => "C",
            Kind::TypeChanged => "T",
            Kind::Untracked => "?",
            Kind::Conflicted => "U",
        }
    }

    fn of(letter: char) -> Option<Kind> {
        Some(match letter {
            'A' => Kind::Added,
            'M' => Kind::Modified,
            'D' => Kind::Deleted,
            'R' => Kind::Renamed,
            'C' => Kind::Copied,
            'T' => Kind::TypeChanged,
            _ => return None,
        })
    }
}

/// One file in the staged or the unstaged group.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// Relative to the project folder, `../` and all when it is outside.
    pub path: PathBuf,
    /// The name it had, for a rename or a copy.
    pub from: Option<PathBuf>,
    pub kind: Kind,
}

/// The work tree as the tab groups it: what the next commit takes, and
/// what it leaves.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tree {
    pub staged: Vec<Entry>,
    /// Changed since it was staged, or never staged; untracked files too.
    pub unstaged: Vec<Entry>,
}

/// The staged and unstaged changes in `folder`'s work tree.
pub fn tree(folder: &Path) -> Result<Tree, String> {
    work_tree(folder)?;
    let args = [
        "--no-optional-locks",
        "status",
        "--porcelain=v2",
        "-z",
        "--untracked-files=all",
    ];
    let out = look(folder, &args)?;
    let prefix = look(folder, &["rev-parse", "--show-prefix"])?;
    let mut tree = parse_tree(&out);
    for entry in tree.staged.iter_mut().chain(tree.unstaged.iter_mut()) {
        entry.path = from_folder(&entry.path, &prefix);
        entry.from = entry.from.as_ref().map(|from| from_folder(from, &prefix));
    }
    Ok(tree)
}

/// `path`, named from the top of the repository as git's porcelain always
/// does, named instead from the folder at `prefix` below the top, so the
/// tab and every pathspec given from there agree.
fn from_folder(path: &Path, prefix: &str) -> PathBuf {
    let prefix = Path::new(prefix.trim_end_matches('\n'));
    if let Ok(inside) = path.strip_prefix(prefix) {
        return inside.to_owned();
    }
    let up = prefix.components().count();
    let mut out: PathBuf = std::iter::repeat_n("..", up).collect();
    out.push(path);
    out
}

fn parse_tree(out: &str) -> Tree {
    let mut tree = Tree::default();
    let mut fields = out.split('\0').filter(|field| !field.is_empty());
    while let Some(field) = fields.next() {
        // The fields before the path, which may itself hold spaces.
        let before = match field.as_bytes().first() {
            Some(b'1') => 8,
            Some(b'2') => 9,
            Some(b'u') => 10,
            Some(b'?') => {
                tree.unstaged.push(Entry {
                    path: PathBuf::from(&field[2..]),
                    from: None,
                    kind: Kind::Untracked,
                });
                continue;
            }
            _ => continue,
        };
        let parts: Vec<&str> = field.splitn(before + 1, ' ').collect();
        let (Some(xy), Some(path)) = (parts.get(1), parts.get(before)) else {
            continue;
        };
        let path = PathBuf::from(path);
        // A rename's old name follows as a field of its own.
        let from = (before == 9)
            .then(|| fields.next().map(PathBuf::from))
            .flatten();
        if field.starts_with('u') {
            tree.unstaged.push(Entry {
                path,
                from: None,
                kind: Kind::Conflicted,
            });
            continue;
        }
        let mut letters = xy.chars();
        let (x, y) = (letters.next(), letters.next());
        if let Some(kind) = x.and_then(Kind::of) {
            tree.staged.push(Entry {
                path: path.clone(),
                from: from.clone(),
                kind,
            });
        }
        if let Some(kind) = y.and_then(Kind::of) {
            tree.unstaged.push(Entry {
                path,
                from: None,
                kind,
            });
        }
    }
    tree
}

/// One file's staged or unstaged diff, cut into hunks that can be staged
/// or unstaged one at a time.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileDiff {
    pub path: PathBuf,
    /// Staged against the last commit, or unstaged against the index.
    pub staged: bool,
    /// Diffed against nothing: it is staged whole, not by hunks.
    pub untracked: bool,
    /// The lines before the first hunk, which name the file to `git apply`.
    pub header: String,
    /// Each starts at its `@@` line.
    pub hunks: Vec<String>,
}

impl FileDiff {
    /// The whole diff, as git printed it.
    pub fn text(&self) -> String {
        let mut text = self.header.clone();
        self.hunks.iter().for_each(|hunk| text.push_str(hunk));
        text
    }
}

/// `entry`'s diff on the staged or the unstaged side.
pub fn file_diff(folder: &Path, entry: &Entry, staged: bool) -> Result<FileDiff, String> {
    work_tree(folder)?;
    let path = entry.path.display().to_string();
    let text = if entry.kind == Kind::Untracked {
        // A difference is exit status 1 here.
        let args = [
            "diff",
            "--no-index",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--",
            "/dev/null",
            path.as_str(),
        ];
        run(folder, &args, None, Guards::On, LOCAL, &[0, 1])?
    } else {
        let mut args = vec![
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--no-relative",
            "--src-prefix=a/",
            "--dst-prefix=b/",
        ];
        if staged {
            args.push("--cached");
        }
        args.push("--");
        if let Some(from) = entry.from.as_ref().filter(|_| staged) {
            args.push(from.to_str().unwrap_or_default());
        }
        args.push(path.as_str());
        look(folder, &args)?
    };
    let mut diff = split(&text, &entry.path, staged);
    diff.untracked = entry.kind == Kind::Untracked;
    Ok(diff)
}

/// Cut `text`, one file's diff, at its hunks.
fn split(text: &str, path: &Path, staged: bool) -> FileDiff {
    let mut diff = FileDiff {
        path: path.to_owned(),
        staged,
        ..Default::default()
    };
    for line in text.split_inclusive('\n') {
        match diff.hunks.last_mut() {
            _ if line.starts_with("@@ ") => diff.hunks.push(line.to_owned()),
            Some(hunk) => hunk.push_str(line),
            None => diff.header.push_str(line),
        }
    }
    diff
}

/// Put `entry` in the index as it is now: a new file, an edit or a delete.
pub fn stage(folder: &Path, entry: &Entry) -> Result<(), String> {
    work_tree(folder)?;
    let path = entry.path.display().to_string();
    look(folder, &["add", "--all", "--", path.as_str()]).map(drop)
}

/// Take `entry` back out of the index, as it was at the last commit; the
/// file itself is left as it is.
pub fn unstage(folder: &Path, entry: &Entry) -> Result<(), String> {
    work_tree(folder)?;
    let mut paths = vec![entry.path.display().to_string()];
    paths.extend(entry.from.as_ref().map(|from| from.display().to_string()));
    let mut args = if born(folder) {
        vec!["reset", "--quiet", "HEAD", "--"]
    } else {
        // Before the first commit there is nothing to reset to.
        vec!["rm", "--cached", "--quiet", "-r", "--"]
    };
    args.extend(paths.iter().map(String::as_str));
    look(folder, &args).map(drop)
}

/// Stage hunk `at` of `diff`, an unstaged diff, and nothing else of it.
pub fn stage_hunk(folder: &Path, diff: &FileDiff, at: usize) -> Result<(), String> {
    if diff.staged {
        return Err("That hunk is staged already.".into());
    }
    if diff.untracked {
        // A new file is one hunk, named from here rather than from the top.
        let entry = Entry {
            path: diff.path.clone(),
            from: None,
            kind: Kind::Untracked,
        };
        return stage(folder, &entry);
    }
    apply(folder, diff, at, false)
}

/// Unstage hunk `at` of `diff`, a staged diff; the file keeps the change.
pub fn unstage_hunk(folder: &Path, diff: &FileDiff, at: usize) -> Result<(), String> {
    if !diff.staged {
        return Err("That hunk is not staged.".into());
    }
    apply(folder, diff, at, true)
}

/// Apply hunk `at` of `diff` to the index alone, or take it back out.
fn apply(folder: &Path, diff: &FileDiff, at: usize, reverse: bool) -> Result<(), String> {
    work_tree(folder)?;
    let Some(hunk) = diff.hunks.get(at) else {
        return Err("That hunk is no longer there; the diff has moved.".into());
    };
    let patch = format!("{}{hunk}", diff.header);
    // Bytes that are not UTF-8 were read as U+FFFD, and git would write that
    // into the index in their place.
    if patch.contains(char::REPLACEMENT_CHARACTER) {
        return Err("This file is not UTF-8 text, so it is staged whole, not by hunks.".into());
    }
    // The diff names files from the top of the repository, so it is applied
    // from there: from a folder below, git apply skips what is outside it.
    let top = look(folder, &["rev-parse", "--show-toplevel"])?;
    let mut args = vec!["apply", "--cached", "--whitespace=nowarn"];
    if reverse {
        args.push("--reverse");
    }
    args.push("-");
    run(
        Path::new(top.trim_end_matches('\n')),
        &args,
        Some(&patch),
        Guards::On,
        LOCAL,
        &[0],
    )
    .map(drop)
}

/// Commit what is staged, and only that, with `message`; says which commit
/// it made.
pub fn commit(folder: &Path, message: &str) -> Result<String, String> {
    work_tree(folder)?;
    if message.trim().is_empty() {
        return Err("Write a commit message first.".into());
    }
    if tree(folder)?.staged.is_empty() {
        return Err("Nothing is staged, so there is nothing to commit.".into());
    }
    // `--cleanup=strip` drops the comment lines an editor would have.
    let args = ["commit", "--quiet", "--cleanup=strip", "--file=-"];
    run(folder, &args, Some(message), Guards::Off, SLOW, &[0])?;
    let made = look(folder, &["log", "-1", "--format=%h %s"])?;
    Ok(format!("Committed {}.", made.trim()))
}

/// The staged changes as one patch for the model to read, cut at a size
/// with a line saying so.
pub fn staged_diff(folder: &Path) -> Result<String, String> {
    work_tree(folder)?;
    let args = [
        "diff",
        "--cached",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--stat=100",
        "--patch",
    ];
    let mut text = look(folder, &args)?;
    if text.len() > SUGGEST_CAP {
        let mut end = SUGGEST_CAP;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n… the rest of the diff is left out.\n");
    }
    Ok(text)
}

/// What the model is told a commit message is, in this project's style.
const HOUSE_STYLE: &str = "You write one git commit message for the staged diff you are given. \
Reply with the message alone: no code fence, no preamble.\n\
The first line is the subject: the outcome for someone using the program, in about 60 \
characters, with no type prefix such as feat: or fix:, no scope, and no trailing period.\n\
Then a blank line, then a body in plain prose paragraphs, no bullets or headings: what \
was missing, what happens now, and any trade-off made. Keep it short; a small change \
needs two or three sentences.";

/// A commit message for what is staged in `folder`, from one round with the
/// session's model and no tools, put in the house style however it came.
pub fn suggest(connection: &Connection, folder: &Path) -> Result<String, String> {
    let diff = staged_diff(folder)?;
    if diff.trim().is_empty() {
        return Err("Nothing is staged to write a message for.".into());
    }
    let endpoint = crate::chat::validate(connection)?;
    let body = suggest_body(connection, &diff);
    // The shared client would give up on a slow reply after its idle wait.
    let mut request = crate::chat::client()?
        .post(endpoint)
        .timeout(Duration::from_secs(60))
        .json(&body);
    if !connection.key.trim().is_empty() {
        request = request.bearer_auth(connection.key.trim());
    }
    let value: Value = request
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.json())
        .map_err(|e| format!("No message came back: {e}"))?;
    let text = crate::chat::unwrap_response(&value)?
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let message = house_style(text);
    if message.is_empty() {
        return Err("The model gave no message; write one instead.".into());
    }
    Ok(message)
}

/// The request for a suggestion. It sends no effort, which a model that
/// offers none may be refused for, and takes the fields this model is known
/// to want in place of others, as a turn's request does.
fn suggest_body(connection: &Connection, diff: &str) -> Value {
    let mut body = json!({
        "model": connection.model.trim(),
        "messages": [
            {"role": "system", "content": HOUSE_STYLE},
            {"role": "user", "content": format!("Staged diff:\n{diff}")},
        ],
        "stream": false,
        "max_tokens": 1500,
    });
    crate::chat::fit_quirks(connection, &mut body);
    body
}

/// `text` as a commit message in the house style: the subject without a
/// type prefix or a closing period, the body's paragraphs wrapped at 72.
pub fn house_style(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim_start().starts_with("```"))
        .collect();
    let mut lines = lines.into_iter().skip_while(|line| line.trim().is_empty());
    let Some(subject) = lines.next() else {
        return String::new();
    };
    let subject = subject.trim().trim_matches(['"', '`', '*']).trim();
    let subject = unprefixed(subject).trim_end_matches('.').trim_end();
    let mut message = subject.to_owned();
    let mut paragraph: Vec<&str> = Vec::new();
    let mut paragraphs: Vec<String> = Vec::new();
    for line in lines.chain([""]) {
        if line.trim().is_empty() {
            if !paragraph.is_empty() {
                paragraphs.push(wrap(&paragraph.join(" "), WRAP));
                paragraph.clear();
            }
        } else {
            paragraph.push(line.trim());
        }
    }
    if !paragraphs.is_empty() {
        message.push_str("\n\n");
        message.push_str(&paragraphs.join("\n\n"));
    }
    message
}

/// `subject` without a leading `feat:` or `fix(ui):`.
fn unprefixed(subject: &str) -> &str {
    let Some((head, rest)) = subject.split_once(": ") else {
        return subject;
    };
    let name = head.trim_end_matches('!');
    let name = match name.split_once('(') {
        Some((name, scope)) if scope.ends_with(')') => name,
        Some(_) => return subject,
        None => name,
    };
    let prefix = !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase());
    if prefix { rest.trim_start() } else { subject }
}

/// `text` broken between words into lines of at most `width` characters;
/// a word longer than that keeps a line of its own.
fn wrap(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut line = 0;
    for word in text.split_whitespace() {
        let len = word.chars().count();
        if line > 0 && line + 1 + len > width {
            out.push('\n');
            line = 0;
        } else if line > 0 {
            out.push(' ');
            line += 1;
        }
        out.push_str(word);
        line += len;
    }
    out
}

/// Where the current branch goes when it is pushed.
#[derive(Clone, Debug, PartialEq)]
pub struct Push {
    pub branch: String,
    pub remote: String,
    /// The branch on the remote, as `refs/heads/…`.
    pub upstream: String,
}

impl Push {
    /// The command it is, as the approval card and the gate read it.
    pub fn command(&self) -> String {
        format!("git push {} {}:{}", self.remote, self.branch, self.upstream)
    }

    /// The push as the `run_command` call it amounts to.
    pub fn call(&self) -> ToolCall {
        ToolCall {
            id: "commit-push".into(),
            name: "run_command".into(),
            arguments: json!({ "command": self.command() }).to_string(),
        }
    }

    /// What the session's permission mode, rules and allowances make of the
    /// push: run it, ask the user first, or not at all.
    pub fn decide(&self, gate: &Gate) -> Outcome {
        gate::decide(&self.call(), gate)
    }
}

/// The current branch and the upstream it pushes to, or why there is none.
pub fn plan_push(folder: &Path) -> Result<Push, String> {
    work_tree(folder)?;
    let Ok(branch) = look(folder, &["symbolic-ref", "--quiet", "--short", "HEAD"]) else {
        return Err("The head is detached, so there is no branch to push.".into());
    };
    let branch = branch.trim().to_owned();
    let setting = |name: &str| {
        let key = format!("branch.{branch}.{name}");
        look(folder, &["config", "--get", key.as_str()])
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    };
    let (Some(remote), Some(upstream)) = (setting("remote"), setting("merge")) else {
        return Err(format!(
            "{branch} has no upstream to push to. Push it once with git push -u, \
             then this can."
        ));
    };
    if remote == "." {
        return Err(format!(
            "{branch} follows another local branch, so there is no remote to push to."
        ));
    }
    Ok(Push {
        branch,
        remote,
        upstream,
    })
}

/// Push `push`, which the gate has allowed; never forced. A rejection is
/// an error that says why in plain words.
pub fn push(folder: &Path, push: &Push) -> Result<String, String> {
    work_tree(folder)?;
    let refspec = format!("refs/heads/{}:{}", push.branch, push.upstream);
    let args = [
        "push",
        "--porcelain",
        push.remote.as_str(),
        refspec.as_str(),
    ];
    let out = match run_both(folder, &args, None, Guards::Off, SLOW) {
        Ok((0, out, _)) => out,
        Ok((_, out, errors)) => return Err(rejected(push, &out, &errors)),
        Err(why) => return Err(why),
    };
    let current = out.lines().any(|line| line.starts_with("=\t"));
    Ok(if current {
        format!("{} was already on {}.", push.branch, push.remote)
    } else {
        format!("Pushed {} to {}.", push.branch, push.remote)
    })
}

/// Why a push failed, from git's porcelain lines or else its first error.
fn rejected(push: &Push, out: &str, errors: &str) -> String {
    let reason = out.lines().find_map(|line| {
        let mut parts = line.split('\t');
        (parts.next() == Some("!")).then(|| parts.nth(1).unwrap_or_default().to_owned())
    });
    match reason {
        Some(reason) if reason.contains("fetch first") || reason.contains("non-fast-forward") => {
            format!(
                "The push was rejected: {} on {} has commits this branch does not. \
                 Pull them in, then push again.",
                push.upstream.trim_start_matches("refs/heads/"),
                push.remote
            )
        }
        Some(reason) => format!("The push was rejected: {}.", reason.trim()),
        None => {
            let why = errors
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty() && !line.starts_with("hint:"))
                .unwrap_or("it failed");
            format!("The push did not go through: {why}")
        }
    }
}

/// Ok in a git work tree, or an error saying there is nothing to commit.
fn work_tree(folder: &Path) -> Result<(), String> {
    let inside =
        look(folder, &["rev-parse", "--is-inside-work-tree"]).is_ok_and(|out| out.trim() == "true");
    if !inside {
        return Err(format!(
            "{} is not in a git work tree, so there is nothing to stage or commit.",
            folder.display()
        ));
    }
    Ok(())
}

/// Whether the repository has a commit yet.
fn born(folder: &Path) -> bool {
    look(
        folder,
        &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
    )
    .is_ok()
}

/// Whether a call runs with the read-only guards.
#[derive(Clone, Copy, PartialEq)]
enum Guards {
    On,
    Off,
}

/// Run a guarded git that only reads or stages, within the local deadline.
fn look(folder: &Path, args: &[&str]) -> Result<String, String> {
    run(folder, args, None, Guards::On, LOCAL, &[0])
}

/// Run git, failing with its first line of error unless it exits with one
/// of `fine`.
fn run(
    folder: &Path,
    args: &[&str],
    input: Option<&str>,
    guards: Guards,
    deadline: Duration,
    fine: &[i32],
) -> Result<String, String> {
    let (code, out, errors) = run_both(folder, args, input, guards, deadline)?;
    if !fine.contains(&code) {
        // A hook's own complaint, or git's "nothing to commit", may be on
        // either stream.
        let why = errors
            .lines()
            .chain(out.lines())
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with("hint:"))
            .unwrap_or("it failed");
        return Err(format!("git {} failed: {why}", name(args)));
    }
    Ok(out)
}

/// The subcommand of `args`, not an option given before it.
fn name<'a>(args: &[&'a str]) -> &'a str {
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        if !arg.starts_with('-') {
            return arg;
        }
        at += if *arg == "-c" { 2 } else { 1 };
    }
    ""
}

/// Run git in `folder` with `input` on its stdin, and give back its exit
/// code with what it printed on each stream; killed at `deadline`.
fn run_both(
    folder: &Path,
    args: &[&str],
    input: Option<&str>,
    guards: Guards,
    deadline: Duration,
) -> Result<(i32, String, String), String> {
    let mut cmd = Command::new("git");
    cmd.args([
        "--no-pager",
        "--literal-pathspecs",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.quotePath=false",
        "-c",
        "color.ui=false",
    ])
    .args(args)
    .current_dir(folder)
    // A remote or a signing key that wants a password would wait forever
    // for an answer nobody can type.
    .env("GIT_TERMINAL_PROMPT", "0")
    .env_remove("GIT_EXTERNAL_DIFF")
    .stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    if guards == Guards::On {
        cmd.envs(crate::command::GUARDS.iter().copied());
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("git could not be run: {e}"))?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        return Err("git gave no output to read.".into());
    };
    let out = reader(stdout);
    let errors = reader(stderr);
    if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
        // One that exits without reading it all says why on stderr.
        let _ = stdin.write_all(text.as_bytes());
    }
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "git {} took longer than {}s and was stopped.",
                    name(args),
                    deadline.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(format!("git did not finish: {e}")),
        }
    };
    // Git has written all it will by now; a process a hook left running in
    // the background may hold the pipes open for good, and is not waited on.
    let out = out.recv_timeout(AFTER_EXIT).unwrap_or_default();
    let errors = errors.recv_timeout(AFTER_EXIT).unwrap_or_default();
    Ok((status.code().unwrap_or(-1), out, errors))
}

/// How long what git printed is waited for once git itself has exited.
const AFTER_EXIT: Duration = Duration::from_secs(2);

/// `stream` read to its end on a thread of its own, sent back when done.
fn reader(stream: impl Read + Send + 'static) -> mpsc::Receiver<String> {
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || send.send(drain(stream)));
    receive
}

/// All of `stream` read to its end, keeping the first [`OUTPUT_CAP`] bytes;
/// the rest is read and dropped so git never waits on a full pipe.
fn drain(stream: impl Read) -> String {
    let mut bytes = Vec::new();
    let mut stream = stream;
    let _ = (&mut stream)
        .take(OUTPUT_CAP as u64)
        .read_to_end(&mut bytes);
    let _ = std::io::copy(&mut stream, &mut std::io::sink());
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::{Grants, Mode};
    use crate::tools::ToolContext;

    /// A folder of its own for one test, under the system's temp folder.
    fn scratch(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("letronna-commit-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// Run git in `folder` as a test sets a repository up, ignoring the
    /// user's own config.
    fn setup(folder: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(folder)
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "nul" } else { "/dev/null" },
            )
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Who commits in a test repository, set in it so that the commits made
    /// here do not reach for the user's own name.
    fn identity(folder: &Path) {
        setup(folder, &["config", "user.name", "t"]);
        setup(folder, &["config", "user.email", "t@t"]);
        setup(folder, &["config", "commit.gpgsign", "false"]);
    }

    /// A repository with one committed file of ten lines.
    fn repo(name: &str) -> PathBuf {
        let folder = scratch(name);
        setup(&folder, &["init", "-q", "-b", "main"]);
        identity(&folder);
        std::fs::write(folder.join("a.txt"), lines(|_| None)).unwrap();
        setup(&folder, &["add", "a.txt"]);
        setup(&folder, &["commit", "-q", "-m", "first"]);
        folder
    }

    /// Ten numbered lines, with those `change` gives replaced.
    fn lines(change: impl Fn(usize) -> Option<&'static str>) -> String {
        (1..=10)
            .map(|n| format!("{}\n", change(n).map_or(n.to_string(), str::to_owned)))
            .collect()
    }

    fn paths(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| entry.path.display().to_string())
            .collect()
    }

    #[test]
    fn status_lines_are_grouped_with_renames_and_spaces_kept() {
        let out = "1 M. N... 100644 100644 100644 a b staged only.txt\0\
                   1 .M N... 100644 100644 100644 a b edited.txt\0\
                   1 MM N... 100644 100644 100644 a b both.txt\0\
                   2 R. N... 100644 100644 100644 a b R100 new name.txt\0old name.txt\0\
                   u UU N... 100644 100644 100644 100644 a b c clash.txt\0\
                   ? loose file.txt\0";
        let tree = parse_tree(out);
        assert_eq!(
            paths(&tree.staged),
            ["staged only.txt", "both.txt", "new name.txt"]
        );
        assert_eq!(
            paths(&tree.unstaged),
            ["edited.txt", "both.txt", "clash.txt", "loose file.txt"]
        );
        let renamed = &tree.staged[2];
        assert_eq!(renamed.kind, Kind::Renamed);
        assert_eq!(renamed.from, Some(PathBuf::from("old name.txt")));
        assert_eq!(tree.unstaged[2].kind, Kind::Conflicted);
        assert_eq!(tree.unstaged[3].kind, Kind::Untracked);
    }

    #[test]
    fn paths_are_named_from_the_project_folder() {
        let from = |path: &str, prefix: &str| from_folder(Path::new(path), prefix);
        assert_eq!(from("a b.txt", ""), PathBuf::from("a b.txt"));
        assert_eq!(from("sub/a.txt", "sub/"), PathBuf::from("a.txt"));
        assert_eq!(
            from("top.txt", "sub/deeper/"),
            PathBuf::from("../../top.txt")
        );
        assert_eq!(from("other/x", "sub/"), PathBuf::from("../other/x"));
    }

    #[test]
    fn a_real_rename_and_a_path_with_spaces_are_read_from_git() {
        let folder = repo("rename");
        setup(&folder, &["mv", "a.txt", "b c.txt"]);
        std::fs::write(folder.join("d e.txt"), "x\n").unwrap();
        let tree = tree(&folder).unwrap();
        assert_eq!(
            tree.staged,
            vec![Entry {
                path: "b c.txt".into(),
                from: Some("a.txt".into()),
                kind: Kind::Renamed,
            }]
        );
        assert_eq!(paths(&tree.unstaged), ["d e.txt"]);
        // And the rename unstages whole, both names.
        unstage(&folder, &tree.staged[0]).unwrap();
        let after = super::tree(&folder).unwrap();
        assert!(after.staged.is_empty(), "{after:?}");
    }

    #[test]
    fn one_hunk_is_staged_and_unstaged_alone() {
        let folder = repo("hunk");
        let edited = lines(|n| match n {
            1 => Some("one"),
            10 => Some("ten"),
            _ => None,
        });
        std::fs::write(folder.join("a.txt"), &edited).unwrap();
        let entry = tree(&folder).unwrap().unstaged.remove(0);
        let diff = file_diff(&folder, &entry, false).unwrap();
        assert_eq!(diff.hunks.len(), 2, "{}", diff.text());

        stage_hunk(&folder, &diff, 1).unwrap();
        let staged = setup(&folder, &["diff", "--cached"]);
        assert!(staged.contains("+ten"), "{staged}");
        assert!(!staged.contains("+one"), "{staged}");
        let left = setup(&folder, &["diff"]);
        assert!(left.contains("+one") && !left.contains("+ten"), "{left}");

        let entry = tree(&folder).unwrap().staged.remove(0);
        let diff = file_diff(&folder, &entry, true).unwrap();
        assert_eq!(diff.hunks.len(), 1, "{}", diff.text());
        unstage_hunk(&folder, &diff, 0).unwrap();
        assert_eq!(setup(&folder, &["diff", "--cached"]), "");
        // The file itself keeps both edits throughout.
        assert_eq!(
            std::fs::read_to_string(folder.join("a.txt")).unwrap(),
            edited
        );
    }

    #[test]
    fn a_hunk_that_is_not_utf8_is_refused_rather_than_changed() {
        let folder = repo("latin1");
        let rest = lines(|n| (n == 10).then_some("ten"));
        let mut edited = b"caf\xe9\n".to_vec();
        edited.extend_from_slice(rest.split_once('\n').unwrap().1.as_bytes());
        std::fs::write(folder.join("a.txt"), &edited).unwrap();
        let entry = tree(&folder).unwrap().unstaged.remove(0);
        let diff = file_diff(&folder, &entry, false).unwrap();
        assert_eq!(diff.hunks.len(), 2, "{}", diff.text());
        let why = stage_hunk(&folder, &diff, 0).unwrap_err();
        assert!(why.contains("not UTF-8"), "{why}");
        assert_eq!(setup(&folder, &["diff", "--cached"]), "");
        // The hunk that is plain text still stages.
        stage_hunk(&folder, &diff, 1).unwrap();
        let staged = setup(&folder, &["diff", "--cached"]);
        assert!(
            staged.contains("+ten") && !staged.contains("caf"),
            "{staged}"
        );
    }

    #[test]
    fn a_hunk_of_a_file_with_spaces_in_a_folder_below_is_staged() {
        let folder = repo("below");
        let below = folder.join("sub dir");
        std::fs::create_dir(&below).unwrap();
        std::fs::write(below.join("f g.txt"), lines(|_| None)).unwrap();
        setup(&folder, &["add", "."]);
        setup(&folder, &["commit", "-q", "-m", "second"]);
        std::fs::write(below.join("f g.txt"), lines(|n| (n == 5).then_some("five"))).unwrap();
        std::fs::write(folder.join("a.txt"), "above\n").unwrap();
        let listed = tree(&below).unwrap().unstaged;
        assert_eq!(paths(&listed), ["../a.txt", "f g.txt"]);
        let diff = file_diff(&below, &listed[1], false).unwrap();
        stage_hunk(&below, &diff, 0).unwrap();
        assert_eq!(paths(&tree(&below).unwrap().staged), ["f g.txt"]);
        // One above the folder is staged by the name the tab shows.
        stage(&below, &listed[0]).unwrap();
        assert_eq!(
            paths(&tree(&below).unwrap().staged),
            ["../a.txt", "f g.txt"]
        );
    }

    #[test]
    fn a_commit_takes_the_staged_set_only() {
        let folder = repo("commit");
        std::fs::write(folder.join("a.txt"), lines(|n| (n == 2).then_some("two"))).unwrap();
        std::fs::write(folder.join("new.txt"), "fresh\n").unwrap();
        std::fs::write(folder.join("later.txt"), "not yet\n").unwrap();
        let tree = tree(&folder).unwrap();
        for entry in tree
            .unstaged
            .iter()
            .filter(|e| e.path != Path::new("later.txt"))
        {
            stage(&folder, entry).unwrap();
        }
        let said = commit(&folder, "Keep the fresh file\n\nA body.").unwrap();
        assert!(said.contains("Keep the fresh file"), "{said}");
        let committed = setup(&folder, &["show", "--name-only", "--format=", "HEAD"]);
        assert_eq!(committed, "a.txt\nnew.txt\n");
        let left = super::tree(&folder).unwrap();
        assert!(left.staged.is_empty(), "{left:?}");
        assert_eq!(paths(&left.unstaged), ["later.txt"]);
        let why = commit(&folder, "Again").unwrap_err();
        assert!(why.contains("Nothing is staged"), "{why}");
        let why = commit(&folder, "  \n").unwrap_err();
        assert!(why.contains("message"), "{why}");
    }

    #[cfg(unix)]
    #[test]
    fn a_hook_that_leaves_a_process_running_does_not_hold_the_commit() {
        use std::os::unix::fs::PermissionsExt;
        let folder = repo("hook-left");
        let hook = folder.join(".git/hooks/post-commit");
        std::fs::write(&hook, "#!/bin/sh\nsleep 30 &\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(folder.join("a.txt"), "changed\n").unwrap();
        setup(&folder, &["add", "a.txt"]);
        let started = Instant::now();
        let said = commit(&folder, "Change the file").unwrap();
        assert!(said.contains("Change the file"), "{said}");
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_folder_outside_a_work_tree_is_refused() {
        let folder = scratch("outside");
        // A temp folder inside someone's repository is in one, and there
        // is nothing to check.
        if look(&folder, &["rev-parse", "--is-inside-work-tree"]).is_ok() {
            return;
        }
        let entry = Entry {
            path: "a.txt".into(),
            from: None,
            kind: Kind::Modified,
        };
        let refusals = [
            tree(&folder).map(drop),
            stage(&folder, &entry),
            unstage(&folder, &entry),
            commit(&folder, "x").map(drop),
            plan_push(&folder).map(drop),
        ];
        for refusal in refusals {
            let why = refusal.unwrap_err();
            assert!(why.contains("not in a git work tree"), "{why}");
        }
    }

    /// `folder` with a bare repository beside it as `origin`, its branch
    /// pushed there and following it.
    fn with_remote(folder: &Path) -> PathBuf {
        let bare = folder.with_extension("git");
        let _ = std::fs::remove_dir_all(&bare);
        std::fs::create_dir_all(&bare).unwrap();
        setup(&bare, &["init", "-q", "--bare", "-b", "main"]);
        setup(folder, &["remote", "add", "origin", bare.to_str().unwrap()]);
        setup(folder, &["push", "-q", "-u", "origin", "main"]);
        bare
    }

    #[test]
    fn the_push_goes_through_the_gate_as_a_command() {
        let folder = repo("gate");
        with_remote(&folder);
        let plan = plan_push(&folder).unwrap();
        assert_eq!(plan.command(), "git push origin main:refs/heads/main");
        let ctx = ToolContext {
            workspace: Some(folder.clone()),
            ..Default::default()
        };
        let grants = Grants::new();
        let gate = |mode, human| Gate {
            ctx: &ctx,
            mode,
            grants: &grants,
            rules: &[],
            broken: None,
            human,
        };
        assert_eq!(plan.decide(&gate(Mode::Default, true)), Outcome::Ask);
        assert_eq!(plan.decide(&gate(Mode::AcceptEdits, true)), Outcome::Ask);
        assert_eq!(plan.decide(&gate(Mode::Bypass, true)), Outcome::Run);
        for refused in [gate(Mode::Plan, true), gate(Mode::DontAsk, true)] {
            assert!(matches!(plan.decide(&refused), Outcome::Refuse(_)));
        }
        // With nobody to answer, what would ask is refused.
        assert!(matches!(
            plan.decide(&gate(Mode::Default, false)),
            Outcome::Refuse(_)
        ));
        let granted = Grants::from([(0, ["run_command".to_owned()].into())]);
        let allowed = Gate {
            grants: &granted,
            ..gate(Mode::Default, true)
        };
        assert_eq!(plan.decide(&allowed), Outcome::Run);
    }

    #[test]
    fn a_push_reaches_the_upstream_and_a_rejected_one_says_so() {
        let folder = repo("push");
        let bare = with_remote(&folder);
        std::fs::write(folder.join("a.txt"), "changed\n").unwrap();
        setup(&folder, &["commit", "-q", "-am", "second"]);
        let plan = plan_push(&folder).unwrap();
        assert_eq!(push(&folder, &plan).unwrap(), "Pushed main to origin.");
        let there = setup(&bare, &["log", "-1", "--format=%s", "main"]);
        assert_eq!(there, "second\n");
        assert!(push(&folder, &plan).unwrap().contains("already on origin"));

        // Someone else pushes first; ours is behind and is not forced.
        let other = scratch("push-other");
        setup(&other, &["clone", "-q", bare.to_str().unwrap(), "."]);
        identity(&other);
        std::fs::write(other.join("b.txt"), "theirs\n").unwrap();
        setup(&other, &["add", "b.txt"]);
        setup(&other, &["commit", "-q", "-m", "theirs"]);
        setup(&other, &["push", "-q", "origin", "main"]);
        std::fs::write(folder.join("a.txt"), "mine\n").unwrap();
        setup(&folder, &["commit", "-q", "-am", "mine"]);
        let why = push(&folder, &plan).unwrap_err();
        assert!(why.contains("The push was rejected"), "{why}");
        assert!(why.contains("Pull them in"), "{why}");
        let there = setup(&bare, &["log", "-1", "--format=%s", "main"]);
        assert_eq!(there, "theirs\n");
    }

    #[test]
    fn a_branch_with_no_upstream_has_nothing_to_push_to() {
        let folder = repo("no-upstream");
        let why = plan_push(&folder).unwrap_err();
        assert!(why.contains("main has no upstream"), "{why}");
    }

    #[test]
    fn a_suggested_message_is_put_in_the_house_style() {
        let long = "word ".repeat(40);
        let text = format!("```\nfeat(ui): Stage one hunk at a time.\n\n{long}\n\nSecond.\n```");
        let message = house_style(&text);
        let mut lines = message.lines();
        assert_eq!(lines.next(), Some("Stage one hunk at a time"));
        assert_eq!(lines.next(), Some(""));
        assert!(message.lines().all(|line| line.chars().count() <= WRAP));
        assert!(message.ends_with("\n\nSecond."), "{message}");
        // A colon in a real subject is not a prefix.
        assert_eq!(house_style("Say why: it failed"), "Say why: it failed");
    }

    #[test]
    fn a_suggestion_asks_as_the_model_takes_it() {
        let on = |endpoint: &str, model: &str| Connection {
            endpoint: endpoint.into(),
            model: model.into(),
            ..Default::default()
        };
        let plain = suggest_body(&on("https://api.openai.com/v1", "gpt-4o"), "d");
        assert!(plain.get("reasoning_effort").is_none(), "{plain}");
        assert_eq!(plain["max_tokens"], 1500);
        let reasoning = suggest_body(&on("https://api.openai.com/v1", "gpt-5"), "d");
        assert!(reasoning.get("max_tokens").is_none(), "{reasoning}");
        assert!(reasoning["max_completion_tokens"].is_u64(), "{reasoning}");
    }

    #[test]
    fn the_staged_diff_is_cut_at_a_size() {
        let folder = repo("staged-cap");
        let big: String = (0..20_000).map(|n| format!("line {n}\n")).collect();
        std::fs::write(folder.join("a.txt"), big).unwrap();
        setup(&folder, &["add", "a.txt"]);
        let diff = staged_diff(&folder).unwrap();
        assert!(diff.len() < SUGGEST_CAP + 100);
        assert!(diff.ends_with("the rest of the diff is left out.\n"));
    }
}
