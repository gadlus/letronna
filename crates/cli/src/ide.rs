//! The link to an editor: VS Code with Letronna's extension
//! (`editors/vscode`).
//!
//! The extension listens on this computer's loopback address and leaves a
//! note, `<data folder>/ide/<port>.json`, holding the token that lets a
//! terminal in. A terminal in one of that window's terminals is given the
//! port in `LETRONNA_IDE_PORT` and links as it starts; any other links with
//! `/ide`, to the window whose folder holds its own. Linked, the editor puts
//! references to what is selected there into the box and says which file
//! and lines are in view, and a change Letronna asks to make opens there as
//! a diff beside the question, closed once it is answered.
//!
//! Messages are JSON, one per line, as the extension's README sets out. The
//! link is dialled at 127.0.0.1 alone, whatever a note says.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use letronna_core::tools::ToolContext;
use serde_json::{Value, json};

/// How long dialling the editor and its welcome may take: it is on this
/// computer, so a wait any longer means it is not answering.
const WAIT: Duration = Duration::from_secs(2);

/// Past this many bytes on both sides a change is not sent for a diff; the
/// terminal still shows its own.
const DIFF_BYTES: usize = 4_000_000;

/// The variable the extension sets in its window's terminals: the port that
/// window listens on.
const PORT_VAR: &str = "LETRONNA_IDE_PORT";

/// What `/ide` says when no window can be linked.
const NONE: &str = "No VS Code window with Letronna's extension has this folder open. \
                    Build and install it from editors/vscode (its README says how), \
                    open this folder there, and run /ide again.";

/// An editor window's note: what it is, the folders it has open and its token.
struct Lock {
    pid: u32,
    ide: String,
    folders: Vec<PathBuf>,
    token: String,
}

/// A link that let this terminal in.
struct Link {
    ide: String,
    out: Mutex<TcpStream>,
}

/// Lines from the first to the last, counted from 1.
type Lines = Option<(u64, u64)>;

/// What the editor has said, and the link it said it on.
struct State {
    link: Option<Arc<Link>>,
    /// References the editor sent for the box, not yet put there.
    added: Vec<(PathBuf, Lines)>,
    /// The file in view there and the lines selected in it.
    seen: Option<(PathBuf, Lines)>,
    /// Whether any of it changed since the box last drew it.
    changed: bool,
}

/// Where a state is kept: this terminal's, or a test's own, so a box another
/// test draws meanwhile sees no editor.
type Slot = &'static Mutex<State>;

static STATE: Mutex<State> = State::empty();

impl State {
    const fn empty() -> Mutex<State> {
        Mutex::new(State {
            link: None,
            added: Vec::new(),
            seen: None,
            changed: false,
        })
    }
}

fn held(slot: Slot) -> MutexGuard<'static, State> {
    // Nothing that can panic runs under this lock, so a poisoned one is whole.
    slot.lock().unwrap_or_else(|e| e.into_inner())
}

/// Link as the terminal starts, when it runs inside VS Code: quietly when
/// no window has the extension, and saying so when one is linked.
pub fn auto(folder: &Path) -> Option<String> {
    let port = std::env::var(PORT_VAR).ok().and_then(|p| p.parse().ok());
    let in_vscode = std::env::var("TERM_PROGRAM").is_ok_and(|t| t == "vscode");
    if port.is_none() && !in_vscode {
        return None;
    }
    let ide = link_in(&STATE, &notes(), folder, port).ok()?;
    Some(format!("Linked to {ide} · /ide says what that does"))
}

/// `/ide`: link, unlink with `off`, or say what the link does.
pub fn command(rest: &str, folder: &Path) -> Result<String, String> {
    if rest.trim() == "off" {
        return Ok(match unlink(&STATE) {
            Some(ide) => format!("Unlinked from {ide}."),
            None => "No editor is linked.".into(),
        });
    }
    let linked = held(&STATE).link.as_ref().map(|link| link.ide.clone());
    let ide = match linked {
        Some(ide) => ide,
        None => {
            let port = std::env::var(PORT_VAR).ok().and_then(|p| p.parse().ok());
            link_in(&STATE, &notes(), folder, port)?
        }
    };
    let key = if cfg!(target_os = "macos") {
        "cmd+alt+k"
    } else {
        "ctrl+alt+k"
    };
    Ok(format!(
        "Linked to {ide}. There, {key} puts the selected lines in this box, the box's \
         edge shows the file in view, and a change Letronna asks to make opens as a diff \
         beside the question. /ide off unlinks."
    ))
}

