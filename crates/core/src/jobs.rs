//! Background processes started by the agent.
//!
//! A foreground command has to finish inside the timeout, which rules out the
//! ones worth leaving up: dev servers, watchers, tunnels. Those run here
//! instead. The process keeps running, its output collects in a buffer the
//! agent can read back, and the chat shows a chip until it is stopped.
//!
//! A job can also be watched for the lines that matter, such as "ready" or
//! "error". The threads that read its output match each line as it comes, so
//! nothing polls the log, and the owner hears in batches at a limited rate:
//! a word that a chatty job prints all the time would otherwise fill the
//! conversation it was meant to help.

use crate::inbox::{Notice, When};
use crate::tasks::{Key, State, Task};
use std::collections::{BTreeMap, VecDeque};
use std::io::Read;
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Output kept per job. Enough to see what a server is doing without letting a
/// chatty process grow without bound.
const MAX_LOG: usize = 64_000;

/// Lines of output an exit notice carries: the end of a failing test run or a
/// stack trace, with read_job there for the rest.
const NOTICE_LINES: usize = 20;

/// How long an exit notice waits for the last output after the shell exits:
/// a closed pipe drains in milliseconds, but a process the shell left running
/// can hold it open for hours.
const DRAIN_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Characters of a line a watch looks at: a log line with its timestamp and
/// path fits, and output that never ends a line, such as a progress bar,
/// cannot grow the line being read without end.
const MAX_LINE: usize = 500;

/// How long the first matching line waits for more: a stack trace or a test
/// summary comes in a burst, and one notice for the burst reads better than
/// one per line.
const BATCH_WAIT: Duration = Duration::from_millis(200);

/// Characters of matching lines one notice carries: three quarters of the
/// 4,000 a notice may take, leaving room for its first line.
const MAX_BATCH: usize = 3_000;

/// The least time between two notices from one watch, so a job that matches
/// all the time sends at most four a minute.
const QUIET: Duration = Duration::from_secs(15);

/// Notices a watch sends before it gives up: past that the job matches too
/// often for a notice to mean much, and read_job serves better.
const MAX_NOTICES: u32 = 8;

/// Quiet periods in a row with matches to hold back before a watch gives up:
/// 45 seconds of steady matching means the text is too common to watch for.
const MAX_CROWDED: u32 = 3;

pub struct Job {
    pub id: u64,
    pub command: String,
    log: Arc<Mutex<VecDeque<u8>>>,
    tree: Tree,
    /// Set once the process has exited and the reader threads are done.
    finished: Arc<AtomicBool>,
    exit_code: Arc<Mutex<Option<i32>>>,
    watch: Option<Arc<Mutex<Watch>>>,
}

/// What the UI needs to draw a chip. Cloned out so callers hold no locks.
#[derive(Clone)]
pub struct JobInfo {
    pub id: u64,
    pub command: String,
    pub running: bool,
}

fn registry() -> &'static Mutex<BTreeMap<u64, Job>> {
    static JOBS: OnceLock<Mutex<BTreeMap<u64, Job>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Drain a pipe into the shared log until the process closes it, showing
/// each line to the job's watch, if it has one.
fn pump(
    mut source: impl Read + Send + 'static,
    log: Arc<Mutex<VecDeque<u8>>>,
    watch: Option<Arc<Mutex<Watch>>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut block = [0u8; 4096];
        let mut partial = Partial::default();
        loop {
            match source.read(&mut block) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    // Under the log's lock, so a line the watch passes over
                    // before `first_output` is in the text that returns.
                    let Ok(mut log) = log.lock() else { break };
                    if let Some(watch) = &watch {
                        partial.feed(&block[..n], |line| check(watch, line));
                    }
                    append(&mut log, &block[..n]);
                }
            }
        }
        // The last line need not end with a line break.
        if let Some(watch) = &watch
            && let Some(line) = partial.rest()
        {
            check(watch, &line);
        }
    })
}

/// The line a pump is partway through, carried from one read to the next.
#[derive(Default)]
struct Partial(Vec<u8>);

impl Partial {
    /// Add a read's bytes, handing each line they finish to `each`.
    fn feed(&mut self, bytes: &[u8], mut each: impl FnMut(&str)) {
        for piece in bytes.split_inclusive(|byte| *byte == b'\n') {
            let (text, ended) = match piece.strip_suffix(b"\n") {
                Some(text) => (text, true),
                None => (piece, false),
            };
            // A character is at most four bytes, so this still holds the
            // MAX_LINE characters a watch looks at.
            let room = (MAX_LINE * 4).saturating_sub(self.0.len());
            self.0.extend_from_slice(&text[..text.len().min(room)]);
            if ended {
                each(&self.line());
            }
        }
    }

