//! What a browser session downloads: let in only around an agent's action,
//! capped, and moved into the conversation's downloads folder, inert.
//!
//! Each session keeps one connection to the browser itself, its keeper:
//! Chromium drops a download rule once the connection that set it closes,
//! and the download events come only on a browser connection -- for every
//! context at once, so each download is claimed by the frame that began it
//! or by where its file lands. Downloads are allowed in a window: while an
//! action runs and for a while after it, as a page often builds a file
//! before it hands it over. A thread reads the connection only while a
//! window is open; outside one a browser with a window goes back to its own
//! way (the user's Downloads folder) and a hidden one refuses them, and
//! nothing reads the connection, so an idle session costs nothing.
//!
//! A kept file lands in the project, where a test runner or a shell could
//! pick it up, so only names of inert kinds are kept as they are; anything
//! else gets ".download" after it, and the folder ignores itself for git.

use crate::cancel::Cancel;
use crate::cdp::{self, Event, Ignored, Reach, Rule, Tab};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The most one action's downloads may come to together, in bytes: a long
/// report, a data export or a folder of photos fits, and past that it is a
/// job for a command.
pub(crate) const MAX_TOTAL: u64 = 100 << 20;
/// Downloads one action keeps: a page that starts more at once is not doing
/// what the user asked.
pub(crate) const MAX_DOWNLOADS: usize = 5;
/// How long one download may take: 100 MB at about 14 Mbit/s. One still
/// going then is stopped rather than left half-written.
pub(crate) const DOWNLOAD_TIME: Duration = Duration::from_secs(60);
/// Names of kept files are cut to this many bytes, the extension kept: well
/// under the 255 bytes file systems allow, with room for "-NN" and
/// ".download".
const NAME_BYTES: usize = 150;
/// Extensions kept as they are: documents, pictures, data, archives and
/// media, which nothing runs by being there. Any other name, or none, gets
/// ".download" after it.
const INERT: &[&str] = &[
    "pdf", "png", "jpg", "jpeg", "gif", "webp", "bmp", "csv", "tsv", "txt", "md", "json", "xml",
    "zip", "gz", "tgz", "tar", "7z", "docx", "xlsx", "pptx", "odt", "ods", "odp", "epub", "mp3",
    "wav", "ogg", "mp4", "webm", "mov",
];

/// Whether a character is kept in a name: not a control character, nor one
/// of the invisible ones that turn text around or hide in it, with which
/// "invoice\u{202E}fdp.exe" shows as "invoiceexe.pdf".
fn shown_as_is(c: char) -> bool {
    !c.is_control()
        && !matches!(c,
            '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}'
            | '\u{FEFF}' | '\u{AD}' | '\u{61C}')
}

/// Whether a name ends in an extension kept as it is.
fn inert(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(stem, extension)| {
        !stem.is_empty() && INERT.contains(&extension.to_ascii_lowercase().as_str())
    })
}