/// Where the editors leave their notes: in the data folder every profile
/// shares, since the extension knows of no profiles.
fn notes() -> PathBuf {
    letronna_core::chat::base_dir().join("ide")
}

/// Link to the window `find_in` picks among the notes in `dir`, and give
/// the editor's name.
fn link_in(slot: Slot, dir: &Path, folder: &Path, port: Option<u16>) -> Result<String, String> {
    let (port, lock) = find_in(dir, folder, port).ok_or(NONE)?;
    let (link, reader) = dial(port, &lock, folder)?;
    let ide = link.ide.clone();
    let old = {
        let mut state = held(slot);
        state.seen = None;
        state.changed = true;
        state.link.replace(link.clone())
    };
    if let Some(old) = old {
        close(&old);
    }
    std::thread::spawn(move || listen(slot, link, reader));
    Ok(ide)
}

/// Take the link down, giving the editor's name when there was one.
fn unlink(slot: Slot) -> Option<String> {
    let link = {
        let mut state = held(slot);
        state.seen = None;
        state.changed = true;
        state.link.take()
    }?;
    close(&link);
    Some(link.ide.clone())
}

fn close(link: &Link) {
    if let Ok(stream) = link.out.lock() {
        let _ = stream.shutdown(Shutdown::Both);
    }
}

/// The note in `dir` to link to, with its port: the window on `port` when
/// the terminal was given one, or else the one whose folder holds `folder`
/// most closely, of those whose editor still runs.
fn find_in(dir: &Path, folder: &Path, port: Option<u16>) -> Option<(u16, Lock)> {
    let folder = std::fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf());
    let mut best: Option<(usize, u16, Lock)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Some(at) = path
            .file_stem()
            .and_then(|stem| stem.to_str()?.parse().ok())
        else {
            continue;
        };
        let Some(lock) = read_lock(&path).filter(|lock| running(lock.pid)) else {
            continue;
        };
        if port == Some(at) {
            return Some((at, lock));
        }
        let depth = lock
            .folders
            .iter()
            .map(|root| std::fs::canonicalize(root).unwrap_or_else(|_| root.clone()))
            .filter(|root| folder.starts_with(root))
            .map(|root| root.components().count())
            .max();
        if let Some(depth) = depth
            && best.as_ref().is_none_or(|(deepest, ..)| depth > *deepest)
        {
            best = Some((depth, at, lock));
        }
    }
    best.map(|(_, at, lock)| (at, lock))
}

fn read_lock(path: &Path) -> Option<Lock> {
    let note: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    Some(Lock {
        pid: u32::try_from(note["pid"].as_u64()?).ok()?,
        ide: note["ide"].as_str().unwrap_or("the editor").to_owned(),
        folders: note["folders"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|folder| Some(PathBuf::from(folder.as_str()?)))
            .collect(),
        token: note["token"].as_str()?.to_owned(),
    })
}