    /// What is left once the pipe has closed, if anything is.
    fn rest(&mut self) -> Option<String> {
        (!self.0.is_empty()).then(|| self.line())
    }

    /// The line so far, cut to MAX_LINE characters, leaving none.
    fn line(&mut self) -> String {
        let line = String::from_utf8_lossy(&self.0)
            .trim_end_matches('\r')
            .chars()
            .take(MAX_LINE)
            .collect();
        self.0.clear();
        line
    }
}

/// What a job is watched for, and what the watch has gathered since its last
/// notice.
struct Watch {
    id: u64,
    command: String,
    needles: Vec<String>,
    /// Set once the job's first output has been shown, since a match in it
    /// is no news.
    shown: bool,
    /// Matching lines for the next notice, at most MAX_BATCH characters.
    lines: Vec<String>,
    /// Characters in `lines`, with a line break after each.
    size: usize,
    /// Matching lines left out of the next notice and only counted.
    more: usize,
    /// Whether a line matched during the quiet period after the last notice.
    held: bool,
    /// Quiet periods in a row that held matches back.
    crowded: u32,
    /// Notices sent so far.
    sent: u32,
    /// When the last notice went, which starts its quiet period.
    last: Option<Instant>,
    /// Whether a flush is waiting to go.
    armed: bool,
    /// Set once the watch has given up or the job has ended.
    off: bool,
}

impl Watch {
    fn new(id: u64, command: &str, needles: &[String]) -> Self {
        Self {
            id,
            command: crate::tools::one_line(command),
            needles: needles.to_vec(),
            shown: false,
            lines: Vec::new(),
            size: 0,
            more: 0,
            held: false,
            crowded: 0,
            sent: 0,
            last: None,
            armed: false,
            off: false,
        }
    }

    /// Take note of a line of output, returning when to flush when it matches
    /// and nothing is waiting to flush yet.
    fn saw(&mut self, line: &str, now: Instant) -> Option<Instant> {
        let wanted = self.needles.iter().any(|needle| line.contains(needle));
        if self.off || !self.shown || !wanted {
            return None;
        }
        let quiet = self.last.map(|last| last + QUIET).filter(|end| now < *end);
        let size = line.chars().count() + 1;
        if quiet.is_some() || self.size + size > MAX_BATCH {
            self.more += 1;
            self.held |= quiet.is_some();
        } else {
            self.lines.push(line.to_owned());
            self.size += size;
        }
        if self.armed {
            return None;
        }
        self.armed = true;
        Some(quiet.unwrap_or(now + BATCH_WAIT))
    }

    /// The notice for what has gathered, or the one that ends the watch when
    /// it has matched too often.
    fn flush(&mut self, now: Instant) -> Option<String> {
        self.armed = false;
        if self.off {
            return None;
        }
        let held = std::mem::take(&mut self.held);
        let text = self.batch()?;
        self.last = Some(now);
        self.sent += 1;
        self.crowded = if held { self.crowded + 1 } else { 0 };
        if self.sent > MAX_NOTICES || self.crowded >= MAX_CROWDED {
            self.off = true;
            return Some(format!(
                "Stopped watching job #{}: it matched too often. Use read_job.",
                self.id
            ));
        }
        Some(text)
    }

    /// Stop watching as the job ends, handing back what had gathered, since
    /// the exit is the next notice anyway.
    fn end(&mut self) -> Option<String> {
        if std::mem::replace(&mut self.off, true) {
            return None;
        }
        self.batch()
    }

    /// The matching lines gathered so far as the text of a notice, leaving
    /// none gathered.
    fn batch(&mut self) -> Option<String> {
        let lines = std::mem::take(&mut self.lines);
        let more = std::mem::take(&mut self.more);
        self.size = 0;
        let count = lines.len() + more;
        if count == 0 {
            return None;
        }
        let s = if count == 1 { "" } else { "s" };
        let mut text = format!(
            "job #{} printed {count} matching line{s} (`{}`)",
            self.id, self.command
        );
        if more > 0 {
            text.push_str(&format!("; read_job has the {more} not shown"));
        }
        if lines.is_empty() {
            text.push('.');
        } else {
            text.push_str(":\n");
            text.push_str(&lines.join("\n"));
        }
        Some(text)
    }
}

/// Show a watch a line of output, setting a timer for its flush when the line
/// is the first to wait for one.
fn check(watch: &Arc<Mutex<Watch>>, line: &str) {
    let due = watch
        .lock()
        .ok()
        .and_then(|mut watch| watch.saw(line, Instant::now()));
    let Some(due) = due else { return };
    let watch = watch.clone();
    crate::timers::at(due, Box::new(move || send(&watch)));
}

/// Tell the job's owner what its watch has gathered, once the flush is due.
fn send(watch: &Mutex<Watch>) {
    let Ok(mut watch) = watch.lock() else { return };
    // Told under the lock, so an exit reported meanwhile cannot come first.
    if let Some(text) = watch.flush(Instant::now()) {
        tell(watch.id, text);
    }
}

