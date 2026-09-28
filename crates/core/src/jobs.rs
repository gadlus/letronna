//! Background processes started by the agent.
//!
//! A foreground command has to finish inside the timeout, which rules out the
//! ones worth leaving up: dev servers, watchers, tunnels. Those run here
//! instead. The process keeps running, its output collects in a buffer the
//! agent can read back, and the chat shows a chip until it is stopped.

use std::collections::{BTreeMap, VecDeque};
use std::io::Read;
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Output kept per job. Enough to see what a server is doing without letting a
/// chatty process grow without bound.
const MAX_LOG: usize = 64_000;

pub struct Job {
    pub id: u64,
    pub command: String,
    log: Arc<Mutex<VecDeque<u8>>>,
    child: Arc<Mutex<Child>>,
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
fn pump(mut source: impl Read + Send + 'static, log: Arc<Mutex<VecDeque<u8>>>) {
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
    });
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

pub fn start(command: &str, cwd: &Path) -> Result<u64, String> {
    let mut child = crate::tools::shell(command, Some(cwd))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Cannot start the shell: {e}"))?;

    let log = Arc::new(Mutex::new(VecDeque::new()));
    pump(child.stdout.take().unwrap(), log.clone());
    pump(child.stderr.take().unwrap(), log.clone());

    let id = next_id();
    let child = Arc::new(Mutex::new(child));
    let finished = Arc::new(AtomicBool::new(false));
    let exit_code = Arc::new(Mutex::new(None));

    // Reap the process so `running` turns false on its own.
    {
        let (child, finished, exit_code) = (child.clone(), finished.clone(), exit_code.clone());
        std::thread::spawn(move || {
            loop {
                let status = child
                    .lock()
                    .ok()
                    .and_then(|mut c| c.try_wait().ok().flatten());
                if let Some(status) = status {
                    if let Ok(mut code) = exit_code.lock() {
                        *code = status.code();
                    }
                    finished.store(true, Ordering::Relaxed);
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(120));
            }
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
                child,
                finished,
                exit_code,
            },
        );
    Ok(id)
}

fn with_job<T>(id: u64, f: impl FnOnce(&Job) -> T) -> Result<T, String> {
    let jobs = registry().lock().map_err(|_| "Job list is poisoned.")?;
    let job = jobs.get(&id).ok_or_else(|| format!("No job #{id}."))?;
    Ok(f(job))
}

/// Everything the job has printed so far.
pub fn log(id: u64) -> Result<String, String> {
    with_job(id, |job| {
        job.log.lock().map(|mut l| text(&mut l)).unwrap_or_default()
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
        if let Ok(mut child) = job.child.lock() {
            let _ = child.kill();
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
        if let Ok(mut child) = job.child.lock() {
            let _ = child.kill();
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
}