/// `text` cut to at most `bytes` bytes, on a character's edge.
fn cut_bytes(text: &str, bytes: usize) -> &str {
    let mut end = text.len().min(bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// A name for a downloaded file from the one the page suggests, which the
/// page chose: its last part only, without what a file system refuses or a
/// character that hides, a leading dot made "_" so it is not hidden, a
/// device name Windows keeps made harmless, and ".download" after anything
/// not of an inert kind.
pub(crate) fn saved_name(suggested: &str) -> String {
    let last = suggested.rsplit(['/', '\\']).next().unwrap_or_default();
    let clean: String = last
        .chars()
        .filter(|&c| shown_as_is(c) || c.is_control())
        .map(|c| match c.is_control() || "<>:\"|?*".contains(c) {
            true => '_',
            false => c,
        })
        .collect();
    let clean = clean.trim().trim_end_matches(['.', ' ']);
    let mut name = match clean.len() > NAME_BYTES {
        false => clean.to_owned(),
        true => {
            let extension = clean
                .rsplit_once('.')
                .map(|(_, extension)| extension)
                .filter(|extension| extension.len() <= 10)
                .map(|extension| format!(".{extension}"))
                .unwrap_or_default();
            let stem = cut_bytes(clean, NAME_BYTES - extension.len());
            format!("{}{extension}", stem.trim_end_matches(['.', ' ']))
        }
    };
    if name.starts_with('.') {
        name.replace_range(..1, "_");
    }
    if name.is_empty() {
        name = "download".into();
    }
    if crate::paths::on_windows(&name, None).is_some() {
        name.insert(0, '_');
    }
    if !inert(&name) {
        name.push_str(".download");
    }
    name
}

/// The name a page gave a download, as the model is told it: on one line,
/// nothing hidden in it, and not too long.
pub(crate) fn told(suggested: &str) -> String {
    let shown: String = suggested.chars().filter(|&c| shown_as_is(c)).collect();
    cut_bytes(&shown, NAME_BYTES).to_owned()
}

/// The conversation's downloads folder under `root`, made one part at a
/// time so nothing is made where a link or a file in the way would lead,
/// with a .gitignore of "*" in `media/downloads` when this made it, so a
/// routine commit leaves what pages sent out.
fn made_folder(root: &Path, conversation: u64) -> Result<PathBuf, String> {
    let mut folder = root.to_path_buf();
    for part in ["media", "downloads", &conversation.to_string()] {
        folder.push(part);
        let made = match std::fs::symlink_metadata(&folder) {
            Ok(found) if found.is_dir() => false,
            Ok(_) => {
                return Err(format!(
                    "{} is a link or a file, not a folder of the project.",
                    folder.display()
                ));
            }
            Err(_) => {
                std::fs::create_dir(&folder)
                    .map_err(|e| format!("Cannot make {}: {e}", folder.display()))?;
                true
            }
        };
        if made && part == "downloads" {
            let _ = std::fs::write(folder.join(".gitignore"), "*\n");
        }
    }
    Ok(folder)
}

/// Put a finished download at a free name in `folder`, never over anything
/// there: linked in place, or copied where the folder is on another disk,
/// the part-written copy taken away if that fails. Where it went.
fn place(staged: &Path, folder: &Path, name: &str) -> Result<PathBuf, String> {
    loop {
        let path = free_path(folder, name);
        let linked = std::fs::hard_link(staged, &path);
        let placed = match linked {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => copy_new(staged, &path),
        };
        return match placed {
            Ok(()) => Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => Err(format!("Cannot save {}: {e}", path.display())),
        };
    }
}

/// Copy `from` into a new file at `to`, which must not exist yet, and take
/// the copy away again if it cannot be finished.
fn copy_new(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut source = std::fs::File::open(from)?;
    let mut target = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)?;
    std::io::copy(&mut source, &mut target)
        .map(drop)
        .inspect_err(|_| {
            let _ = std::fs::remove_file(to);
        })
}

/// Move a finished download from where the browser left it into the
/// conversation's downloads folder under `root`, the project or Letronna's
/// own folder. A note naming the file as the page named it, where it was
/// saved, its size and what it holds.
pub(crate) fn keep(
    staged: &Path,
    suggested: &str,
    root: &Path,
    conversation: u64,
) -> Result<String, String> {
    let name = told(suggested);
    let lost = || format!("The download of {name} finished, but its file was not found.");
    let size = std::fs::metadata(staged).map_err(|_| lost())?.len();
    if size > MAX_TOTAL {
        return Err(format!(
            "The download of {name} was thrown away: it is over {} MB.",
            MAX_TOTAL >> 20
        ));
    }
    let thrown = |why: String| format!("The download of {name} was thrown away: {why}");
    let folder = made_folder(root, conversation).map_err(thrown)?;
    let path = place(staged, &folder, &saved_name(suggested)).map_err(thrown)?;
    let _ = std::fs::remove_file(staged);
    let mut head = Vec::new();
    if let Ok(file) = std::fs::File::open(&path) {
        let _ = std::io::Read::read_to_end(&mut std::io::Read::take(file, 512), &mut head);
    }
    Ok(format!(
        "Downloaded {name}: {}, {}. Saved to {}",
        crate::media::size(size as usize),
        kind_of(&head),
        path.display()
    ))
}

/// `name` in `folder`, or with -2, -3 and so on before its extension when
/// something is already there.
fn free_path(folder: &Path, name: &str) -> PathBuf {
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, format!(".{extension}")),
        _ => (name, String::new()),
    };
    let mut path = folder.join(name);
    let mut n = 2;
    // A link that leads nowhere is something there too.
    while std::fs::symlink_metadata(&path).is_ok() {
        path = folder.join(format!("{stem}-{n}{extension}"));
        n += 1;
    }
    path
}

/// What a file holds, by its first bytes rather than the name the page gave
/// it.
fn kind_of(head: &[u8]) -> &'static str {
    if head.is_empty() {
        return "empty";
    }
    const KINDS: &[(&[u8], &str)] = &[
        (b"%PDF-", "application/pdf"),
        (b"\x89PNG", "image/png"),
        (b"\xFF\xD8\xFF", "image/jpeg"),
        (b"GIF8", "image/gif"),
        (b"PK\x03\x04", "application/zip"),
        (b"\x1F\x8B", "application/gzip"),
    ];
    if let Some((_, kind)) = KINDS.iter().find(|(magic, _)| head.starts_with(magic)) {
        return kind;
    }
    if head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP") {
        return "image/webp";
    }
    // Only its start was read, which may end part way through a character.
    let text = match std::str::from_utf8(head) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none(),
    };
    match text && !head.contains(&0) {
        true => "text",
        false => "application/octet-stream",
    }
}

/// What to do about a download, as a window's tally decides.
#[derive(Debug, PartialEq)]
enum Step {
    /// Cancel it, and tell the model why.
    Stop { guid: String, note: String },
    /// It ended by itself; tell the model how.
    Say(String),
    /// It finished within bounds: keep its file.
    Keep { guid: String, name: String },
}

/// A download under way, as its events have told of it.
struct Going {
    /// The name the page gave it, as the model is told it.
    name: String,
    /// Its size so far, or the whole of it once the browser knows that.
    bytes: u64,
    began: Instant,
}

/// One window's downloads, counted together: at most `MAX_DOWNLOADS` of
/// them and `MAX_TOTAL` bytes between them, each done within
/// `DOWNLOAD_TIME`, and none begun once the window is over.
#[derive(Default)]
struct Tally {
    going: HashMap<String, Going>,
    /// How many were let begin.
    taken: usize,
    /// Bytes of those already kept.
    kept: u64,
    /// Set once the window is over.
    over: bool,
}