/// Whether the process `pid` still runs, so a note a crashed editor left is
/// passed over.
#[cfg(unix)]
fn running(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 sends nothing; it only asks whether the process is there.
    let there = unsafe { libc::kill(pid, 0) } == 0;
    // One that is there but not ours to signal still runs.
    there || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn running(_pid: u32) -> bool {
    true
}

/// The shell this terminal runs in, which the editor finds its terminal by.
#[cfg(unix)]
fn shell() -> Option<u32> {
    // SAFETY: getppid has no arguments and cannot fail.
    u32::try_from(unsafe { libc::getppid() }).ok()
}

#[cfg(not(unix))]
fn shell() -> Option<u32> {
    None
}

/// Where the editor on `port` is dialled: this computer, never another.
fn address(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

/// Dial the editor on `port`, give it the token and wait for its welcome,
/// giving the link and what reads what follows it.
fn dial(
    port: u16,
    lock: &Lock,
    folder: &Path,
) -> Result<(Arc<Link>, BufReader<TcpStream>), String> {
    let ide = &lock.ide;
    let failed = |e: std::io::Error| format!("{ide} did not answer ({e}).");
    let stream = TcpStream::connect_timeout(&address(port), WAIT).map_err(failed)?;
    stream.set_read_timeout(Some(WAIT)).map_err(failed)?;
    stream.set_write_timeout(Some(WAIT)).map_err(failed)?;
    let hello = json!({
        "type": "hello",
        "token": lock.token,
        "pid": std::process::id(),
        "shell": shell(),
        "folder": folder,
    });
    (&stream)
        .write_all(format!("{hello}\n").as_bytes())
        .map_err(failed)?;
    // One reader from here on: what the editor sends straight after its
    // welcome may already be in its buffer.
    let mut reader = BufReader::new(stream.try_clone().map_err(failed)?);
    let mut welcome = String::new();
    reader.read_line(&mut welcome).map_err(failed)?;
    let welcome: Value = serde_json::from_str(&welcome).unwrap_or_default();
    if welcome["type"] != "welcome" {
        return Err(format!("{ide} did not let this terminal in."));
    }
    stream.set_read_timeout(None).map_err(failed)?;
    let ide = welcome["ide"].as_str().unwrap_or(ide).to_owned();
    let link = Arc::new(Link {
        ide,
        out: Mutex::new(stream),
    });
    Ok((link, reader))
}

/// Take in what the editor says on `link` until it goes, then forget it.
fn listen(slot: Slot, link: Arc<Link>, reader: BufReader<TcpStream>) {
    let mine = |state: &State| state.link.as_ref().is_some_and(|l| Arc::ptr_eq(l, &link));
    for line in reader.lines() {
        let Ok(line) = line else {
            break;
        };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let mut state = held(slot);
        if !mine(&state) {
            return;
        }
        let path = message["path"].as_str().map(PathBuf::from);
        match message["type"].as_str() {
            Some("add") => {
                if let Some(path) = path {
                    state.added.push((path, lines(&message)));
                    state.changed = true;
                }
            }
            Some("selection") => {
                state.seen = path.map(|path| (path, lines(&message)));
                state.changed = true;
            }
            _ => {}
        }
    }
    let mut state = held(slot);
    if mine(&state) {
        state.link = None;
        state.seen = None;
        state.changed = true;
    }
}

fn lines(message: &Value) -> Lines {
    let start = message["start"].as_u64().filter(|&start| start > 0)?;
    let end = message["end"].as_u64().unwrap_or(start).max(start);
    Some((start, end))
}

/// The references the editor sent since last asked, written as the box
/// takes them, from `folder`, each with a space after it.
pub fn take_added(folder: Option<&Path>) -> Vec<String> {
    added_in(&STATE, folder)
}

fn added_in(slot: Slot, folder: Option<&Path>) -> Vec<String> {
    let added = std::mem::take(&mut held(slot).added);
    added
        .into_iter()
        .map(|(path, lines)| {
            let mut named = letronna_core::references::reference(&path, folder);
            match lines {
                Some((first, last)) if first == last => named.push_str(&format!("#L{first}")),
                Some((first, last)) => named.push_str(&format!("#L{first}-{last}")),
                None => {}
            }
            named + " "
        })
        .collect()
}

/// Whether the editor sent anything for the box to draw since last asked.
pub fn take_changed() -> bool {
    std::mem::take(&mut held(&STATE).changed)
}

/// `place` with the file in view in the linked editor after it, and its
/// lines, as the box's edge shows them.
pub fn with_seen(place: &str) -> String {
    seen_in(&STATE, place)
}

fn seen_in(slot: Slot, place: &str) -> String {
    let state = held(slot);
    let Some(link) = &state.link else {
        return place.to_owned();
    };
    let seen = match &state.seen {
        Some((path, lines)) => {
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            match lines {
                Some((first, last)) if first == last => format!("{name}:{first}"),
                Some((first, last)) => format!("{name}:{first}-{last}"),
                None => name,
            }
        }
        None => link.ide.clone(),
    };
    if place.is_empty() {
        format!("⧉ {seen}")
    } else {
        format!("{place} · ⧉ {seen}")
    }
}

/// A change shown in the editor, to close once its question is answered.
pub struct Shown {
    id: String,
    path: PathBuf,
    /// The first line it changes, where the file opens if it goes ahead.
    line: u64,
}

/// Show the change the call `id` asks to make as a diff in the linked
/// editor, when there is one and the call writes or edits a file.
pub fn show_change(name: &str, id: &str, args: &str, ctx: &ToolContext) -> Option<Shown> {
    let link = held(&STATE).link.clone()?;
    let change = letronna_core::tools::proposed(name, args, ctx)?;
    if change.before.len() + change.after.len() > DIFF_BYTES {
        return None;
    }
    let shown = Shown {
        id: plain(id),
        line: first_change(&change.before, &change.after),
        path: change.path,
    };
    let diff = json!({
        "type": "diff",
        "id": shown.id,
        "path": shown.path,
        "before": change.before,
        "after": change.after,
    });
    send(&STATE, &link, &diff).then_some(shown)
}

/// Close the diff `shown` once its question is answered, and with a yes
/// open the file where it changes.
pub fn answered(shown: Option<Shown>, yes: bool) {
    let (Some(shown), Some(link)) = (shown, held(&STATE).link.clone()) else {
        return;
    };
    send(
        &STATE,
        &link,
        &json!({"type": "close_diff", "id": shown.id}),
    );
    if yes {
        let open = json!({"type": "open", "path": shown.path, "line": shown.line, "preview": true});
        send(&STATE, &link, &open);
    }
}

/// Send `message` on `link`; one it cannot take is let go.
fn send(slot: Slot, link: &Arc<Link>, message: &Value) -> bool {
    let sent = link
        .out
        .lock()
        .is_ok_and(|mut out| out.write_all(format!("{message}\n").as_bytes()).is_ok());
    if !sent {
        let mut state = held(slot);
        if state.link.as_ref().is_some_and(|l| Arc::ptr_eq(l, link)) {
            state.link = None;
            state.seen = None;
            state.changed = true;
        }
    }
    sent
}

/// A call's id as the editor takes one for a diff: letters, digits, `_` and `-`.
fn plain(id: &str) -> String {
    let id: String = id
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' => c,
            _ => '_',
        })
        .take(128)
        .collect();
    if id.is_empty() { "change".into() } else { id }
}

