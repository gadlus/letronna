//! What each conversation has seen of the files it touches, so `write_file`
//! and `edit_file` never overwrite a file the model has not read, or one
//! that changed after it looked.
//!
//! Entries are keyed by conversation and canonical path: helpers and other
//! sessions have ids of their own, so a file one of them wrote reads as
//! changed to everyone else. Each entry holds the SHA-256 of the bytes the
//! model last read or wrote, and a write is refused unless the file on
//! disk still hashes to it. The hash decides, not the modification time:
//! times are coarse on some filesystems, and a formatter can rewrite a
//! file within one tick. Times only spot which files a command touched,
//! for the note after it, and stand in for the hash past `HASH_BYTES`.
//!
//! A read counts only while the model can see it. The chat says, before
//! each request, where in the history the reply will go and where the
//! window starts, and every entry is stamped with the reply whose call
//! made it: a read from the reply being worked through was not seen when
//! the change was composed, and one from before the window start has been
//! dropped or summarised. Pages of a file add up while it stays unchanged:
//! once they have shown every line, and the request goes out with each of
//! them whole rather than cleared to save room, they count as one read of
//! all of it. A rewind forgets the conversation's entries, since the
//! positions they were stamped with are used again.
//!
//! The check and the write happen under one lock, on the same bytes, so
//! two calls in this process cannot both pass and then clobber each other.
//! Another process writing in between is not covered. The state lives in
//! memory: after a restart the first overwrite asks for a fresh read,
//! which costs a call.

use ring::digest;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::SystemTime;

/// Entries kept across all conversations, oldest dropped first. One is a
/// path, a few numbers and 32 bytes for each part of the file a read still
/// in the window showed; a file read whole or a few pages at a time comes
/// to a few hundred bytes, so the cap holds around a megabyte.
const KEEP: usize = 4096;

/// Files named in the note after a command; more are counted, not named.
const NAMED: usize = 10;

/// The conversation of no session in particular: tests, and callers outside
/// a chat. Unrelated callers would share its entries, so nothing is kept or
/// checked for it; every session and helper has an id of its own.
const NONE: u64 = 0;

/// Files longer than this are not hashed; their length and modification
/// time stand in. SHA-256 gets through this much in a fraction of a second,
/// where a log of many gigabytes would hold the session up, and read_file
/// counts lines no further either.
const HASH_BYTES: u64 = 64 * 1024 * 1024;

/// What tells one state of a file's bytes from another.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Print {
    Hash([u8; 32]),
    /// Past `HASH_BYTES`, when it was last modified.
    Modified(Option<SystemTime>),
}

/// Lines one read showed, every one of them as the file holds it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Lines {
    /// The first, counting from 1.
    pub(crate) first: usize,
    /// The last; one before `first` for an empty file.
    pub(crate) last: usize,
    /// Whether the last is the file's own last line.
    pub(crate) end: bool,
}

/// What a conversation last saw of a file.
#[derive(Clone)]
struct Seen {
    len: u64,
    print: Print,
    /// Whether the model was shown all of it, which an overwrite needs.
    whole: bool,
    /// The lines reads have shown of it as it is, each with the reply whose
    /// call read them; empty once it was written or changed.
    pages: Vec<(Lines, usize)>,
    /// Where in the history the reply whose call recorded it sits.
    reply: usize,
    /// Whether the model wrote it: what it sent is in that reply, so the
    /// same reply may change it again.
    own: bool,
    /// When it was recorded, so the oldest can go first.
    at: u64,
}

impl Seen {
    /// The reply the model's view of all of the file rests on in the request
    /// `asked`; None when it cannot see all of it.
    fn whole_since(&self, asked: Reply) -> Option<usize> {
        if !self.whole {
            return None;
        }
        // What was written carries the reply it rests on in its mark.
        if self.pages.is_empty() {
            return Some(self.reply);
        }
        covers(&self.pages, asked)
    }
}

/// The reply the request `asked` has all of the file since: that of one
/// read of all of it, or the earliest of pages that show every line together.
fn covers(pages: &[(Lines, usize)], asked: Reply) -> Option<usize> {
    // One read of all of it counts while the window holds it, cleared or
    // not, as it always has: the model once had all of it in one request.
    let whole = pages
        .iter()
        .find(|(lines, at)| lines.first <= 1 && lines.end && *at >= asked.window);
    if let Some((_, at)) = whole {
        return Some(*at);
    }
    // Pages count only while each goes out whole, or the model would write
    // over a file it never had all of in any one request.
    let from = asked.window.max(asked.cleared);
    let mut seen: Vec<&(Lines, usize)> = pages.iter().filter(|(_, at)| *at >= from).collect();
    seen.sort_by_key(|(lines, _)| lines.first);
    let earliest = seen.iter().map(|(_, at)| *at).min()?;
    let mut next = 1;
    for (lines, _) in seen {
        if lines.first > next {
            return None;
        }
        if lines.end {
            return Some(earliest);
        }
        next = next.max(lines.last + 1);
    }
    None
}

/// The reply a conversation is being worked through, where its window
/// starts and where the results it sends whole start, all as positions in
/// its history.
#[derive(Clone, Copy, Default)]
struct Reply {
    at: usize,
    window: usize,
    cleared: usize,
}

#[derive(Default)]
struct State {
    seen: HashMap<(u64, PathBuf), Seen>,
    replies: HashMap<u64, Reply>,
    clock: u64,
}