impl Tally {
    /// A download began: taken, or stopped at once when the window is over
    /// or has had its share.
    fn begin(&mut self, guid: &str, name: &str, now: Instant) -> Option<Step> {
        let why = match (self.over, self.taken >= MAX_DOWNLOADS) {
            (true, _) => "it began after the action was over".to_owned(),
            (false, true) => format!("an action keeps at most {MAX_DOWNLOADS} downloads"),
            (false, false) => {
                self.taken += 1;
                let going = Going {
                    name: name.to_owned(),
                    bytes: 0,
                    began: now,
                };
                self.going.insert(guid.to_owned(), going);
                return None;
            }
        };
        Some(Step::Stop {
            guid: guid.to_owned(),
            note: format!("The page started downloading {name}, and it was refused: {why}."),
        })
    }

    /// A download's progress: how far it got, its whole size if known, and
    /// its state. Crossing `MAX_TOTAL` stops every download still going.
    fn progress(&mut self, guid: &str, received: u64, total: u64, state: &str) -> Vec<Step> {
        let Some(going) = self.going.get_mut(guid) else {
            return Vec::new();
        };
        going.bytes = received.max(total);
        let sum = self.kept + self.going.values().map(|g| g.bytes).sum::<u64>();
        if sum > MAX_TOTAL {
            let why = format!(
                "this action's downloads came to over {} MB",
                MAX_TOTAL >> 20
            );
            return self.stop_all(&why);
        }
        let ended = match state {
            "completed" => true,
            "canceled" => false,
            _ => return Vec::new(),
        };
        let Some(going) = self.going.remove(guid) else {
            return Vec::new();
        };
        match ended {
            true => {
                self.kept += going.bytes;
                vec![Step::Keep {
                    guid: guid.to_owned(),
                    name: going.name,
                }]
            }
            false => vec![Step::Say(format!(
                "The download of {} did not finish.",
                going.name
            ))],
        }
    }

    /// Stop every download that has taken longer than `DOWNLOAD_TIME`.
    fn overdue(&mut self, now: Instant) -> Vec<Step> {
        let late: Vec<String> = (self.going.iter())
            .filter(|(_, going)| now.duration_since(going.began) >= DOWNLOAD_TIME)
            .map(|(guid, _)| guid.clone())
            .collect();
        let why = format!("it was not done after {} seconds", DOWNLOAD_TIME.as_secs());
        late.into_iter()
            .filter_map(|guid| self.stop(&guid, &why))
            .collect()
    }

    /// Stop every download still going, saying why.
    fn stop_all(&mut self, why: &str) -> Vec<Step> {
        let guids: Vec<String> = self.going.keys().cloned().collect();
        guids
            .into_iter()
            .filter_map(|guid| self.stop(&guid, why))
            .collect()
    }

    fn stop(&mut self, guid: &str, why: &str) -> Option<Step> {
        let going = self.going.remove(guid)?;
        Some(Step::Stop {
            guid: guid.to_owned(),
            note: format!("The download of {} was stopped: {why}.", going.name),
        })
    }
}

impl Drop for Keeper {
    /// The staging folder goes with the keeper, whose thread holds it until
    /// nothing is going.
    fn drop(&mut self) {
        let inner = self.inner.get_mut().unwrap_or_else(PoisonError::into_inner);
        let _ = std::fs::remove_dir_all(&inner.staging);
    }
}

/// How long downloads stay let in after an action: a page that builds a
/// file first, as an export does, hands it over within this.
const GRACE: Duration = Duration::from_secs(30);
/// How long the thread reads at a time before it looks at the clock and at
/// the user's stop again.
const READ_STEP: Duration = Duration::from_millis(100);

/// A session's hold on its downloads, shared with the thread that reads
/// the browser while a window is open.
pub(crate) struct Keeper {
    inner: Mutex<Inner>,
    /// Woken each time the thread has dealt with what it read.
    read: Condvar,
}

struct Inner {
    /// The connection calls go over; `None` once it broke.
    calls: Option<Tab>,
    /// The connection the download events come on, here while no thread
    /// reads it. It holds the rule too, as its own last word on it.
    events: Option<Tab>,
    /// Where the browser saves while a window is open, each file named by
    /// its id.
    staging: PathBuf,
    /// The session's private context, or `None` for the browser's own.
    context: Option<String>,
    /// That context as the browser names it, which a frame is matched to.
    mine: String,
    /// Whether the browser has a window, which outside a window saves
    /// downloads its own way, where a hidden one refuses them.
    headed: bool,
    window: Option<Window>,
    /// Downloads begun in a frame that is not a tab, by name, until their
    /// file shows whose they are.
    unclaimed: HashMap<String, String>,
    /// What the model has not been told yet.
    notes: Vec<String>,
    /// Whether a thread reads the events now.
    reading: bool,
    /// How many times the thread has read, so a call can wait until what
    /// came before it was dealt with.
    rounds: u64,
    /// Why downloads are off, once a connection broke.
    broken: Option<String>,
}

