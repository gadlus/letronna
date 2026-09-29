//! Background processes started by the agent.
//!
//! A foreground command has to finish inside the timeout, which rules out the
//! ones worth leaving up: dev servers, watchers, tunnels. Those run here
//! instead. The process keeps running, its output collects in a buffer the
//! agent can read back, and the chat shows a chip until it is stopped.

use crate::inbox::{Notice, When};
use crate::tasks::{Key, State, Task};
use std::collections::{BTreeMap, VecDeque};
use std::io::Read;
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

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

pub struct Job {
    pub id: u64,
    pub command: String,
    log: Arc<Mutex<VecDeque<u8>>>,
    tree: Tree,
    /// Set once the process has exited and the reader threads are done.
    finished: Arc<AtomicBool>,
    exit_code: Arc<Mutex<Option<i32>>>,
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

/// Drain a pipe into the shared log until the process closes it.
fn pump(
    mut source: impl Read + Send + 'static,
    log: Arc<Mutex<VecDeque<u8>>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut block = [0u8; 4096];
        loop {
            match source.read(&mut block) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let Ok(mut log) = log.lock() else { break };
                    append(&mut log, &block[..n]);
                }
            }
        }
    })
}

/// Add output to the log, dropping its oldest bytes beyond `MAX_LOG`.
fn append(log: &mut VecDeque<u8>, bytes: &[u8]) {
    log.extend(bytes);
    // Keep the tail: the newest output is what matters.
    let over = log.len().saturating_sub(MAX_LOG);
    log.drain(..over);
}

/// The log as text, starting at its first whole character.
fn text(log: &mut VecDeque<u8>) -> String {
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

    let log = Arc::new(Mutex::new(VecDeque::new()));
    let out = pump(child.stdout.take().unwrap(), log.clone());
    let err = pump(child.stderr.take().unwrap(), log.clone());

    let id = next_id();
    crate::tasks::register(Task::new(Key::Job(id), owner, None, command));
    let finished = Arc::new(AtomicBool::new(false));
    let exit_code = Arc::new(Mutex::new(None));

    // Reap the process so `running` turns false on its own. Stopping goes
    // through the tree, so this can own the child and wait for it.
    {
        let (finished, exit_code) = (finished.clone(), exit_code.clone());
        let (log, command) = (log.clone(), command.to_owned());
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
    with_job(id, |job| {
        job.log
            .lock()
            .map(|mut l| crate::tools::strip_escapes(&text(&mut l)))
            .unwrap_or_default()
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
}
