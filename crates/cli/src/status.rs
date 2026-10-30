//! The status line: a command of the user's whose first line of output is
//! shown under the footer. It is given the session as JSON on its input, in
//! the shape Claude Code's statusLine reads, so a script written for that
//! works here too.
//!
//! It runs on a thread of its own, never where the box is drawn, so a slow
//! script never holds up typing: what it printed last stays up until it
//! prints again. It runs again when what it would be given changes, no more
//! often than `EVERY`, and one still going after `LIMIT` is stopped, with
//! that said in its place.

use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The least time between two runs: a turn changes what the line is given
/// at every step, and a script started for each would never be idle.
const EVERY: Duration = Duration::from_millis(300);

/// How long one run may take. A status line that needs longer is broken
/// rather than slow, and says so where its line would be.
const LIMIT: Duration = Duration::from_secs(5);

/// The most of its first line that is kept, so a script printing without
/// end cannot fill memory before the line is cut to the window anyway.
const MOST: u64 = 4096;

/// What a run is given: the command, its input, and where it runs.
#[derive(Clone, PartialEq)]
struct Asked {
    command: String,
    input: String,
    folder: Option<PathBuf>,
}

#[derive(Default)]
struct State {
    /// The newest run asked for and not yet started.
    asked: Option<Asked>,
    /// What the last run was given, so the same is not run again for nothing;
    /// none once the line is taken away.
    given: Option<Asked>,
    /// The line to show.
    shown: Option<String>,
    /// Whether `shown` changed since the bar last took it.
    fresh: bool,
    /// Whether the thread that runs it has been started.
    running: bool,
}

/// One terminal's status line, and the thread that runs its command.
#[derive(Default)]
pub struct StatusLine {
    shared: Arc<(Mutex<State>, Condvar)>,
}

impl StatusLine {
    /// Run `command` with `input` on its stdin in `folder` soon, unless that
    /// is just what it was last given; an empty command takes the line away.
    pub fn ask(&self, command: &str, input: String, folder: Option<&Path>) {
        let mut state = lock(&self.shared);
        if command.trim().is_empty() {
            state.asked = None;
            state.given = None;
            if state.shown.take().is_some() {
                state.fresh = true;
            }
            return;
        }
        let asked = Asked {
            command: command.to_owned(),
            input,
            folder: folder.map(Path::to_path_buf),
        };
        if state.given.as_ref() == Some(&asked) || state.asked.as_ref() == Some(&asked) {
            return;
        }
        state.asked = Some(asked);
        if !state.running {
            state.running = true;
            let shared = self.shared.clone();
            std::thread::spawn(move || keep_running(&shared));
        }
        self.shared.1.notify_one();
    }

    /// The line to show, when it changed since this was last asked: `None`
    /// inside for no line.
    pub fn take_new(&self) -> Option<Option<String>> {
        let mut state = lock(&self.shared);
        std::mem::take(&mut state.fresh).then(|| state.shown.clone())
    }

    /// The line shown now.
    pub fn shown(&self) -> Option<String> {
        lock(&self.shared).shown.clone()
    }
}

fn lock(shared: &(Mutex<State>, Condvar)) -> MutexGuard<'_, State> {
    shared.0.lock().unwrap_or_else(|e| e.into_inner())
}

/// Run each newest ask in turn, at most once every `EVERY`.
fn keep_running(shared: &(Mutex<State>, Condvar)) {
    let mut last: Option<Instant> = None;
    loop {
        {
            let mut state = lock(shared);
            while state.asked.is_none() {
                state = shared.1.wait(state).unwrap_or_else(|e| e.into_inner());
            }
        }
        // Asks that come meanwhile replace this one, so only the newest runs.
        if let Some(at) = last {
            std::thread::sleep(EVERY.saturating_sub(at.elapsed()));
        }
        let asked = {
            let mut state = lock(shared);
            let Some(asked) = state.asked.take() else {
                continue;
            };
            state.given = Some(asked.clone());
            asked
        };
        last = Some(Instant::now());
        let line = run(&asked, LIMIT);
        let mut state = lock(shared);
        // Taken away or changed while it ran: what it printed is for nobody.
        if state.given.as_ref() != Some(&asked) {
            continue;
        }
        if state.shown != line {
            state.shown = line;
            state.fresh = true;
        }
    }
}