/// End a job's watch as the job ends, passing on what it had gathered ahead
/// of the exit notice.
fn close(watch: &Mutex<Watch>) {
    let Ok(mut watch) = watch.lock() else { return };
    if let Some(text) = watch.end() {
        tell(watch.id, text);
    }
}

/// Leave a watch's news for whoever hears from job `id`, where it merges with
/// the job's other notices.
fn tell(id: u64, text: String) {
    let notice = Notice {
        from: format!("job {id}"),
        when: When::Next,
        text,
    };
    crate::tasks::tell(&Key::Job(id), notice);
}

/// Add output to the log, dropping its oldest bytes beyond `MAX_LOG`.
pub(crate) fn append(log: &mut VecDeque<u8>, bytes: &[u8]) {
    log.extend(bytes);
    // Keep the tail: the newest output is what matters.
    let over = log.len().saturating_sub(MAX_LOG);
    log.drain(..over);
}

/// The log as text, starting at its first whole character.
pub(crate) fn text(log: &mut VecDeque<u8>) -> String {
    let bytes = log.make_contiguous();
    // Dropping bytes can leave the tail of a cut character at the front, and a
    // character is at most four bytes, so at most three of them.
    let start = bytes
        .iter()
        .take(3)
        .take_while(|b| **b & 0xC0 == 0x80)
        .count();
    let mut text = String::from_utf8_lossy(&bytes[start..]).into_owned();
    // A byte that was never text decodes to a three-byte replacement
    // character, so keep the limit the text always had as well.
    if text.len() > MAX_LOG {
        let cut = (text.len() - MAX_LOG..text.len())
            .find(|i| text.is_char_boundary(*i))
            .unwrap_or(text.len());
        text.drain(..cut);
    }
    text
}

/// A job's shell and everything it starts, held as one Job Object so that
/// stopping reaches a server the shell launched and not just the shell.
#[cfg(windows)]
struct Tree(std::os::windows::io::OwnedHandle);

#[cfg(windows)]
impl Tree {
    /// Put a just-started shell in a new Job Object that ends its processes when closed.
    fn of(child: &Child) -> Result<Tree, String> {
        use std::os::windows::io::{AsRawHandle, HandleOrNull, OwnedHandle};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };
        let failed = || {
            format!(
                "Cannot group the job's processes: {}",
                std::io::Error::last_os_error()
            )
        };
        // SAFETY: no attributes and no name are both allowed, and the handle
        // it returns, or null, is ours alone.
        let job = unsafe {
            HandleOrNull::from_raw_handle(CreateJobObjectW(std::ptr::null(), std::ptr::null()))
        };
        let Ok(job) = OwnedHandle::try_from(job) else {
            return Err(failed());
        };
        // SAFETY: all zeroes is a valid value of this plain C struct.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        // Letronna holds the only handle, so nothing in the tree outlives it,
        // even when it exits without stopping its jobs.
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the job is open, and `limits` is the struct this class
        // names, passed with its own size.
        let limited = unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if limited == 0 {
            return Err(failed());
        }
        // SAFETY: both handles are open: the job's is owned here and the
        // child is borrowed.
        if unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) } == 0 {
            return Err(failed());
        }
        Ok(Tree(job))
    }

    /// End every process in the tree, with the exit code a killed child gets.
    fn kill(&self) {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        // SAFETY: the handle is open for as long as the tree is.
        unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) };
    }
}

/// A job's shell and everything it starts, as the process group the shell
/// leads.
#[cfg(unix)]
struct Tree(libc::pid_t);

#[cfg(unix)]
impl Tree {
    /// The group a shell started with `process_group(0)` leads.
    fn of(child: &Child) -> Result<Tree, String> {
        Ok(Tree(child.id() as libc::pid_t))
    }

    /// End every process in the tree, with the signal a killed child gets.
    fn kill(&self) {
        // SAFETY: this only sends a signal.
        unsafe { libc::kill(-self.0, libc::SIGKILL) };
    }
}

/// Start a job whose exit nobody is told of.
pub fn start(command: &str, cwd: &Path) -> Result<u64, String> {
    start_for(command, cwd, 0)
}

/// Start a job whose exit is told to `owner`, a session or helper, unless it
/// was stopped; 0 tells nobody.
pub fn start_for(command: &str, cwd: &Path, owner: u64) -> Result<u64, String> {
    start_watching(command, cwd, owner, &[])
}