impl State {
    fn record(&mut self, conversation: u64, path: PathBuf, mut seen: Seen) {
        if conversation == NONE {
            return;
        }
        self.clock += 1;
        let key = (conversation, path);
        if self.seen.len() >= KEEP && !self.seen.contains_key(&key) {
            let oldest = self
                .seen
                .iter()
                .min_by_key(|(_, s)| s.at)
                .map(|(k, _)| k.clone());
            if let Some(oldest) = oldest {
                self.seen.remove(&oldest);
            }
        }
        seen.at = self.clock;
        self.seen.insert(key, seen);
    }

    fn reply(&self, conversation: u64) -> Reply {
        self.replies.get(&conversation).copied().unwrap_or_default()
    }
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    // A panic mid-write leaves nothing half-done in the map itself, and
    // giving up the guard for good would be worse than using it.
    STATE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

fn hash_of(bytes: &[u8]) -> [u8; 32] {
    let hash = digest::digest(&digest::SHA256, bytes);
    hash.as_ref().try_into().unwrap_or_default()
}

/// The print of `bytes`, which the file at `path` holds.
fn print_of(bytes: &[u8], path: &Path) -> Print {
    if bytes.len() as u64 > HASH_BYTES {
        return Print::Modified(stat(path).and_then(|(_, time)| time));
    }
    Print::Hash(hash_of(bytes))
}

/// The length and print of the file at `path` as it is now, hashed a block
/// at a time so a big one is not held in memory.
fn look(path: &Path) -> std::io::Result<(u64, Print)> {
    let meta = std::fs::metadata(path)?;
    // Opening a pipe would wait for a writer.
    if !meta.is_file() {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    if meta.len() > HASH_BYTES {
        return Ok((meta.len(), Print::Modified(meta.modified().ok())));
    }
    let mut file = std::fs::File::open(path)?.take(HASH_BYTES);
    let mut context = digest::Context::new(&digest::SHA256);
    let mut block = vec![0; 64 * 1024];
    let mut len = 0;
    loop {
        let n = match file.read(&mut block) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        context.update(&block[..n]);
        len += n as u64;
    }
    let hash = context.finish().as_ref().try_into().unwrap_or_default();
    Ok((len, Print::Hash(hash)))
}

/// Whether `seen` still describes the `bytes` the file at `path` holds.
fn matches(seen: &Seen, bytes: &[u8], path: &Path) -> bool {
    seen.len == bytes.len() as u64 && seen.print == print_of(bytes, path)
}

/// The map's key for `path`: canonical, so a read and a write of one file
/// meet however the model spelled it.
fn key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// A file's length and modification time; `None` when it is gone.
pub(crate) type Stat = Option<(u64, Option<SystemTime>)>;

pub(crate) fn stat(path: &Path) -> Stat {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()))
}

fn changed(raw: &str) -> String {
    format!(
        "`{raw}` changed on disk since you last read or wrote it -- the user, a \
         formatter or another session edited it. Nothing was changed. Read it \
         again with read_file, then redo this change on what is there now."
    )
}

/// Says that the model is being asked for the reply at position `at` of the
/// conversation's history, with the window starting at `window` and the
/// long results of replies before `cleared` sent as a line saying what they
/// were: the calls that come back are that reply's.
pub(crate) fn asked(conversation: u64, at: usize, window: usize, cleared: usize) {
    if conversation == NONE {
        return;
    }
    let reply = Reply {
        at,
        window,
        cleared,
    };
    state().replies.insert(conversation, reply);
}

/// Drops all the conversation has seen: after a rewind the positions its
/// entries were stamped with are used again, and a deleted session's id
/// can be given to a new one.
pub(crate) fn forget(conversation: u64) {
    let mut state = state();
    state.seen.retain(|(c, _), _| *c != conversation);
    state.replies.remove(&conversation);
}

/// Records that the model read `path`, and the `lines` it was shown if each
/// showed as the file holds it. `before` is the file's stat from before the
/// read: when it moved since, the model saw bytes that are no longer there,
/// and nothing is recorded.
pub(crate) fn read(conversation: u64, path: &Path, before: Stat, lines: Option<Lines>) {
    if conversation == NONE {
        return;
    }
    let Ok((len, print)) = look(path) else {
        return;
    };
    if before.is_none() || stat(path) != before {
        return;
    }
    let key = key(path);
    let mut state = state();
    let reply = state.reply(conversation);
    // Pages of the file as it still is add up; what they showed of bytes
    // since changed is gone. One that left the window, or that this read
    // showed again, adds nothing more.
    let mut pages = match state.seen.get(&(conversation, key.clone())) {
        Some(seen) if (seen.len, seen.print) == (len, print) => seen.pages.clone(),
        _ => Vec::new(),
    };
    pages.retain(|(page, at)| {
        *at >= reply.window
            && !lines.is_some_and(|new| new.first <= page.first && page.last <= new.last)
    });
    pages.extend(lines.map(|lines| (lines, reply.at)));
    let seen = Seen {
        len,
        print,
        whole: covers(&pages, reply).is_some(),
        pages,
        reply: reply.at,
        own: false,
        at: 0,
    };
    state.record(conversation, key, seen);
}