/// The time downloads are let in: an action and the grace after it.
struct Window {
    /// The project, or Letronna's own folder without one; `None` in plan
    /// mode, which refuses every download.
    root: Option<PathBuf>,
    conversation: u64,
    /// The call's stop, which ends the window too.
    cancel: Cancel,
    /// When downloads stop being let in; `None` while the action runs.
    until: Option<Instant>,
    tally: Tally,
}

impl Keeper {
    /// Take hold of the downloads of a session whose tab is `tab`, in
    /// `context`: refused, or with a window the browser's own way, until an
    /// action opens a window. Its files are staged in a folder of its own
    /// under `folder`, which no other keeper shares even for the same
    /// conversation, so one going away never takes another's.
    pub(crate) fn start(
        reach: &Reach,
        tab: &str,
        context: Option<&str>,
        folder: &Path,
    ) -> Result<Arc<Keeper>, String> {
        static KEEPERS: AtomicU64 = AtomicU64::new(0);
        let staging = folder.join(KEEPERS.fetch_add(1, Ordering::Relaxed).to_string());
        std::fs::create_dir_all(&staging)
            .map_err(|e| format!("cannot make {}: {e}", staging.display()))?;
        let headed = !reach.headless;
        let idle = idle_rule(headed);
        let events = reach.keep_downloads(idle, context)?;
        let mut calls = reach.keep_downloads(idle, context)?;
        let info: Value = calls.call("Target.getTargetInfo", json!({"targetId": tab}))?;
        let mine = info["targetInfo"]["browserContextId"]
            .as_str()
            .ok_or("the browser did not say which context the tab is in")?
            .to_owned();
        let inner = Inner {
            calls: Some(calls),
            events: Some(events),
            staging,
            context: context.map(str::to_owned),
            mine,
            headed,
            window: None,
            unclaimed: HashMap::new(),
            notes: Vec::new(),
            reading: false,
            rounds: 0,
            broken: None,
        };
        Ok(Arc::new(Keeper {
            inner: Mutex::new(inner),
            read: Condvar::new(),
        }))
    }

    /// Let downloads in for an action about to run, into `root`'s folder
    /// for `conversation`, or refuse them all in plan mode (`None`). A
    /// window still open from the last action ends first.
    pub(crate) fn open(
        self: &Arc<Self>,
        root: Option<PathBuf>,
        conversation: u64,
        cancel: &Cancel,
    ) {
        let mut inner = self.lock();
        inner.end_window("the next action began");
        // What came while nobody read is dealt with as having come outside
        // a window, which it did.
        if !inner.reading
            && let Some(mut events) = inner.events.take()
        {
            let heard = events.wait(Duration::from_millis(10), |_| false);
            inner.deal_with(&mut events, heard.map(drop));
            inner.events = (!events.broken).then_some(events);
        }
        let rule = root
            .as_ref()
            .map_or(Rule::Refuse, |_| Rule::Save(&inner.staging));
        let rule = cdp::download_rule(rule, inner.context.as_deref());
        inner.set_rule(rule);
        if inner.broken.is_some() {
            return;
        }
        inner.window = Some(Window {
            root,
            conversation,
            cancel: cancel.clone(),
            until: None,
            tally: Tally::default(),
        });
        if !inner.reading
            && let Some(events) = inner.events.take()
        {
            inner.reading = true;
            let keeper = self.clone();
            let spawned = std::thread::Builder::new()
                .name("downloads".into())
                .spawn(move || keeper.read_while_open(events));
            if spawned.is_err() {
                inner.reading = false;
                inner.broke("they could not be watched".into());
            }
        }
    }