/// The first line, counted from 1, where `after` differs from `before`.
fn first_change(before: &str, after: &str) -> u64 {
    let same = before
        .lines()
        .zip(after.lines())
        .take_while(|(old, new)| old == new)
        .count();
    u64::try_from(same).unwrap_or(0) + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// The tests' own state, which they take turns at.
    static TESTS: Mutex<State> = State::empty();
    static TURN: Mutex<()> = Mutex::new(());

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("letronna-ide-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn note(dir: &Path, port: u16, pid: u32, folders: &[&Path], token: &str) {
        let note = json!({"version": 1, "pid": pid, "ide": "Test Code", "folders": folders, "token": token});
        std::fs::write(dir.join(format!("{port}.json")), note.to_string()).unwrap();
    }

    #[test]
    fn the_link_only_ever_dials_this_computer() {
        assert!(address(4242).ip().is_loopback());
        assert_eq!(address(4242).port(), 4242);
    }

    #[test]
    fn the_window_holding_the_folder_most_closely_is_picked_and_a_gone_one_passed_over() {
        let dir = scratch("find");
        let project = dir.join("work/project");
        std::fs::create_dir_all(&project).unwrap();
        let me = std::process::id();
        note(&dir, 1001, me, &[&dir.join("work")], "outer");
        note(&dir, 1002, me, &[&project], "inner");
        // A pid no process has: the editor that left it is gone.
        note(&dir, 1003, i32::MAX as u32, &[&project], "gone");
        note(&dir, 1004, me, &[&dir.join("elsewhere")], "other");
        let picked = |port| find_in(&dir, &project, port).map(|(at, lock)| (at, lock.token));
        assert_eq!(picked(None), Some((1002, "inner".to_owned())));
        assert_eq!(picked(Some(1004)), Some((1004, "other".to_owned())));
        assert_eq!(picked(Some(1003)), Some((1002, "inner".to_owned())));
        assert!(find_in(&dir, &dir.join("elsewhere-not"), None).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_linked_editor_is_given_the_token_first_and_its_references_go_in_as_the_box_writes_them() {
        let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("link");
        let project = dir.join("project");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(project.join("src/a b.rs"), "fn a() {}\n").unwrap();
        let editor = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = editor.local_addr().unwrap().port();
        note(&dir, port, std::process::id(), &[&project], "the-token");
        let file = project.join("src/a b.rs");
        let sent = file.clone();
        let server = std::thread::spawn(move || {
            let (stream, _) = editor.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut hello = String::new();
            reader.read_line(&mut hello).unwrap();
            let mut out = &stream;
            out.write_all(b"{\"type\":\"welcome\",\"ide\":\"Test Code\",\"folders\":[]}\n")
                .unwrap();
            let add = json!({"type": "add", "path": sent, "start": 3, "end": 4});
            let seen = json!({"type": "selection", "path": sent, "start": 3, "end": 3});
            out.write_all(format!("{add}\n{seen}\n").as_bytes())
                .unwrap();
            let mut asked = String::new();
            reader.read_line(&mut asked).unwrap();
            // Kept open until the test unlinks, as a running editor would be.
            (hello, asked, stream)
        });
        assert_eq!(
            link_in(&TESTS, &dir, &project, None),
            Ok("Test Code".to_owned())
        );
        let waited = std::time::Instant::now();
        while held(&TESTS).seen.is_none() && waited.elapsed() < WAIT {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(added_in(&TESTS, Some(&project)), ["@src/a\\ b.rs#L3-4 "]);
        assert!(std::mem::take(&mut held(&TESTS).changed));
        assert_eq!(seen_in(&TESTS, "main"), "main · ⧉ a b.rs:3");
        // The terminal's own state saw none of it.
        assert_eq!(with_seen("main"), "main");

        let link = held(&TESTS).link.clone().unwrap();
        send(&TESTS, &link, &json!({"type": "close_diff", "id": "x"}));
        let (hello, asked, _open) = server.join().unwrap();
        let hello: Value = serde_json::from_str(&hello).unwrap();
        assert_eq!(hello["type"], "hello");
        assert_eq!(hello["token"], "the-token");
        let asked: Value = serde_json::from_str(&asked).unwrap();
        assert_eq!(asked, json!({"type": "close_diff", "id": "x"}));
        assert_eq!(unlink(&TESTS).as_deref(), Some("Test Code"));
        assert_eq!(seen_in(&TESTS, "main"), "main");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_editor_that_does_not_welcome_the_terminal_is_not_linked() {
        let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("refused");
        let editor = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = editor.local_addr().unwrap().port();
        note(&dir, port, std::process::id(), &[&dir], "wrong");
        // As the extension does with a token it does not know: closed unread.
        let server = std::thread::spawn(move || drop(editor.accept().unwrap()));
        let refused = link_in(&TESTS, &dir, &dir, None).unwrap_err();
        assert!(refused.contains("Test Code"), "{refused}");
        assert!(held(&TESTS).link.is_none());
        server.join().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_diff_is_named_plainly_and_opens_where_the_change_begins() {
        assert_eq!(plain("toolu_01AB-c"), "toolu_01AB-c");
        assert_eq!(plain("functions.edit_file:0"), "functions_edit_file_0");
        assert_eq!(plain(""), "change");
        assert_eq!(first_change("a\nb\nc\n", "a\nB\nc\n"), 2);
        assert_eq!(first_change("", "new\n"), 1);
    }
}