/// The file's bytes as text, or a refusal: a file in another encoding or a
/// binary one would lose what the tools cannot show, and asking for a read
/// first would only send the model round in a loop.
fn text(bytes: Vec<u8>, raw: &str) -> Result<String, String> {
    let refusal = || {
        format!(
            "`{raw}` is not UTF-8 text -- another encoding, or binary -- and \
             Letronna does not overwrite such files, since what its tools cannot \
             show would be lost. Nothing was changed."
        )
    };
    if bytes.contains(&0) {
        return Err(refusal());
    }
    String::from_utf8(bytes).map_err(|_| refusal())
}

/// What this conversation has seen of `path` that the reply being worked
/// through may build on. `then` ends the refusal's advice.
fn seen<'a>(
    state: &'a State,
    conversation: u64,
    path: &Path,
    raw: &str,
    then: &str,
) -> Result<&'a Seen, String> {
    let Some(seen) = state.seen.get(&(conversation, key(path))) else {
        return Err(format!(
            "You have not read `{raw}` in this conversation, so this would change \
             what you have not seen. Nothing was changed. Read it with read_file \
             first, then {then}."
        ));
    };
    let reply = state.reply(conversation);
    if seen.reply < reply.window {
        return Err(format!(
            "Your read of `{raw}` has left the part of this conversation you can \
             still see: it was dropped for room or summarised. Nothing was \
             changed. Read it again with read_file, then {then}."
        ));
    }
    if !seen.own && seen.reply >= reply.at {
        return Err(format!(
            "You read `{raw}` in this same reply, so this change was made before \
             you saw what the read returned. Nothing was changed. Look at it, \
             then {then} if it still applies."
        ));
    }
    Ok(seen)
}

/// A file a tool is about to change, held under the lock from the check to
/// the write, so nothing else in this process changes it in between.
pub(crate) struct Change {
    state: MutexGuard<'static, State>,
    conversation: u64,
    path: PathBuf,
    before: Option<String>,
    /// What is recorded once it is written, besides its length and print.
    whole: bool,
    reply: usize,
    own: bool,
}

impl Change {
    /// What the file holds now; `None` for a new file.
    pub(crate) fn before(&self) -> Option<&str> {
        self.before.as_deref()
    }

    /// Writes `bytes` over the file, whole or not at all, and records them
    /// as what the model has seen of it.
    pub(crate) fn write(mut self, bytes: &[u8], log: &crate::history::Log) -> Result<(), String> {
        crate::write::atomic(&self.path, bytes, log)?;
        let mark = Mark {
            whole: self.whole,
            reply: self.reply,
            own: self.own,
        };
        wrote(&mut self.state, self.conversation, &self.path, bytes, mark);
        Ok(())
    }
}

/// What a change builds on: whether the model saw all of the file, in which
/// reply, and whether it wrote it; carried over to what it writes.
#[derive(Clone, Copy)]
pub(crate) struct Mark {
    whole: bool,
    reply: usize,
    own: bool,
}

/// Records `bytes`, just written to `path`, as what the conversation has
/// seen of it.
fn wrote(state: &mut State, conversation: u64, path: &Path, bytes: &[u8], mark: Mark) {
    let seen = Seen {
        len: bytes.len() as u64,
        print: print_of(bytes, path),
        whole: mark.whole,
        pages: Vec::new(),
        reply: mark.reply,
        own: mark.own,
        at: 0,
    };
    state.record(conversation, key(path), seen);
}

/// Several files one apply_patch call changes, held under one lock from the
/// first check to the last write, so the patch is judged on the bytes it
/// then replaces.
pub(crate) struct Batch {
    state: MutexGuard<'static, State>,
    conversation: u64,
}

/// Opens a batch of changes for the conversation.
pub(crate) fn batch(conversation: u64) -> Batch {
    Batch {
        state: state(),
        conversation,
    }
}

impl Batch {
    /// What `path` holds, refused as edit_file refuses it.
    pub(crate) fn open(&self, path: &Path, raw: &str) -> Result<(String, Mark), String> {
        existing(&self.state, self.conversation, path, raw)
    }

    /// The mark of a file the call creates: seen whole, and the model's own.
    pub(crate) fn fresh(&self) -> Mark {
        Mark {
            whole: true,
            reply: self.state.reply(self.conversation).at,
            own: true,
        }
    }

    /// Writes `bytes` over `path`, whole or not at all, and records them.
    pub(crate) fn write(
        &mut self,
        path: &Path,
        bytes: &[u8],
        mark: Mark,
        log: &crate::history::Log,
    ) -> Result<(), String> {
        crate::write::atomic(path, bytes, log)?;
        wrote(&mut self.state, self.conversation, path, bytes, mark);
        Ok(())
    }

    /// Deletes `path`, kept in `log` for a rewind, and forgets what was seen
    /// of it.
    pub(crate) fn remove(
        &mut self,
        path: &Path,
        raw: &str,
        log: &crate::history::Log,
    ) -> Result<(), String> {
        let key = key(path);
        crate::write::remove(path, log).map_err(|e| format!("Could not delete `{raw}`: {e}"))?;
        self.state.seen.remove(&(self.conversation, key));
        Ok(())
    }
}