/// Start a job as `start_for` does, telling `owner` too of the output lines
/// after `first_output` that contain one of `needles`.
pub fn start_watching(
    command: &str,
    cwd: &Path,
    owner: u64,
    needles: &[String],
) -> Result<u64, String> {
    let mut shell = crate::tools::shell(command, Some(cwd));
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut shell, 0);
    let mut child = shell
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Cannot start the shell: {e}"))?;
    // At once, so the shell has had as little time as possible to start
    // anything that would escape the tree.
    let tree = match Tree::of(&child) {
        Ok(tree) => tree,
        Err(e) => {
            let _ = child.kill();
            return Err(e);
        }
    };

    let id = next_id();
    let watch =
        (!needles.is_empty()).then(|| Arc::new(Mutex::new(Watch::new(id, command, needles))));
    let log = Arc::new(Mutex::new(VecDeque::new()));
    let out = pump(child.stdout.take().unwrap(), log.clone(), watch.clone());
    let err = pump(child.stderr.take().unwrap(), log.clone(), watch.clone());

    crate::tasks::register(Task::new(Key::Job(id), owner, None, command));
    let finished = Arc::new(AtomicBool::new(false));
    let exit_code = Arc::new(Mutex::new(None));

    // Reap the process so `running` turns false on its own. Stopping goes
    // through the tree, so this can own the child and wait for it.
    {
        let (finished, exit_code) = (finished.clone(), exit_code.clone());
        let (log, command) = (log.clone(), command.to_owned());
        let watch = watch.clone();
        std::thread::spawn(move || {
            let Ok(status) = child.wait() else { return };
            if let Ok(mut code) = exit_code.lock() {
                *code = status.code();
            }
            finished.store(true, Ordering::Relaxed);
            // The pumps can still be reading the last lines, which are the
            // ones the notice is for.
            let waited = std::time::Instant::now();
            while !(out.is_finished() && err.is_finished()) && waited.elapsed() < DRAIN_WAIT {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            if let Some(watch) = &watch {
                close(watch);
            }
            report_exit(id, &command, status.code(), &log);
        });
    }

    registry()
        .lock()
        .map_err(|_| "Job list is poisoned.")?
        .insert(
            id,
            Job {
                id,
                command: command.to_owned(),
                log,
                tree,
                finished,
                exit_code,
                watch,
            },
        );
    Ok(id)
}

/// Record how a job ended and tell its owner, with the last lines it printed.
fn report_exit(id: u64, command: &str, code: Option<i32>, log: &Mutex<VecDeque<u8>>) {
    let log = log.lock().map(|mut l| text(&mut l)).unwrap_or_default();
    let how = match code {
        Some(code) => format!("exited with code {code}"),
        None => "ended without an exit code".to_owned(),
    };
    let lines: Vec<&str> = log.trim_end().lines().collect();
    let last = lines[lines.len().saturating_sub(NOTICE_LINES)..].join("\n");
    let command = crate::tools::one_line(command);
    let text = if last.trim().is_empty() {
        format!("job #{id} {how} (`{command}`) and printed nothing.")
    } else {
        format!("job #{id} {how} (`{command}`):\n{last}")
    };
    let state = if code == Some(0) {
        State::Done(how)
    } else {
        State::Failed(how)
    };
    let notice = Notice {
        from: format!("job {id}"),
        when: When::Next,
        text,
    };
    crate::tasks::finish(&Key::Job(id), state, Some(notice));
}

fn with_job<T>(id: u64, f: impl FnOnce(&Job) -> T) -> Result<T, String> {
    let jobs = registry().lock().map_err(|_| "Job list is poisoned.")?;
    let job = jobs.get(&id).ok_or_else(|| format!("No job #{id}."))?;
    Ok(f(job))
}

/// Everything the job has printed so far, without escape sequences.
pub fn log(id: u64) -> Result<String, String> {
    // Stripped here rather than as it arrives, because a sequence can
    // straddle two reads.
    let log = with_job(id, |job| {
        job.log
            .lock()
            .map(|mut l| crate::tools::strip_escapes(&text(&mut l)))
            .unwrap_or_default()
    })?;
    // Its start is where the oldest bytes were dropped and its end where a
    // running job has got to, so either could be part of a key.
    Ok(crate::redact::scrub_cut(&log, true))
}

/// Everything the job has printed so far, for whoever started it to show;
/// its watch tells only of the lines that end after this.
pub fn first_output(id: u64) -> Result<String, String> {
    with_job(id, |job| {
        let Ok(mut log) = job.log.lock() else {
            return String::new();
        };
        if let Some(watch) = &job.watch
            && let Ok(mut watch) = watch.lock()
        {
            watch.shown = true;
        }
        text(&mut log)
    })
}

pub fn running(id: u64) -> Result<bool, String> {
    with_job(id, |job| !job.finished.load(Ordering::Relaxed))
}

pub fn exit_code(id: u64) -> Result<Option<i32>, String> {
    with_job(id, |job| job.exit_code.lock().ok().and_then(|c| *c))
}