/// The first line `asked` prints, made safe to draw, or what went wrong in
/// its place; none when it prints nothing.
fn run(asked: &Asked, limit: Duration) -> Option<String> {
    let mut command = letronna_core::tools::shell(&asked.command, asked.folder.as_deref());
    // Its own group, so one that runs too long is stopped with what it started.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = match command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => return Some(format!("status line could not start: {error}")),
    };
    // On threads of their own: a script that never reads its input, or
    // never ends its line, would otherwise hold this up past the limit.
    if let Some(mut stdin) = child.stdin.take() {
        let input = asked.input.clone();
        std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
    }
    let (sender, first) = std::sync::mpsc::channel();
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = std::io::BufReader::new(stdout.take(MOST)).read_line(&mut line);
            let _ = sender.send(line);
        });
    }
    let started = Instant::now();
    let line = loop {
        if !matches!(child.try_wait(), Ok(None)) {
            break first.recv_timeout(Duration::from_millis(200)).ok();
        }
        if started.elapsed() >= limit {
            stop(&mut child);
            // One that printed its line and went on is shown all the same.
            match first.recv_timeout(Duration::from_millis(200)) {
                Ok(line) if !line.trim().is_empty() => break Some(line),
                _ => {
                    return Some(format!(
                        "status line still running after {}s, so it was stopped",
                        limit.as_secs_f64().ceil()
                    ));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let line = crate::render::safe(line?.trim_end_matches(['\n', '\r']));
    (!line.trim().is_empty()).then_some(line)
}

/// End `child` and everything it started.
fn stop(child: &mut std::process::Child) {
    #[cfg(unix)]
    if let Ok(group) = libc::pid_t::try_from(child.id()) {
        // SAFETY: signals the child's own process group, made at its start.
        unsafe { libc::killpg(group, libc::SIGKILL) };
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked(command: &str, input: &str) -> Asked {
        Asked {
            command: command.to_owned(),
            input: input.to_owned(),
            folder: None,
        }
    }

    // The commands are sh; on Windows the shell is PowerShell.
    #[cfg(unix)]
    #[test]
    fn the_status_line_is_the_first_line_its_command_prints() {
        let line = run(&asked("echo first; echo second", ""), LIMIT);
        assert_eq!(line.as_deref(), Some("first"));
        assert_eq!(run(&asked("true", ""), LIMIT), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_command_is_given_the_session_on_its_input() {
        let line = run(
            &asked("head -c 15", r#"{"session_id":"7","model":{}}"#),
            LIMIT,
        );
        assert_eq!(line.as_deref(), Some(r#"{"session_id":""#));
    }

    #[cfg(unix)]
    #[test]
    fn a_status_line_that_runs_too_long_is_stopped_and_says_so() {
        let started = Instant::now();
        let line = run(&asked("sleep 30", ""), Duration::from_millis(300)).unwrap();
        assert!(line.contains("still running"), "{line}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    fn a_status_line_that_prints_and_lingers_is_shown_and_stopped() {
        let line = run(&asked("echo up; sleep 30", ""), Duration::from_millis(300));
        assert_eq!(line.as_deref(), Some("up"));
    }

    #[cfg(unix)]
    #[test]
    fn what_a_status_line_prints_keeps_its_colours_and_nothing_else() {
        let line = run(
            &asked(r"printf '\033[31mred\033[0m\033]0;title\007 done'", ""),
            LIMIT,
        );
        assert_eq!(line.as_deref(), Some("\x1b[31mred\x1b[0m done"));
    }

    #[cfg(unix)]
    #[test]
    fn the_line_shows_what_was_asked_last_and_goes_when_it_is_taken_away() {
        let status = StatusLine::default();
        status.ask("echo one", "{}".into(), None);
        let waited = Instant::now();
        let mut seen = None;
        while waited.elapsed() < Duration::from_secs(5) {
            if let Some(line) = status.take_new() {
                seen = line;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(seen.as_deref(), Some("one"));
        // The same again is not run again, so nothing new comes of it.
        status.ask("echo one", "{}".into(), None);
        std::thread::sleep(EVERY * 2);
        assert_eq!(status.take_new(), None);
        status.ask("", String::new(), None);
        assert_eq!(status.take_new(), Some(None));
        assert_eq!(status.shown(), None);
    }
}