/// Opens `path` for write_file. Refused when the file is there and is not
/// text, is too large to check, or this conversation has not seen all of it,
/// unchanged, in an earlier reply it can still see.
pub(crate) fn overwrite(conversation: u64, path: &Path, raw: &str) -> Result<Change, String> {
    let state = state();
    let reply = state.reply(conversation).at;
    let meta = match std::fs::metadata(path) {
        Ok(meta) => Some(meta),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("Could not read `{raw}`: {e}")),
    };
    let mut change = Change {
        state,
        conversation,
        path: path.to_path_buf(),
        before: None,
        whole: true,
        reply,
        own: true,
    };
    // Only a regular file is read: opening a pipe would wait for a writer.
    let Some(meta) = meta.filter(std::fs::Metadata::is_file) else {
        return Ok(change);
    };
    if meta.len() > HASH_BYTES {
        return Err(format!(
            "`{raw}` is too large to overwrite: over {} MB, what it holds is not \
             checked. Nothing was changed. Use edit_file.",
            HASH_BYTES / (1024 * 1024)
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("Could not read `{raw}`: {e}"))?;
    let text = text(bytes, raw)?;
    // An empty file holds nothing to lose.
    if conversation != NONE && !text.is_empty() {
        let asked = change.state.reply(conversation);
        let seen = seen(&change.state, conversation, path, raw, "write it again")?;
        if seen.whole_since(asked).is_none() {
            return Err(format!(
                "You have seen only part of `{raw}`, or pages of it you read have \
                 since been dropped or cleared to save room, so writing the whole \
                 file would drop the rest. Nothing was changed. Read what you cannot \
                 see of it with read_file first, or change it with edit_file instead."
            ));
        }
        if !matches(seen, text.as_bytes(), path) {
            return Err(changed(raw));
        }
    }
    change.before = Some(text);
    Ok(change)
}

/// Opens `path` for edit_file. Refused when it is not text, or this
/// conversation has not seen it, unchanged, in an earlier reply it can
/// still see; what it had seen, whole or in part, carries over to the
/// result.
pub(crate) fn edit(conversation: u64, path: &Path, raw: &str) -> Result<Change, String> {
    let state = state();
    let (text, mark) = existing(&state, conversation, path, raw)?;
    Ok(Change {
        state,
        conversation,
        path: path.to_path_buf(),
        before: Some(text),
        whole: mark.whole,
        reply: mark.reply,
        own: mark.own,
    })
}

/// What `path` holds and what the conversation saw of it, for a change that
/// builds on it; refused as `edit` refuses it.
fn existing(
    state: &State,
    conversation: u64,
    path: &Path,
    raw: &str,
) -> Result<(String, Mark), String> {
    let meta = std::fs::metadata(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            format!("`{raw}` does not exist; to create it, use write_file.")
        }
        _ => e.to_string(),
    })?;
    // Opening a pipe would wait for a writer, holding up every other change.
    if !meta.is_file() {
        return Err(format!("`{raw}` is not a regular file."));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let text = text(bytes, raw)?;
    let mut mark = Mark {
        whole: false,
        reply: 0,
        own: false,
    };
    if conversation != NONE {
        let seen = seen(state, conversation, path, raw, "make the edit")?;
        if !matches(seen, text.as_bytes(), path) {
            return Err(changed(raw));
        }
        // The result rests on the latest read, as any edit does, so it
        // stays whole only when the view of all of it rests there too:
        // pages read over several replies have to be read again.
        let whole = seen.whole_since(state.reply(conversation));
        mark = Mark {
            whole: whole == Some(seen.reply),
            reply: seen.reply,
            own: seen.own,
        };
    }
    Ok((text, mark))
}

/// How the files this conversation has seen stand, taken before a command
/// runs. Only metadata is read, so this stays cheap however many there are.
pub(crate) fn before(conversation: u64) -> Vec<(PathBuf, Stat)> {
    let paths: Vec<PathBuf> = state()
        .seen
        .keys()
        .filter(|(c, _)| *c == conversation)
        .map(|(_, path)| path.clone())
        .collect();
    paths
        .into_iter()
        .map(|path| {
            let stat = stat(&path);
            (path, stat)
        })
        .collect()
}