    /// Wait, outside the browser's lock, until the downloads the action
    /// began are done, at most `DOWNLOAD_TIME` or until the user stops, then
    /// start the grace after it. What there is to tell the model.
    pub(crate) fn see_through(&self, cancel: &Cancel) -> Vec<String> {
        let deadline = Instant::now() + DOWNLOAD_TIME;
        let mut inner = self.lock();
        // Two reads on: what the browser said before this was dealt with.
        let caught_up = inner.rounds + 2;
        loop {
            let going = inner
                .window
                .as_ref()
                .is_some_and(|w| !w.tally.going.is_empty());
            let behind = inner.reading && inner.rounds < caught_up;
            if !(going || behind) || cancel.is_set() || Instant::now() >= deadline {
                break;
            }
            inner = match self.read.wait_timeout(inner, READ_STEP * 2) {
                Ok((inner, _)) => inner,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
        if cancel.is_set() {
            inner.end_window("it was stopped");
        }
        inner.grace();
        inner.told()
    }

    /// Start the grace after an action without waiting for its downloads,
    /// which are told of with the next result.
    pub(crate) fn grace(&self) {
        self.lock().grace();
    }

    /// What there is to tell the model that it has not been told yet.
    pub(crate) fn notes(&self) -> Vec<String> {
        self.lock().told()
    }

    /// Why downloads are off for the session, once they are.
    pub(crate) fn broken(&self) -> Option<String> {
        self.lock().broken.clone()
    }

    /// Let go of the downloads when the session closes: whatever is going
    /// is stopped and the staging folder removed.
    pub(crate) fn close(&self) {
        let mut inner = self.lock();
        inner.end_window("the browser was closed");
        inner.calls = None;
        inner.events = None;
        let _ = std::fs::remove_dir_all(&inner.staging);
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // Nothing under the lock leaves it half-changed on a panic.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The rule outside a window: a browser with a window saves the user's own
/// downloads its own way, as it did before Letronna watched them; a hidden
/// one has nobody to save them for.
fn idle_rule(headed: bool) -> Rule<'static> {
    match headed {
        true => Rule::Own,
        false => Rule::Refuse,
    }
}

impl Inner {
    /// Let downloads in only for `GRACE` more, as the action is over.
    fn grace(&mut self) {
        if let Some(window) = self.window.as_mut().filter(|w| w.until.is_none()) {
            window.until = Some(Instant::now() + GRACE);
        }
    }

    /// What the model has not been told yet, taken.
    fn told(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// Downloads are off from now on: the connections are let go of, which
    /// sends the browser back to its own way, so whoever holds the session
    /// refuses them on its tab (see `broken`).
    fn broke(&mut self, why: String) {
        self.notes
            .push(format!("Downloads are off for this browser: {why}."));
        if let Some(window) = &mut self.window {
            let steps = window.tally.stop_all(&why);
            self.carry_out(steps);
        }
        self.window = None;
        self.calls = None;
        self.events = None;
        self.broken = Some(why);
    }

    /// Put a download rule in place, or note that it could not be.
    fn set_rule(&mut self, rule: Value) {
        let Some(calls) = &mut self.calls else {
            return;
        };
        if let Err(e) = calls.call::<Ignored>("Browser.setDownloadBehavior", rule) {
            self.broke(format!(
                "the browser would not take its download rule ({e})"
            ));
        }
    }

    /// End the window, if one is open: whatever is going is stopped, saying
    /// `why`, the rule goes back to the one outside a window and what is
    /// left in staging is thrown away.
    fn end_window(&mut self, why: &str) {
        let Some(mut window) = self.window.take() else {
            return;
        };
        let steps = window.tally.stop_all(why);
        self.carry_out(steps);
        let rule = cdp::download_rule(idle_rule(self.headed), self.context.as_deref());
        self.set_rule(rule);
        self.clear_staging();
    }

    /// Stop and throw away whatever is left in staging -- a download begun
    /// the moment the rule changed -- so nothing grows there unwatched.
    fn clear_staging(&mut self) {
        let Ok(entries) = std::fs::read_dir(&self.staging) else {
            return;
        };
        let names: Vec<String> = entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
            .collect();
        for name in names {
            self.cancel(name.trim_end_matches(".crdownload"));
            let _ = std::fs::remove_file(self.staging.join(&name));
        }
    }

    /// Cancel a download, if it is in the session's context -- the browser
    /// finds none in another -- and throw away what it wrote.
    fn cancel(&mut self, guid: &str) {
        if let Some(calls) = &mut self.calls {
            let params = cdp::in_context(json!({"guid": guid}), self.context.as_deref());
            let _ = calls.call::<Ignored>("Browser.cancelDownload", params);
        }
        for name in [guid.to_owned(), format!("{guid}.crdownload")] {
            let _ = std::fs::remove_file(self.staging.join(name));
        }
    }
}

impl Inner {
    /// Deal with what was read from the events connection, or with its
    /// breaking.
    fn deal_with(&mut self, events: &mut Tab, heard: Result<(), String>) {
        let read: Vec<Event> = events.events.drain(..).collect();
        for event in read {
            self.heard(&event);
        }
        if let Err(e) = heard {
            self.broke(format!("the browser stopped telling of them ({e})"));
        }
    }

    /// Deal with one download event, from any context of the browser.
    fn heard(&mut self, event: &Event) {
        let text = |key: &str| event.params[key].as_str().unwrap_or_default();
        let guid = text("guid");
        // The id names a file in staging, so only the browser's own shape of
        // it is taken.
        if guid.is_empty() || !guid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return;
        }
        match event.method.as_str() {
            "Browser.downloadWillBegin" => {
                let name = told(text("suggestedFilename"));
                match self.whose(text("frameId")) {
                    Some(true) => self.began(guid, &name),
                    Some(false) => {}
                    None => {
                        self.unclaimed.insert(guid.to_owned(), name);
                    }
                }
            }
            "Browser.downloadProgress" => {
                let state = text("state");
                if self.unclaimed.contains_key(guid) {
                    let staged = |name: String| self.staging.join(name).exists();
                    let landed = staged(guid.to_owned()) || staged(format!("{guid}.crdownload"));
                    let name = self.unclaimed.remove(guid).unwrap_or_default();
                    match (landed, state) {
                        (true, _) => self.began(guid, &name),
                        // Not here yet: it may still be put here.
                        (false, "inProgress") => {
                            self.unclaimed.insert(guid.to_owned(), name);
                        }
                        (false, _) => {}
                    }
                }
                let bytes = |key: &str| event.params[key].as_f64().unwrap_or(0.) as u64;
                let Some(window) = &mut self.window else {
                    return;
                };
                let steps = (window.tally).progress(
                    guid,
                    bytes("receivedBytes"),
                    bytes("totalBytes"),
                    state,
                );
                self.carry_out(steps);
            }
            _ => {}
        }
    }

    /// Whether a download begun in `frame` is this session's: a tab says
    /// which context it is in; a frame inside a page is not a tab, so that
    /// is not known yet (`None`).
    fn whose(&mut self, frame: &str) -> Option<bool> {
        let calls = self.calls.as_mut()?;
        let info: Value = calls
            .call("Target.getTargetInfo", json!({"targetId": frame}))
            .ok()?;
        Some(info["targetInfo"]["browserContextId"] == self.mine.as_str())
    }
}

impl Inner {
    /// One of the session's downloads began.
    fn began(&mut self, guid: &str, name: &str) {
        let headed = self.headed;
        // Outside a window the rule outside one dealt with it: a browser with
        // a window saved it its own way, as it may be the user's; a hidden
        // one refused it, which the model is told.
        let Some(window) = self.window.as_mut().filter(|w| !(w.tally.over && headed)) else {
            if !headed {
                let note = format!(
                    "The page started downloading {name} after the action was over, and it \
                     was refused."
                );
                self.notes.push(note);
            }
            return;
        };
        let step = match window.root {
            None => Some(Step::Stop {
                guid: guid.to_owned(),
                note: format!(
                    "The page started downloading {name}, and it was refused: plan mode keeps \
                     nothing."
                ),
            }),
            Some(_) => window.tally.begin(guid, name, Instant::now()),
        };
        self.carry_out(step.into_iter().collect());
    }

    /// Do what the tally decided.
    fn carry_out(&mut self, steps: Vec<Step>) {
        for step in steps {
            match step {
                Step::Stop { guid, note } => {
                    self.cancel(&guid);
                    self.notes.push(note);
                }
                Step::Say(note) => self.notes.push(note),
                Step::Keep { guid, name } => {
                    let staged = self.staging.join(&guid);
                    let kept = match &self.window {
                        Some(Window {
                            root: Some(root),
                            conversation,
                            ..
                        }) => keep(&staged, &name, root, *conversation),
                        _ => Err(format!("The download of {name} was thrown away.")),
                    };
                    let _ = std::fs::remove_file(&staged);
                    self.notes.push(kept.unwrap_or_else(|why| why));
                }
            }
        }
    }
}

impl Inner {
    /// Look at the clock and the stop: overdue downloads are stopped, and
    /// once the grace is over new ones follow the rule outside a window and
    /// the window ends when nothing is left going. Whether to read on.
    fn tick(&mut self, now: Instant) -> bool {
        let Some(window) = &mut self.window else {
            return false;
        };
        if window.cancel.is_set() {
            self.end_window("it was stopped");
            return false;
        }
        let steps = window.tally.overdue(now);
        self.carry_out(steps);
        let Some(window) = &mut self.window else {
            return false;
        };
        if !window.until.is_some_and(|until| now >= until) {
            return true;
        }
        if !window.tally.over {
            window.tally.over = true;
            let rule = cdp::download_rule(idle_rule(self.headed), self.context.as_deref());
            self.set_rule(rule);
        }
        let done = self
            .window
            .as_ref()
            .is_none_or(|window| window.tally.going.is_empty());
        if done {
            self.end_window("the time for downloads was over");
        }
        !done
    }
}

impl Keeper {
    /// Read the browser's download events until the window ends, dealing
    /// with each as it comes, then give the connection back.
    fn read_while_open(self: Arc<Self>, mut events: Tab) {
        loop {
            let heard = events.wait(READ_STEP, |_| true).map(drop);
            let mut inner = self.lock();
            inner.deal_with(&mut events, heard);
            let open = inner.broken.is_none() && inner.tick(Instant::now());
            inner.rounds += 1;
            self.read.notify_all();
            if !open {
                inner.reading = false;
                if inner.calls.is_some() {
                    inner.events = Some(events);
                }
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_downloaded_file_keeps_only_a_safe_name_from_what_the_page_suggests() {
        assert_eq!(saved_name("report.pdf"), "report.pdf");
        assert_eq!(saved_name("Report.PDF"), "Report.PDF");
        assert_eq!(
            saved_name("../../.ssh/authorized_keys"),
            "authorized_keys.download"
        );
        assert_eq!(saved_name(r"C:\Users\x\evil.exe"), "evil.exe.download");
        assert_eq!(saved_name("a:b?<c>.pdf"), "a_b__c_.pdf");
        assert_eq!(saved_name("report.pdf. "), "report.pdf");
        assert_eq!(saved_name("line\nbreak.txt"), "line_break.txt");
        assert_eq!(saved_name(""), "download.download");
        assert_eq!(saved_name(".."), "download.download");
        assert_eq!(saved_name("CON.txt"), "_CON.txt");
        // Nothing hides in a name to make it read as another.
        assert_eq!(
            saved_name("invoice\u{202E}fdp.exe"),
            "invoicefdp.exe.download"
        );
        assert_eq!(saved_name("a\u{200B}b.csv"), "ab.csv");
    }

    #[test]
    fn a_name_a_runner_or_a_shell_would_pick_up_is_kept_inert() {
        for (page, kept) in [
            ("conftest.py", "conftest.py.download"),
            ("login.test.js", "login.test.js.download"),
            ("x_test.go", "x_test.go.download"),
            ("x.url", "x.url.download"),
            ("desktop.ini", "desktop.ini.download"),
            ("Makefile", "Makefile.download"),
            (".env", "_env.download"),
            (".gitignore", "_gitignore.download"),
            ("photo.jpeg", "photo.jpeg"),
            ("data.tar.gz", "data.tar.gz"),
        ] {
            assert_eq!(saved_name(page), kept, "{page}");
        }
    }

    #[test]
    fn a_long_name_is_cut_by_bytes_on_a_characters_edge_keeping_its_extension() {
        let latin = saved_name(&format!("{}.pdf", "a".repeat(300)));
        assert!(
            latin.len() == NAME_BYTES && latin.ends_with("a.pdf"),
            "{latin}"
        );
        // Three bytes each: 100 of them would be 300 bytes, past what a file
        // system takes.
        let wide = saved_name(&format!("{}.csv", "\u{6587}".repeat(100)));
        assert!(
            wide.len() <= NAME_BYTES && wide.ends_with("\u{6587}.csv"),
            "{wide}"
        );
        let code = saved_name(&format!("{}.py", "b".repeat(300)));
        assert!(code.len() <= NAME_BYTES + ".download".len(), "{code}");
        assert!(code.ends_with(".py.download"), "{code}");
    }

    #[test]
    fn the_name_the_model_is_told_is_on_one_line_with_nothing_hidden() {
        assert_eq!(told("a\nb\u{202E}c.pdf"), "abc.pdf");
        assert!(told(&"x".repeat(400)).len() == NAME_BYTES);
    }

    /// The notes of the stops among `steps`, in order of their downloads.
    fn stopped(steps: &[Step]) -> Vec<String> {
        let mut guids: Vec<String> = (steps.iter())
            .filter_map(|step| match step {
                Step::Stop { guid, .. } => Some(guid.clone()),
                _ => None,
            })
            .collect();
        guids.sort();
        guids
    }

    #[test]
    fn a_download_past_the_fifth_of_an_action_is_refused_as_it_begins() {
        let (mut tally, now) = (Tally::default(), Instant::now());
        for n in 1..=MAX_DOWNLOADS {
            assert_eq!(tally.begin(&format!("g{n}"), "a.csv", now), None);
        }
        let Some(Step::Stop { guid, note }) = tally.begin("g6", "b.csv", now) else {
            panic!("the sixth was taken");
        };
        assert_eq!(guid, "g6");
        assert!(
            note.contains("b.csv") && note.contains("at most 5"),
            "{note}"
        );
        assert_eq!(tally.going.len(), MAX_DOWNLOADS);
    }

    #[test]
    fn the_downloads_of_an_action_share_one_cap_and_all_stop_when_it_is_crossed() {
        let (mut tally, now) = (Tally::default(), Instant::now());
        for guid in ["g1", "g2", "g3"] {
            tally.begin(guid, guid, now);
        }
        let half = MAX_TOTAL / 2;
        assert!(tally.progress("g1", half, 0, "inProgress").is_empty());
        // One finishes first and is kept, and counts towards the cap.
        let kept = tally.progress("g2", 10, 10, "completed");
        let name = "g2".to_owned();
        assert_eq!(
            kept,
            [Step::Keep {
                guid: name.clone(),
                name
            }]
        );
        // Its whole size, once known, counts before it has all come.
        let crossed = tally.progress("g3", 0, half, "inProgress");
        assert_eq!(stopped(&crossed), ["g1", "g3"]);
        assert!(tally.going.is_empty());
        // Nothing is heard of a download the tally has let go of.
        assert!(tally.progress("g1", half, 0, "inProgress").is_empty());
    }

    #[test]
    fn a_download_that_fails_is_said_and_one_that_takes_too_long_is_stopped() {
        let (mut tally, now) = (Tally::default(), Instant::now());
        tally.begin("g1", "a.csv", now);
        tally.begin("g2", "b.csv", now + Duration::from_secs(30));
        let failed = tally.progress("g1", 5, 10, "canceled");
        assert_eq!(
            failed,
            [Step::Say("The download of a.csv did not finish.".into())]
        );
        assert!(tally.overdue(now + DOWNLOAD_TIME).is_empty());
        let late = tally.overdue(now + Duration::from_secs(30) + DOWNLOAD_TIME);
        assert_eq!(stopped(&late), ["g2"]);
    }

    #[test]
    fn a_download_that_begins_once_the_window_is_over_is_refused() {
        let (mut tally, now) = (Tally::default(), Instant::now());
        tally.over = true;
        let Some(Step::Stop { note, .. }) = tally.begin("g1", "late.csv", now) else {
            panic!("a late download was taken");
        };
        assert!(note.contains("after the action was over"), "{note}");
        assert_eq!(tally.taken, 0);
    }

    #[test]
    fn a_download_is_described_by_what_it_holds_not_its_name() {
        assert_eq!(kind_of(b"%PDF-1.7\n"), "application/pdf");
        assert_eq!(kind_of(b"\x89PNG\r\n\x1a\n"), "image/png");
        assert_eq!(kind_of(b"RIFF\0\0\0\0WEBPVP8 "), "image/webp");
        assert_eq!(kind_of(b"PK\x03\x04rest"), "application/zip");
        assert_eq!(kind_of("name,price\nt\u{e9}".as_bytes()), "text");
        // Cut part way through a character, it is still text.
        assert_eq!(kind_of(&"\u{e9}".as_bytes()[..1]), "text");
        assert_eq!(kind_of(b"MZ\x90\0\x03"), "application/octet-stream");
        assert_eq!(kind_of(b""), "empty");
    }

    /// A scratch folder with a staging and a project folder in it.
    fn scratch(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("letronna-{name}-{}", std::process::id()));
        let (staging, project) = (dir.join("staging"), dir.join("project"));
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        (dir, staging, project)
    }

    #[test]
    fn a_finished_download_moves_into_the_session_folder_beside_one_of_the_same_name() {
        let (dir, staging, project) = scratch("keep");
        let folder = project.join("media").join("downloads").join("9");
        for n in 1..=2 {
            let staged = staging.join(format!("guid-{n}"));
            std::fs::write(&staged, b"%PDF-1.7 a report").unwrap();
            let note = keep(&staged, "report.pdf", &project, 9).unwrap();
            let name = if n == 1 { "report.pdf" } else { "report-2.pdf" };
            let path = folder.join(name);
            let told = format!(
                "Downloaded report.pdf: 1 KB, application/pdf. Saved to {}",
                path.display()
            );
            assert_eq!(note, told);
            assert!(!staged.exists() && path.is_file());
        }
        // A name of a kind something runs is kept inert, and the page's name
        // is still told.
        let staged = staging.join("guid-3");
        std::fs::write(&staged, b"import os").unwrap();
        let note = keep(&staged, "conftest.py", &project, 9).unwrap();
        assert!(note.starts_with("Downloaded conftest.py: "), "{note}");
        assert!(
            folder.join("conftest.py.download").is_file() && !folder.join("conftest.py").exists()
        );
        let missing = keep(&staging.join("gone"), "x.txt", &project, 9).unwrap_err();
        assert!(missing.contains("not found"), "{missing}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_downloads_folder_ignores_itself_for_git_when_it_is_made() {
        let (dir, staging, project) = scratch("ignore");
        std::fs::write(staging.join("g"), b"a").unwrap();
        keep(&staging.join("g"), "a.txt", &project, 1).unwrap();
        let ignore = project.join("media/downloads/.gitignore");
        assert_eq!(std::fs::read_to_string(&ignore).unwrap(), "*\n");
        // One the project already had is left as it is.
        std::fs::remove_dir_all(project.join("media")).unwrap();
        std::fs::create_dir_all(project.join("media/downloads")).unwrap();
        std::fs::write(staging.join("g"), b"a").unwrap();
        keep(&staging.join("g"), "a.txt", &project, 1).unwrap();
        assert!(!ignore.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_download_folder_a_link_leads_out_of_the_project_is_not_used_or_made() {
        let (dir, staging, project) = scratch("keep-link");
        let elsewhere = dir.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, project.join("media")).unwrap();
        let staged = staging.join("guid");
        std::fs::write(&staged, b"a,b").unwrap();
        let refused = keep(&staged, "run.csv", &project, 1).unwrap_err();
        assert!(refused.contains("is a link or a file"), "{refused}");
        assert!(!elsewhere.join("downloads").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_copy_never_writes_over_a_file_that_is_there() {
        let (dir, staging, project) = scratch("copy-new");
        std::fs::write(staging.join("g"), b"new").unwrap();
        std::fs::write(project.join("a.txt"), b"old").unwrap();
        let there = copy_new(&staging.join("g"), &project.join("a.txt")).unwrap_err();
        assert_eq!(there.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(project.join("a.txt")).unwrap(), b"old");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_download_from_a_frame_is_claimed_by_where_its_file_lands_and_kept() {
        let (dir, staging, project) = scratch("claim");
        let mut inner = Inner {
            calls: None,
            events: None,
            staging: staging.clone(),
            context: Some("C1".into()),
            mine: "C1".into(),
            headed: false,
            window: Some(Window {
                root: Some(project.clone()),
                conversation: 4,
                cancel: Cancel::default(),
                until: None,
                tally: Tally::default(),
            }),
            unclaimed: HashMap::new(),
            notes: Vec::new(),
            reading: false,
            rounds: 0,
            broken: None,
        };
        let event = |method: &str, params: Value| Event {
            method: format!("Browser.{method}"),
            params,
        };
        let progress = |guid: &str, state: &str| {
            event(
                "downloadProgress",
                json!({"guid": guid, "state": state,
                                              "receivedBytes": 3, "totalBytes": 3}),
            )
        };
        // Two begin in frames that are not tabs: whose they are is not known.
        for guid in ["g1", "g2"] {
            let params = json!({"guid": guid, "frameId": "F", "suggestedFilename": "a.csv"});
            inner.heard(&event("downloadWillBegin", params));
        }
        assert_eq!(inner.unclaimed.len(), 2);
        std::fs::write(staging.join("g1.crdownload"), b"a,b").unwrap();
        inner.heard(&progress("g1", "inProgress"));
        std::fs::rename(staging.join("g1.crdownload"), staging.join("g1")).unwrap();
        inner.heard(&progress("g1", "completed"));
        // The other went to another session's folder, and is not told of.
        inner.heard(&progress("g2", "completed"));
        let saved = project
            .join("media")
            .join("downloads")
            .join("4")
            .join("a.csv");
        let told = format!("Downloaded a.csv: 1 KB, text. Saved to {}", saved.display());
        assert_eq!(inner.told(), [told]);
        assert!(inner.unclaimed.is_empty() && !staging.join("g1").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