pub fn stop(id: u64) -> Result<(), String> {
    with_job(id, |job| {
        // Recorded before the kill, so the exit it causes tells nobody: whoever
        // stopped it already knows.
        crate::tasks::finish(&Key::Job(id), State::Stopped, None);
        // Once the shell is reaped, its Unix process group id can pass to an
        // unrelated process, so there a finished job is left alone, as it
        // always was. A Job Object stays Letronna's for the life of the job,
        // so on Windows stopping still ends what an exited shell left running.
        if cfg!(windows) || !job.finished.load(Ordering::Relaxed) {
            job.tree.kill();
        }
    })?;
    Ok(())
}

pub fn list() -> Vec<JobInfo> {
    let Ok(jobs) = registry().lock() else {
        return Vec::new();
    };
    jobs.values()
        .map(|job| JobInfo {
            id: job.id,
            command: job.command.clone(),
            running: !job.finished.load(Ordering::Relaxed),
        })
        .collect()
}

/// Kill every job. Called when the app quits so nothing is left running.
pub fn stop_all() {
    let Ok(jobs) = registry().lock() else { return };
    for job in jobs.values() {
        crate::tasks::finish(&Key::Job(job.id), State::Stopped, None);
        if cfg!(windows) || !job.finished.load(Ordering::Relaxed) {
            job.tree.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sleep_command(seconds: u32) -> String {
        if cfg!(windows) {
            format!("Start-Sleep -Seconds {seconds}")
        } else {
            format!("sleep {seconds}")
        }
    }

    /// Poll until `ready` or the deadline. A cold PowerShell start can take
    /// several seconds on a loaded machine, so the limit is generous.
    fn until(mut ready: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            if ready() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        false
    }

    /// Whether the process with this id is still running.
    #[cfg(windows)]
    fn alive(pid: u32) -> bool {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // SAFETY: the handle is checked before it is used and closed after.
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                return false;
            }
            let mut code = 0;
            let read = GetExitCodeProcess(process, &mut code);
            CloseHandle(process);
            read != 0 && code == STILL_ACTIVE as u32
        }
    }

    /// Whether the process with this id is still running.
    #[cfg(unix)]
    fn alive(pid: u32) -> bool {
        // SAFETY: signal 0 only asks whether the process is there.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    /// Start a job whose shell prints "grandchild <pid>" of a process it
    /// started, and return the job and that process once it is running.
    fn start_with_grandchild(command: &str) -> (u64, u32) {
        let id = start(command, &std::env::temp_dir()).unwrap();
        let grandchild = || {
            let said = log(id).unwrap();
            let (pid, _) = said.split("grandchild ").nth(1)?.split_once('\n')?;
            pid.trim().parse::<u32>().ok()
        };
        assert!(
            until(|| grandchild().is_some()),
            "job never said: {:?}",
            log(id).unwrap()
        );
        let pid = grandchild().unwrap();
        assert!(alive(pid), "process {pid} was not running");
        (id, pid)
    }

    #[test]
    fn stopping_a_job_ends_its_child_processes() {
        // The shell starts a second process, says which, and waits.
        let (id, pid) = start_with_grandchild(if cfg!(windows) {
            "$p = Start-Process powershell '-NoProfile -Command Start-Sleep 60' \
             -NoNewWindow -PassThru; Write-Output \"grandchild $($p.Id)\"; Start-Sleep 60"
        } else {
            "sleep 60 & echo \"grandchild $!\"; wait"
        });

        stop(id).unwrap();
        assert!(
            until(|| !running(id).unwrap()),
            "job kept running after stop"
        );
        assert!(until(|| !alive(pid)), "process {pid} outlived its job");
    }

    #[cfg(windows)]
    #[test]
    fn stopping_a_job_whose_shell_has_exited_ends_what_it_left_running() {
        // The shell starts a second process, says which, and exits at once.
        let (id, pid) = start_with_grandchild(
            "$p = Start-Process powershell '-NoProfile -Command Start-Sleep 60' \
             -NoNewWindow -PassThru; Write-Output \"grandchild $($p.Id)\"",
        );
        assert!(until(|| !running(id).unwrap()), "shell never exited");
        assert!(alive(pid), "process {pid} ended with its shell");

        stop(id).unwrap();
        assert!(until(|| !alive(pid)), "process {pid} outlived its job");
    }

    #[test]
    fn a_job_that_exits_is_reaped_without_polling() {
        let id = start("exit 3", &std::env::temp_dir()).unwrap();
        assert!(until(|| !running(id).unwrap()), "job never finished");
        assert_eq!(exit_code(id).unwrap(), Some(3));
        // Stopping it afterwards changes nothing.
        stop(id).unwrap();
        assert_eq!(exit_code(id).unwrap(), Some(3));
    }

    #[test]
    fn runs_collects_and_stops() {
        let dir = std::env::temp_dir();
        let echo = if cfg!(windows) {
            format!("Write-Output ready; {}", sleep_command(30))
        } else {
            format!("echo ready; {}", sleep_command(30))
        };
        let id = start(&echo, &dir).unwrap();
        assert!(
            until(|| log(id).unwrap().contains("ready")),
            "job never printed: {:?}",
            log(id).unwrap()
        );
        assert!(running(id).unwrap());
        assert!(list().iter().any(|j| j.id == id && j.running));

        stop(id).unwrap();
        assert!(
            until(|| !running(id).unwrap()),
            "job kept running after stop"
        );
        assert!(log(999).is_err());
    }

    #[test]
    fn a_jobs_log_comes_without_escape_sequences() {
        let print = if cfg!(windows) {
            r#"Write-Output "$([char]27)[32mdone$([char]27)[0m""#
        } else {
            r#"printf '\033[32mdone\033[0m\n'"#
        };
        let id = start(print, &std::env::temp_dir()).unwrap();
        assert!(
            until(|| log(id).unwrap().contains("done")),
            "job never printed: {:?}",
            log(id).unwrap()
        );
        assert_eq!(log(id).unwrap().trim_end(), "done");
    }

    #[test]
    fn a_full_log_keeps_the_newest_whole_characters() {
        // Three-byte characters in pump-sized reads: reads split characters,
        // and the byte limit lands inside one.
        let output = "€".repeat(30_000) + "newest";
        let mut log = VecDeque::new();
        for read in output.as_bytes().chunks(4096) {
            append(&mut log, read);
        }
        assert_eq!(log.len(), MAX_LOG);
        let kept = (MAX_LOG - "newest".len()) / "€".len();
        assert_eq!(text(&mut log), "€".repeat(kept) + "newest");
    }

    #[test]
    fn output_that_was_never_text_reads_back_within_the_limit() {
        let mut log = VecDeque::new();
        append(&mut log, &[0xFF; MAX_LOG]);
        let read = text(&mut log);
        assert!(read.len() <= MAX_LOG, "{} bytes", read.len());
        assert!(read.chars().all(|c| c == char::REPLACEMENT_CHARACTER));
    }

    fn say_done() -> &'static str {
        if cfg!(windows) {
            "Write-Output done"
        } else {
            "echo done"
        }
    }

    /// The notices job `id` has left `owner`, once there are any.
    fn heard(owner: u64, id: u64) -> Vec<Notice> {
        let from = format!("job {id}");
        let mut heard = Vec::new();
        until(|| {
            let taken = crate::inbox::take(owner, true);
            heard.extend(taken.into_iter().filter(|notice| notice.from == from));
            !heard.is_empty()
        });
        heard
    }

    #[test]
    fn a_job_that_exits_tells_its_owner_once() {
        let _turn = crate::inbox::tests::DOORBELL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let id = start_for(say_done(), &std::env::temp_dir(), 7_101).unwrap();
        let notices = heard(7_101, id);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert_eq!(notices[0].when, When::Next);
        assert_eq!(
            notices[0].text,
            format!("job #{id} exited with code 0 (`{}`):\ndone", say_done())
        );

        // Neither a stop after the exit nor a second finish tells it again.
        stop(id).unwrap();
        let again = Notice {
            from: format!("job {id}"),
            when: When::Next,
            text: "again".into(),
        };
        crate::tasks::finish(&Key::Job(id), State::Failed("again".into()), Some(again));
        assert!(crate::inbox::take(7_101, true).is_empty());
    }

    #[test]
    fn a_stopped_job_sends_no_notice() {
        let _turn = crate::inbox::tests::DOORBELL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let id = start_for(&sleep_command(30), &std::env::temp_dir(), 7_102).unwrap();
        stop(id).unwrap();
        assert!(
            until(|| !running(id).unwrap()),
            "job kept running after stop"
        );
        // The reaper reports just after the exit, so give it that long.
        std::thread::sleep(std::time::Duration::from_secs(2));
        assert!(crate::inbox::take(7_102, true).is_empty());
        let task = crate::tasks::list(7_102)
            .into_iter()
            .find(|task| task.key == Key::Job(id))
            .unwrap();
        assert_eq!(task.state, State::Stopped);
    }

    #[test]
    fn the_notice_carries_the_last_lines() {
        let _turn = crate::inbox::tests::DOORBELL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let count = if cfg!(windows) {
            "1..30 | ForEach-Object { \"line $_\" }; exit 1"
        } else {
            "i=1; while [ $i -le 30 ]; do echo line $i; i=$((i+1)); done; exit 1"
        };
        let id = start_for(count, &std::env::temp_dir(), 7_103).unwrap();
        let notices = heard(7_103, id);
        let last: Vec<String> = (11..=30).map(|i| format!("line {i}")).collect();
        assert_eq!(
            notices[0].text,
            format!(
                "job #{id} exited with code 1 (`{count}`):\n{}",
                last.join("\n")
            )
        );
    }

    #[test]
    fn a_job_with_no_owner_stays_quiet() {
        let _turn = crate::inbox::tests::DOORBELL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let id = start(say_done(), &std::env::temp_dir()).unwrap();
        let ended = || {
            crate::tasks::list(0)
                .iter()
                .any(|task| task.key == Key::Job(id) && task.state != State::Running)
        };
        assert!(until(ended), "the exit was never recorded");
        let from = format!("job {id}");
        let taken = crate::inbox::take(0, true);
        assert!(taken.iter().all(|notice| notice.from != from), "{taken:?}");
    }

    #[test]
    fn a_process_left_behind_does_not_hold_the_notice_back() {
        let _turn = crate::inbox::tests::DOORBELL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // The leftover keeps the output pipe open for twice as long as heard
        // waits.
        let leave = if cfg!(windows) {
            "Start-Process -NoNewWindow powershell -ArgumentList '-NoProfile','-Command','Start-Sleep 60'; Write-Output done"
        } else {
            "sleep 60 & echo done"
        };
        let id = start_for(leave, &std::env::temp_dir(), 7_104).unwrap();
        let notices = heard(7_104, id);
        assert_eq!(notices.len(), 1, "no notice while the leftover ran");
        assert!(
            notices[0].text.ends_with(":\ndone"),
            "{:?}",
            notices[0].text
        );
    }

    fn needles(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|text| text.to_string()).collect()
    }

    /// Job #7's watch for `texts`, past the first output run_command shows.
    fn watching(command: &str, texts: &[&str]) -> Watch {
        let mut watch = Watch::new(7, command, &needles(texts));
        watch.shown = true;
        watch
    }

    #[test]
    fn a_matching_line_sends_one_notice() {
        let _turn = crate::inbox::tests::DOORBELL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // The job waits at the gate after its first line, so that line is in
        // the first output before the rest comes; the wait gives up after 30 s.
        let dir = std::env::temp_dir();
        let gate = dir.join(format!("letronna-watch-gate-{}", std::process::id()));
        let _ = std::fs::remove_file(&gate);
        let say = if cfg!(windows) {
            format!(
                "Write-Output 'ready early'; \
                 for ($i = 0; $i -lt 600 -and -not (Test-Path '{}'); $i++) {{ Start-Sleep -Milliseconds 50 }}; \
                 Write-Output starting; Write-Output 'ready on 3000'; Write-Output serving",
                gate.display()
            )
        } else {
            format!(
                "echo 'ready early'; \
                 i=0; while [ ! -e '{}' ] && [ $i -lt 600 ]; do sleep 0.05; i=$((i+1)); done; \
                 echo starting; echo 'ready on 3000'; echo serving",
                gate.display()
            )
        };
        let print = format!("{say}; {}", sleep_command(30));
        let id = start_watching(&print, &dir, 7_105, &needles(&["ready"])).unwrap();
        let early_ended = || {
            log(id)
                .unwrap()
                .split_once("ready early")
                .is_some_and(|(_, after)| after.contains('\n'))
        };
        assert!(until(early_ended), "job never printed: {:?}", log(id));
        // The first output carries the early line, so no notice does.
        assert!(first_output(id).unwrap().contains("ready early"));
        std::fs::write(&gate, "").unwrap();
        let mut notices = heard(7_105, id);
        // Well past the batch's wait, so a line sent twice would be here by now.
        std::thread::sleep(Duration::from_secs(1));
        stop(id).unwrap();
        let _ = std::fs::remove_file(&gate);
        let from = format!("job {id}");
        let taken = crate::inbox::take(7_105, true);
        notices.extend(taken.into_iter().filter(|notice| notice.from == from));
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert_eq!(
            notices[0].text,
            format!("job #{id} printed 1 matching line (`{print}`):\nready on 3000")
        );
    }

    #[test]
    fn lines_within_200ms_are_batched() {
        let mut watch = watching("npm test", &["FAIL"]);
        let start = Instant::now();
        assert_eq!(watch.saw("PASS src/a.test.js", start), None);
        assert_eq!(
            watch.saw("FAIL src/b.test.js", start),
            Some(start + BATCH_WAIT)
        );
        let soon = start + Duration::from_millis(150);
        assert_eq!(watch.saw("FAIL src/c.test.js", soon), None);
        assert_eq!(
            watch.flush(start + BATCH_WAIT).as_deref(),
            Some(
                "job #7 printed 2 matching lines (`npm test`):\nFAIL src/b.test.js\nFAIL src/c.test.js"
            )
        );

        // A burst bigger than one notice shows what fits and counts the rest.
        let later = start + QUIET * 2;
        let long = format!("FAIL {}", "x".repeat(395));
        for _ in 0..20 {
            watch.saw(&long, later);
        }
        let text = watch.flush(later + BATCH_WAIT).unwrap();
        let (first, shown) = text.split_once('\n').unwrap();
        assert_eq!(
            first,
            "job #7 printed 20 matching lines (`npm test`); read_job has the 13 not shown:"
        );
        assert_eq!(shown.lines().count(), 7);
        assert!(shown.chars().count() <= MAX_BATCH);
    }

    #[test]
    fn matches_in_the_quiet_period_are_counted_not_sent() {
        let mut watch = watching("npm run dev", &["error"]);
        let start = Instant::now();
        let first = watch.saw("error: cannot find module", start).unwrap();
        assert!(watch.flush(first).is_some());
        // The next flush waits out the quiet period, not the batch's wait.
        let soon = start + Duration::from_secs(1);
        assert_eq!(watch.saw("error: one", soon), Some(first + QUIET));
        assert_eq!(watch.saw("error: two", soon), None);
        assert_eq!(
            watch.flush(first + QUIET).as_deref(),
            Some("job #7 printed 2 matching lines (`npm run dev`); read_job has the 2 not shown.")
        );

        // What the next quiet period holds goes out with the exit, the next notice.
        let later = first + QUIET + Duration::from_secs(1);
        assert!(watch.saw("error: three", later).is_some());
        assert_eq!(
            watch.end().as_deref(),
            Some("job #7 printed 1 matching line (`npm run dev`); read_job has the 1 not shown.")
        );
        assert_eq!(watch.saw("error: four", later + QUIET), None);
        assert_eq!(watch.end(), None);
    }

    /// Show `watch` a matching line every `every`, `lines` times, flushing as
    /// its timers would, and return what it sent.
    fn drive(watch: &mut Watch, every: Duration, lines: u32) -> Vec<String> {
        let mut now = Instant::now();
        let mut due = None;
        let mut sent = Vec::new();
        for _ in 0..lines {
            if let Some(when) = due.filter(|when| *when <= now) {
                due = None;
                sent.extend(watch.flush(when));
            }
            if let Some(when) = watch.saw("error", now) {
                due = Some(when);
            }
            now += every;
        }
        sent.extend(due.and_then(|when| watch.flush(when)));
        sent
    }

    #[test]
    fn a_watch_that_keeps_firing_switches_off_and_says_so() {
        let stopped = "Stopped watching job #7: it matched too often. Use read_job.";
        // A match every second holds lines back in three quiet periods in a row.
        let mut watch = watching("npm run dev", &["error"]);
        let sent = drive(&mut watch, Duration::from_secs(1), 120);
        assert_eq!(sent.len(), 4, "{sent:?}");
        assert_eq!(
            sent[0],
            "job #7 printed 1 matching line (`npm run dev`):\nerror"
        );
        assert_eq!(
            sent[1],
            "job #7 printed 15 matching lines (`npm run dev`); read_job has the 15 not shown."
        );
        assert_eq!(sent[3], stopped);

        // A match every 20 s is never held back, but eight notices is the most.
        let mut watch = watching("npm run dev", &["error"]);
        let sent = drive(&mut watch, Duration::from_secs(20), 20);
        assert_eq!(sent.len(), 9, "{sent:?}");
        assert!(sent[..8].iter().all(|text| text == &sent[0]), "{sent:?}");
        assert_eq!(sent[8], stopped);
    }

    #[test]
    fn a_line_split_across_reads_still_matches() {
        let long = format!("ready {}", "x".repeat(5_000));
        // Cut inside a word, inside a character, and through a line too long to keep.
        let reads: [&[u8]; 6] = [
            b"compiling\nserver re",
            b"ady on :3000\r\ncaf\xC3",
            b"\xA9 ready\n",
            &long.as_bytes()[..3_000],
            &long.as_bytes()[3_000..],
            b"\nlast ready",
        ];
        let mut partial = Partial::default();
        let mut seen = Vec::new();
        for read in reads {
            partial.feed(read, |line| seen.push(line.to_owned()));
        }
        seen.extend(partial.rest());
        assert_eq!(
            seen,
            [
                "compiling",
                "server ready on :3000",
                "café ready",
                &long[..MAX_LINE],
                "last ready"
            ]
        );

        let mut watch = watching("npm run dev", &["ready"]);
        let now = Instant::now();
        for line in &seen {
            watch.saw(line, now);
        }
        assert_eq!(
            watch.flush(now + BATCH_WAIT),
            Some(format!(
                "job #7 printed 4 matching lines (`npm run dev`):\n{}",
                seen[1..].join("\n")
            ))
        );
    }
}