/// A line naming the files among `before` that the command touched and
/// that no longer hold what the model saw, or "" when there are none. The
/// model ran the command, so each counts as seen in part as it is now:
/// edit_file still has to find its `old` there, but write_file needs a
/// fresh read. Names are relative to `workspace` where they can be.
pub(crate) fn note(conversation: u64, before: Vec<(PathBuf, Stat)>, workspace: &Path) -> String {
    let touched: Vec<PathBuf> = before
        .into_iter()
        .filter(|(path, was)| stat(path) != *was)
        .map(|(path, _)| path)
        .collect();
    if touched.is_empty() {
        return String::new();
    }
    let seen: Vec<(PathBuf, Seen)> = {
        let state = state();
        touched
            .into_iter()
            .filter_map(|path| {
                let seen = state.seen.get(&(conversation, path.clone()))?.clone();
                Some((path, seen))
            })
            .collect()
    };
    let root = key(workspace);
    let mut changed = Vec::new();
    for (path, seen) in seen {
        let now = look(&path).ok();
        if now == Some((seen.len, seen.print)) {
            continue;
        }
        if let Some((len, print)) = now {
            let part = Seen {
                len,
                print,
                whole: false,
                pages: Vec::new(),
                ..seen
            };
            state().record(conversation, path.clone(), part);
        }
        let name = path.strip_prefix(&root).unwrap_or(&path).display();
        changed.push(name.to_string());
    }
    if changed.is_empty() {
        return String::new();
    }
    let mut names = changed[..changed.len().min(NAMED)].join(", ");
    if changed.len() > NAMED {
        names.push_str(&format!(" and {} more", changed.len() - NAMED));
    }
    format!(
        "[This command changed files you had read or written: {names}. edit_file \
         works on them as they are now; read one in full again before replacing \
         it with write_file.]\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{ToolContext, run};

    /// A fresh project folder, and a session in it working on its first
    /// reply.
    fn project(name: &str, conversation: u64) -> (PathBuf, ToolContext) {
        let dir =
            std::env::temp_dir().join(format!("letronna-filestate-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        asked(conversation, 1, 0, 0);
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            conversation,
            ..Default::default()
        };
        (dir, ctx)
    }

    /// Moves the session on to the reply at `at`, as the chat does before
    /// each request, with the whole history in the window.
    fn reply(ctx: &ToolContext, at: usize) {
        asked(ctx.conversation, at, 0, 0);
    }

    fn read_to(dir: &Path, name: &str) -> String {
        std::fs::read_to_string(dir.join(name)).unwrap()
    }

    /// The offset the page `shown` says to continue from.
    fn next(shown: &str) -> usize {
        let (_, offset) = shown.rsplit_once("offset=").unwrap();
        offset.trim_end_matches('.').parse().unwrap()
    }

    /// A page of `name` from line `offset` on.
    fn page(ctx: &ToolContext, name: &str, offset: usize) -> String {
        let args = format!(r#"{{"path":"{name}","offset":{offset}}}"#);
        run("read_file", &args, ctx).unwrap()
    }

    /// 300 lines of about 40 bytes, each starting with `word`: two pages.
    fn lines(word: &str) -> String {
        (1..=300)
            .map(|n| format!("{word} {n:03} {}\n", "x".repeat(30)))
            .collect()
    }

    #[test]
    fn an_existing_file_is_overwritten_only_after_a_read_in_an_earlier_reply() {
        let (dir, ctx) = project("unread", 101);
        std::fs::write(dir.join("a.txt"), "mine").unwrap();
        let write = r#"{"path":"a.txt","content":"theirs"}"#;
        let refused = run("write_file", write, &ctx).unwrap_err();
        assert!(
            refused.contains("Read it with read_file first"),
            "{refused}"
        );
        let edit = r#"{"path":"a.txt","old":"mine","new":"theirs"}"#;
        let refused = run("edit_file", edit, &ctx).unwrap_err();
        assert!(
            refused.contains("Read it with read_file first"),
            "{refused}"
        );
        // Read and written in one reply: the write was made up before the
        // read came back.
        run("read_file", r#"{"path":"a.txt"}"#, &ctx).unwrap();
        let refused = run("write_file", write, &ctx).unwrap_err();
        assert!(refused.contains("in this same reply"), "{refused}");
        assert!(
            run("edit_file", edit, &ctx)
                .unwrap_err()
                .contains("in this same reply")
        );
        assert_eq!(read_to(&dir, "a.txt"), "mine");

        reply(&ctx, 3);
        run("write_file", write, &ctx).unwrap();
        assert_eq!(read_to(&dir, "a.txt"), "theirs");
        // An empty file holds nothing to lose, and a new one nothing at all.
        std::fs::write(dir.join("empty.txt"), "").unwrap();
        run("write_file", r#"{"path":"empty.txt","content":"x"}"#, &ctx).unwrap();
        run("write_file", r#"{"path":"new/b.txt","content":"x"}"#, &ctx).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn editing_a_missing_file_points_to_write_file() {
        let (dir, ctx) = project("missing", 112);
        std::fs::write(dir.join("tools.rs"), "").unwrap();
        let edit = r#"{"path":"notes.txt","old":"a","new":"b"}"#;
        let refused = run("edit_file", edit, &ctx).unwrap_err();
        assert_eq!(
            refused,
            "`notes.txt` does not exist; to create it, use write_file."
        );
        // A name like one there is may be a slip rather than a new file.
        let edit = r#"{"path":"tool.rs","old":"a","new":"b"}"#;
        let refused = run("edit_file", edit, &ctx).unwrap_err();
        assert!(refused.ends_with(" Did you mean `tools.rs`?"), "{refused}");
        assert!(!dir.join("notes.txt").exists() && !dir.join("tool.rs").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_read_counts_only_while_the_window_holds_it_and_not_after_a_rewind() {
        let (dir, ctx) = project("window", 102);
        std::fs::write(dir.join("a.txt"), "one").unwrap();
        run("read_file", r#"{"path":"a.txt"}"#, &ctx).unwrap();
        // The window now starts after the reply that read it.
        asked(102, 9, 4, 4);
        let edit = r#"{"path":"a.txt","old":"one","new":"two"}"#;
        let refused = run("edit_file", edit, &ctx).unwrap_err();
        assert!(refused.contains("has left the part"), "{refused}");
        let write = r#"{"path":"a.txt","content":"two"}"#;
        let refused = run("write_file", write, &ctx).unwrap_err();
        assert!(refused.contains("has left the part"), "{refused}");
        asked(102, 9, 1, 1);
        run("edit_file", edit, &ctx).unwrap();
        assert_eq!(read_to(&dir, "a.txt"), "two");

        // Taking the conversation back forgets it, reads and writes alike.
        let mut conversation = crate::store::Conversation {
            id: 102,
            messages: vec![Default::default(); 9],
            ..Default::default()
        };
        conversation.rewind(1);
        reply(&ctx, 3);
        let refused = run("write_file", write, &ctx).unwrap_err();
        assert!(refused.contains("have not read"), "{refused}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_file_changed_since_it_was_read_is_left_alone() {
        let (dir, ctx) = project("changed", 103);
        std::fs::write(dir.join("a.txt"), "one two").unwrap();
        run("read_file", r#"{"path":"a.txt"}"#, &ctx).unwrap();
        reply(&ctx, 3);
        // The same length, so only the hash can tell.
        std::fs::write(dir.join("a.txt"), "one TWO").unwrap();
        let write = r#"{"path":"a.txt","content":"three"}"#;
        let refused = run("write_file", write, &ctx).unwrap_err();
        assert!(refused.contains("changed on disk"), "{refused}");
        assert!(
            refused.contains("Read it again with read_file"),
            "{refused}"
        );
        let edit = r#"{"path":"a.txt","old":"one","new":"1"}"#;
        let refused = run("edit_file", edit, &ctx).unwrap_err();
        assert!(refused.contains("changed on disk"), "{refused}");
        assert_eq!(read_to(&dir, "a.txt"), "one TWO");

        run("read_file", r#"{"path":"a.txt"}"#, &ctx).unwrap();
        reply(&ctx, 5);
        run("edit_file", edit, &ctx).unwrap();
        assert_eq!(read_to(&dir, "a.txt"), "1 TWO");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn what_the_model_wrote_itself_counts_as_read_even_in_the_same_reply() {
        let (dir, ctx) = project("own", 104);
        run("write_file", r#"{"path":"a.txt","content":"one"}"#, &ctx).unwrap();
        run(
            "edit_file",
            r#"{"path":"a.txt","old":"one","new":"two"}"#,
            &ctx,
        )
        .unwrap();
        run("write_file", r#"{"path":"a.txt","content":"three"}"#, &ctx).unwrap();
        assert_eq!(read_to(&dir, "a.txt"), "three");
        // A formatter rewriting it on save is a change like any other.
        std::fs::write(dir.join("a.txt"), "three\n").unwrap();
        reply(&ctx, 3);
        let edit = r#"{"path":"a.txt","old":"three","new":"four"}"#;
        assert!(
            run("edit_file", edit, &ctx)
                .unwrap_err()
                .contains("changed on disk")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_file_kept_in_its_own_format_still_reads_as_what_was_written() {
        let (dir, ctx) = project("format", 105);
        std::fs::write(dir.join("a.txt"), "\u{FEFF}one\r\ntwo\r\n").unwrap();
        run("read_file", r#"{"path":"a.txt"}"#, &ctx).unwrap();
        reply(&ctx, 3);
        run("write_file", r#"{"path":"a.txt","content":"1\n2\n"}"#, &ctx).unwrap();
        assert_eq!(read_to(&dir, "a.txt"), "\u{FEFF}1\r\n2\r\n");
        reply(&ctx, 5);
        run("edit_file", r#"{"path":"a.txt","old":"2","new":"3"}"#, &ctx).unwrap();
        run("write_file", r#"{"path":"a.txt","content":"4\n"}"#, &ctx).unwrap();
        assert_eq!(read_to(&dir, "a.txt"), "\u{FEFF}4\r\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn each_session_needs_its_own_read_and_sees_the_others_writes() {
        let (dir, first) = project("sessions", 106);
        asked(107, 1, 0, 0);
        let second = ToolContext {
            conversation: 107,
            ..first.clone()
        };
        std::fs::write(dir.join("a.txt"), "one").unwrap();
        run("read_file", r#"{"path":"a.txt"}"#, &first).unwrap();
        reply(&first, 3);
        let write = r#"{"path":"a.txt","content":"two"}"#;
        let refused = run("write_file", write, &second).unwrap_err();
        assert!(refused.contains("have not read"), "{refused}");

        run("read_file", r#"{"path":"a.txt"}"#, &second).unwrap();
        reply(&second, 3);
        run("write_file", write, &second).unwrap();
        let edit = r#"{"path":"a.txt","old":"one","new":"1"}"#;
        let refused = run("edit_file", edit, &first).unwrap_err();
        assert!(refused.contains("another session"), "{refused}");
        assert_eq!(read_to(&dir, "a.txt"), "two");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn only_reads_that_showed_every_line_of_a_file_let_it_be_overwritten() {
        let (dir, ctx) = project("part", 108);
        // Two pages with lines between them that neither showed.
        std::fs::write(dir.join("long.txt"), lines("line")).unwrap();
        let first = run("read_file", r#"{"path":"long.txt"}"#, &ctx).unwrap();
        assert!(next(&first) < 290, "{first}");
        let second = page(&ctx, "long.txt", 290);
        assert!(second.contains("\n300|line 300 "), "{second}");
        // A line too long to show, and a limit short of the end.
        std::fs::write(dir.join("wide.txt"), "x".repeat(1_500)).unwrap();
        run("read_file", r#"{"path":"wide.txt"}"#, &ctx).unwrap();
        std::fs::write(dir.join("short.txt"), "a\nb\nc\n").unwrap();
        run("read_file", r#"{"path":"short.txt","limit":2}"#, &ctx).unwrap();
        reply(&ctx, 3);
        for name in ["long.txt", "wide.txt", "short.txt"] {
            let write = format!(r#"{{"path":"{name}","content":"short"}}"#);
            let refused = run("write_file", &write, &ctx).unwrap_err();
            assert!(refused.contains("edit_file instead"), "{name}: {refused}");
        }
        let edit = r#"{"path":"long.txt","old":"line 300","new":"end"}"#;
        run("edit_file", edit, &ctx).unwrap();
        assert!(read_to(&dir, "long.txt").contains("\nend x"));

        // Just under a page comes in one result.
        run("read_file", r#"{"path":"short.txt"}"#, &ctx).unwrap();
        let lines = format!("{}\n", "y".repeat(60)).repeat(100);
        std::fs::write(dir.join("page.txt"), lines).unwrap();
        run("read_file", r#"{"path":"page.txt"}"#, &ctx).unwrap();
        reply(&ctx, 5);
        run(
            "write_file",
            r#"{"path":"short.txt","content":"d\n"}"#,
            &ctx,
        )
        .unwrap();
        run("write_file", r#"{"path":"page.txt","content":"z\n"}"#, &ctx).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reading_every_page_lets_the_file_be_overwritten() {
        let (dir, ctx) = project("pages", 115);
        std::fs::write(dir.join("long.txt"), lines("line")).unwrap();
        let write = r#"{"path":"long.txt","content":"short\n"}"#;
        let second = next(&page(&ctx, "long.txt", 1));
        reply(&ctx, 3);
        let refused = run("write_file", write, &ctx).unwrap_err();
        assert!(refused.contains("only part"), "{refused}");
        let last = page(&ctx, "long.txt", second);
        assert!(last.ends_with(&format!("300|line 300 {}", "x".repeat(30))));
        // The last page came back in the reply that would write.
        let refused = run("write_file", write, &ctx).unwrap_err();
        assert!(refused.contains("in this same reply"), "{refused}");
        // A first page the window no longer holds leaves a gap, and so does
        // one the request clears to save room.
        for (window, cleared) in [(2, 2), (0, 2)] {
            asked(115, 5, window, cleared);
            let refused = run("write_file", write, &ctx).unwrap_err();
            assert!(refused.contains("only part"), "{refused}");
            assert!(refused.contains("Read what you cannot see"), "{refused}");
        }

        // An edit rests on the last page, so a second one goes through with
        // the first page gone; but the file is no longer seen whole.
        asked(115, 5, 1, 1);
        let edit = r#"{"path":"long.txt","old":"line 300","new":"end 300"}"#;
        run("edit_file", edit, &ctx).unwrap();
        asked(115, 7, 2, 2);
        let edit = r#"{"path":"long.txt","old":"line 299","new":"end 299"}"#;
        run("edit_file", edit, &ctx).unwrap();
        let refused = run("write_file", write, &ctx).unwrap_err();
        assert!(refused.contains("only part"), "{refused}");
        // Every page read in one reply keeps it whole through an edit.
        let second = next(&page(&ctx, "long.txt", 1));
        page(&ctx, "long.txt", second);
        asked(115, 9, 2, 2);
        let edit = r#"{"path":"long.txt","old":"line 298","new":"end 298"}"#;
        run("edit_file", edit, &ctx).unwrap();
        asked(115, 11, 2, 2);
        run("write_file", write, &ctx).unwrap();
        assert_eq!(read_to(&dir, "long.txt"), "short\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn one_read_of_all_of_a_file_counts_though_it_is_cleared_to_save_room() {
        let (dir, ctx) = project("cleared", 114);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        run("read_file", r#"{"path":"a.txt"}"#, &ctx).unwrap();
        asked(114, 5, 0, 3);
        run("write_file", r#"{"path":"a.txt","content":"two\n"}"#, &ctx).unwrap();
        assert_eq!(read_to(&dir, "a.txt"), "two\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_change_between_pages_resets_what_was_read() {
        let (dir, ctx) = project("repage", 113);
        // The same length, so only the hash tells the old bytes from the new.
        std::fs::write(dir.join("a.txt"), lines("line")).unwrap();
        std::fs::write(dir.join("b.txt"), lines("line")).unwrap();
        std::fs::write(dir.join("c.txt"), lines("LINE")).unwrap();
        let second = next(&page(&ctx, "a.txt", 1));
        page(&ctx, "b.txt", 1);
        reply(&ctx, 3);
        // Changed outside, and by a command, between the first page and the
        // second: the first showed bytes no longer there.
        std::fs::write(dir.join("a.txt"), lines("LINE")).unwrap();
        run("run_command", r#"{"command":"cp c.txt b.txt"}"#, &ctx).unwrap();
        page(&ctx, "a.txt", second);
        page(&ctx, "b.txt", second);
        reply(&ctx, 5);
        for name in ["a.txt", "b.txt"] {
            let write = format!(r#"{{"path":"{name}","content":"short"}}"#);
            let refused = run("write_file", &write, &ctx).unwrap_err();
            assert!(refused.contains("only part"), "{name}: {refused}");
        }

        // The first page again, of the file as it is now, completes it.
        page(&ctx, "a.txt", 1);
        reply(&ctx, 7);
        run("write_file", r#"{"path":"a.txt","content":"short"}"#, &ctx).unwrap();
        assert_eq!(read_to(&dir, "a.txt"), "short");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_command_that_changes_a_file_read_before_says_which_and_leaves_it_editable() {
        let (dir, ctx) = project("command", 109);
        std::fs::write(dir.join("a.txt"), "one").unwrap();
        // Another length: Copy-Item keeps the time b.txt was written, which
        // Windows may not tell from a.txt's.
        std::fs::write(dir.join("b.txt"), "three").unwrap();
        run("read_file", r#"{"path":"a.txt"}"#, &ctx).unwrap();
        run("read_file", r#"{"path":"b.txt"}"#, &ctx).unwrap();
        reply(&ctx, 3);
        // cp copies the bytes in sh and PowerShell alike; a redirect in
        // PowerShell 5 would write UTF-16.
        let out = run("run_command", r#"{"command":"cp b.txt a.txt"}"#, &ctx).unwrap();
        assert!(
            out.starts_with("[This command changed files you had read or written: a.txt."),
            "{out}"
        );
        assert!(!out.contains("b.txt"), "{out}");
        // Named once: the next command did not touch it.
        let out = run("run_command", r#"{"command":"echo hi"}"#, &ctx).unwrap();
        assert!(out.trim_start().starts_with("hi"), "{out}");
        // Seen in part as it is now: an edit has to find its text there, and
        // a whole new file needs a read first.
        let write = r#"{"path":"a.txt","content":"1"}"#;
        let refused = run("write_file", write, &ctx).unwrap_err();
        assert!(refused.contains("only part"), "{refused}");
        let edit = r#"{"path":"a.txt","old":"three","new":"2"}"#;
        run("edit_file", edit, &ctx).unwrap();
        assert_eq!(read_to(&dir, "a.txt"), "2");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_file_that_is_not_utf8_text_is_never_overwritten() {
        let (dir, ctx) = project("binary", 110);
        let files: [(&str, &[u8]); 3] = [
            ("latin.ini", b"caf\xE9=1\n"),
            ("wide.txt", b"\xFF\xFEa\0\n\0"),
            ("data.bin", b"a\0b"),
        ];
        for (name, bytes) in files {
            std::fs::write(dir.join(name), bytes).unwrap();
            // read_file shows the first two and refuses the last, which is
            // then refused the same way: no read would ever let it through.
            let _ = run("read_file", &format!(r#"{{"path":"{name}"}}"#), &ctx);
        }
        reply(&ctx, 3);
        for (name, bytes) in files {
            let write = format!(r#"{{"path":"{name}","content":"x"}}"#);
            let refused = run("write_file", &write, &ctx).unwrap_err();
            assert!(refused.contains("does not overwrite"), "{name}: {refused}");
            let edit = format!(r#"{{"path":"{name}","old":"a","new":"b"}}"#);
            let refused = run("edit_file", &edit, &ctx).unwrap_err();
            assert!(refused.contains("not UTF-8 text"), "{name}: {refused}");
            assert_eq!(std::fs::read(dir.join(name)).unwrap(), bytes);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_file_past_the_hash_cap_is_told_by_length_and_time_and_not_overwritten() {
        let (dir, ctx) = project("big", 111);
        let path = dir.join("big.log");
        // Text first, then zeros that a sparse file keeps off the disk.
        std::fs::write(&path, "entry\n".repeat(4_000)).unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(HASH_BYTES + 1).unwrap();
        run("read_file", r#"{"path":"big.log"}"#, &ctx).unwrap();
        reply(&ctx, 3);
        let write = r#"{"path":"big.log","content":"x"}"#;
        let refused = run("write_file", write, &ctx).unwrap_err();
        assert!(refused.contains("too large to overwrite"), "{refused}");

        let (len, print) = look(&path).unwrap();
        assert!(matches!(print, Print::Modified(Some(_))), "{print:?}");
        let seen = state().seen.get(&(111, key(&path))).cloned().unwrap();
        assert_eq!((seen.len, seen.print), (len, print));
        let then = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1 << 30);
        file.set_modified(then).unwrap();
        assert_ne!(look(&path).unwrap(), (len, print));
        drop(file);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn no_session_in_particular_is_neither_kept_nor_checked() {
        let (dir, ctx) = project("none", NONE);
        std::fs::write(dir.join("a.txt"), "one").unwrap();
        run("write_file", r#"{"path":"a.txt","content":"two"}"#, &ctx).unwrap();
        run(
            "edit_file",
            r#"{"path":"a.txt","old":"two","new":"3"}"#,
            &ctx,
        )
        .unwrap();
        assert!(before(NONE).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_oldest_entry_goes_when_the_map_is_full() {
        let mut state = State::default();
        let seen = Seen {
            len: 0,
            print: Print::Hash([0; 32]),
            whole: true,
            pages: Vec::new(),
            reply: 0,
            own: false,
            at: 0,
        };
        for n in 0..=KEEP {
            state.record(1, PathBuf::from(format!("f{n}")), seen.clone());
        }
        assert_eq!(state.seen.len(), KEEP);
        assert!(!state.seen.contains_key(&(1, PathBuf::from("f0"))));
        assert!(
            state
                .seen
                .contains_key(&(1, PathBuf::from(format!("f{KEEP}"))))
        );
    }
}
