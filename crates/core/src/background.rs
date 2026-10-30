//! Sessions that work on with no terminal open, as `letronna --bg` starts
//! them, and what another terminal reads of them and says to them.
//!
//! A background run is a `-p` run in a process of its own, cut loose from
//! the terminal that started it, in its session's mode. Beside the session
//! it keeps a few small files in `background/`, named for the session:
//! `<id>.json`, what it is doing and what it waits on, which only it writes;
//! `<id>.lock`, which it holds for as long as it runs, so a run that crashed
//! or was killed reads as gone rather than working, and a second run of the
//! same session is refused; `<id>.answer`, which `attach` writes while a call
//! waits for a yes; and `<id>.stop`, which `stop` leaves.
//!
//! A call that needs a yes waits here, as a card waits in the window, and
//! the run asks nobody else: every way of not answering is a no, and an
//! answer counts only for the call it names, so one written for an earlier
//! card can never let a later one through. The run and the terminal that
//! answers are two processes with nothing in memory to share, so the run
//! looks for an answer every `ANSWER_GAP` while it waits, and for a stop
//! every `STOP_GAP` while it works: each sooner than a person notices, and
//! no more often than that.

use crate::approvals::{Decision, Request};
use crate::cancel::Cancel;
use crate::chat::ToolCall;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

/// How often a run waiting for a yes looks for the answer.
const ANSWER_GAP: Duration = Duration::from_millis(250);

/// How often a working run looks for a stop: once a second, since stopping
/// is never so urgent that a second matters.
const STOP_GAP: Duration = Duration::from_secs(1);

/// An ended run is listed for a day, long enough to come back to it after a
/// night, and its files are then removed.
const KEPT: Duration = Duration::from_secs(24 * 60 * 60);

/// How much of the run's answer its record keeps: a line or two, for the
/// list; the whole answer is in the session.
const SAID_CHARS: usize = 300;

/// Where a run stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    #[default]
    Working,
    /// A call waits for the user's yes.
    Waiting,
    Done,
    Failed,
    Stopped,
    /// Its process ended without saying how, as a crash or a kill does.
    Gone,
}

impl State {
    /// Whether the run is still going.
    pub fn running(self) -> bool {
        matches!(self, State::Working | State::Waiting)
    }

    /// The state as the list says it.
    pub fn name(self) -> &'static str {
        match self {
            State::Working => "working",
            State::Waiting => "waiting for you",
            State::Done => "done",
            State::Failed => "failed",
            State::Stopped => "stopped",
            State::Gone => "ended unexpectedly",
        }
    }
}

/// A call waiting for a yes, as its card shows it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Asking {
    pub call: ToolCall,
    /// What the card shows, from the tool's own preview.
    #[serde(default)]
    pub preview: String,
    /// Why the gate put it to the user, when it said; empty when it asks as
    /// it always does.
    #[serde(default)]
    pub why: String,
    /// The rule a yes for the rest of the run covers, when one would.
    #[serde(default)]
    pub for_run: Option<String>,
    /// Where a memory save may be kept, as its card offers them, in order.
    #[serde(default)]
    pub places: Vec<String>,
}

/// What a run is and where it stands, as `<id>.json` holds it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub session: u64,
    /// Whoever started the run finds its record by this.
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub pid: u32,
    /// The first line of what it was asked.
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub folder: Option<PathBuf>,
    /// The mode it runs in, by the name `--mode` takes.
    #[serde(default)]
    pub mode: String,
    /// Seconds since 1970, as the sessions keep time.
    #[serde(default)]
    pub started: u64,
    #[serde(default)]
    pub state: State,
    /// What it is on now, in a few words.
    #[serde(default)]
    pub doing: String,
    #[serde(default)]
    pub asking: Option<Asking>,
    #[serde(default)]
    pub ended: Option<u64>,
    /// The start of its answer, or why it failed.
    #[serde(default)]
    pub said: String,
}

/// What the user said to a call that waits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "kebab-case")]
pub enum Answer {
    Once,
    /// Yes, and to the rest of the run's calls the same rule covers.
    ForRun,
    /// Yes to a memory save, kept in the card's place at this index.
    Keep {
        place: usize,
    },
    /// No, for the reason given, empty when there was none.
    Deny {
        why: String,
    },
    /// No, and stop the run.
    Stop,
}

/// An answer as `<id>.answer` holds it, with the call it is for.
#[derive(Serialize, Deserialize)]
struct Answered {
    call: String,
    #[serde(flatten)]
    answer: Answer,
}

/// The run of one session in this process, which keeps its record and holds
/// its lock until dropped.
pub struct Run {
    dir: PathBuf,
    record: Mutex<Record>,
    _lock: std::fs::File,
}

impl Run {
    /// Take on session `record.session` as this process's background run,
    /// or say why not: another run of it is going.
    pub fn claim(record: Record) -> Result<Arc<Run>, String> {
        Run::claim_in(&dir(), record).map(Arc::new)
    }

    fn claim_in(dir: &Path, mut record: Record) -> Result<Run, String> {
        let id = record.session;
        std::fs::create_dir_all(dir).map_err(|e| format!("Cannot make {}: {e}", dir.display()))?;
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path(dir, id, "lock"))
            .map_err(|e| format!("Cannot start session {id} in the background: {e}"))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(format!(
                    "Session {id} is already working in the background: letronna attach {id} to watch it."
                ));
            }
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(format!("Cannot start session {id} in the background: {e}"));
            }
        }
        // What an earlier run of the session left is not for this one.
        for leftover in ["answer", "stop"] {
            let _ = std::fs::remove_file(path(dir, id, leftover));
        }
        record.pid = std::process::id();
        record.started = crate::chat::now();
        record.state = State::Working;
        record.asking = None;
        record.ended = None;
        let run = Run {
            dir: dir.to_owned(),
            record: Mutex::new(record),
            _lock: lock,
        };
        run.save();
        Ok(run)
    }

    pub fn session(&self) -> u64 {
        self.lock().session
    }

    /// Say that the run is on `what` now, when that is news.
    pub fn doing(&self, what: &str) {
        if self.lock().doing != what {
            self.change(|record| record.doing = what.to_owned());
        }
    }

    /// Wait for the user to answer `asking`, or for a stop, and give what
    /// they said. A stop asked for, or `cancel` set, while it waits is a
    /// `Stop`.
    pub fn wait(&self, asking: Asking, cancel: &Cancel) -> Answer {
        let call = asking.call.id.clone();
        self.change(|record| {
            record.state = State::Waiting;
            record.asking = Some(asking);
        });
        let answer = loop {
            if let Some(answer) = self.answered(&call) {
                break answer;
            }
            if self.stop_asked() || !cancel.sleep(ANSWER_GAP) {
                break Answer::Stop;
            }
        };
        self.change(|record| {
            record.state = State::Working;
            record.asking = None;
        });
        answer
    }

    /// Set `cancel` once a stop is asked for, looking every `STOP_GAP` until
    /// then, for as long as the run is held.
    pub fn watch(self: &Arc<Self>, cancel: Cancel) {
        let run: Weak<Run> = Arc::downgrade(self);
        std::thread::spawn(move || {
            while cancel.sleep(STOP_GAP) {
                let Some(run) = run.upgrade() else {
                    return;
                };
                if run.stop_asked() {
                    cancel.set();
                    return;
                }
            }
        });
    }

    /// Answer each call `requests` brings by waiting for the user, telling
    /// `told` first, until the requests end. A stop answered stops the run
    /// through `cancel`.
    pub fn serve(
        self: &Arc<Self>,
        requests: std::sync::mpsc::Receiver<Request>,
        cancel: Cancel,
        told: impl Fn(&ToolCall) + Send + 'static,
    ) {
        let run = self.clone();
        std::thread::spawn(move || {
            for request in requests {
                told(&request.call);
                let asking = Asking {
                    call: request.call.clone(),
                    preview: request.preview.clone(),
                    why: request.why.clone(),
                    for_run: request.for_run().map(str::to_owned),
                    places: (request.places.iter())
                        .map(|place| place.save_label().to_owned())
                        .collect(),
                };
                let answer = run.wait(asking, &cancel);
                if answer == Answer::Stop {
                    cancel.set();
                }
                let decision = decided(&request, answer);
                request.answer(decision);
            }
        });
    }

    /// Say how the run ended, with the start of its answer or why it failed.
    pub fn end(&self, state: State, said: &str) {
        let said: String = said.trim().chars().take(SAID_CHARS).collect();
        self.change(|record| {
            record.state = state;
            record.asking = None;
            record.ended = Some(crate::chat::now());
            record.said = said;
        });
        // What it was told is spent with it.
        for spent in ["answer", "stop"] {
            let _ = std::fs::remove_file(path(&self.dir, self.session(), spent));
        }
    }

    /// The answer `attach` left for call `call`, taken, if there is one. One
    /// for another call is left for that call's card, which has gone, and is
    /// cleared by the next answer written.
    fn answered(&self, call: &str) -> Option<Answer> {
        let path = path(&self.dir, self.session(), "answer");
        let answered: Answered = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
        if answered.call != call {
            return None;
        }
        let _ = std::fs::remove_file(&path);
        Some(answered.answer)
    }

    fn stop_asked(&self) -> bool {
        path(&self.dir, self.session(), "stop").exists()
    }

    fn change(&self, change: impl FnOnce(&mut Record)) {
        change(&mut self.lock());
        self.save();
    }

    fn save(&self) {
        let record = self.lock().clone();
        // Only this run writes it, and a record that could not be written is
        // one the list reads as before; the run itself goes on.
        if let Ok(bytes) = serde_json::to_vec_pretty(&record) {
            let _ = put(&path(&self.dir, record.session, "json"), &bytes);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Record> {
        // Nothing that can panic runs under this lock, so a poisoned one is
        // still whole.
        self.record.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The decision `answer` stands for on `request`'s card: a yes for the rest
/// of the run only where the card offers one, and a memory save kept only in
/// a place its card offers.
fn decided(request: &Request, answer: Answer) -> Decision {
    match answer {
        Answer::ForRun if request.for_run().is_some() => Decision::ForRun,
        Answer::Once | Answer::ForRun => match request.places.first() {
            Some(&place) => Decision::Keep(place),
            None => Decision::Once,
        },
        Answer::Keep { place } => match request.places.get(place) {
            Some(&place) => Decision::Keep(place),
            None => Decision::Deny(String::new()),
        },
        Answer::Deny { why } => Decision::Deny(why),
        Answer::Stop => Decision::Deny(String::new()),
    }
}

/// Where the background runs keep their files, in the data folder.
pub fn dir() -> PathBuf {
    crate::chat::data_dir().join("background")
}

fn path(dir: &Path, session: u64, kind: &str) -> PathBuf {
    dir.join(format!("{session}.{kind}"))
}

/// Write `bytes` to `path` whole, so a reader never finds half of it.
fn put(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    crate::store::write_private(&temp, bytes)?;
    std::fs::rename(&temp, path)
}

/// Whether a background run of session `session` is going now.
pub fn running(session: u64) -> bool {
    held(&dir(), session)
}

fn held(dir: &Path, session: u64) -> bool {
    let Ok(lock) = std::fs::OpenOptions::new()
        .write(true)
        .open(path(dir, session, "lock"))
    else {
        return false;
    };
    match lock.try_lock() {
        Ok(()) => {
            let _ = lock.unlock();
            false
        }
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(std::fs::TryLockError::Error(_)) => false,
    }
}

/// The record of session `session`'s background run, the one going or the
/// last one, if there is one.
pub fn get(session: u64) -> Option<Record> {
    read(&dir(), session)
}

fn read(dir: &Path, session: u64) -> Option<Record> {
    let bytes = std::fs::read(path(dir, session, "json")).ok()?;
    let mut record: Record = serde_json::from_slice(&bytes).ok()?;
    if record.state.running() && !held(dir, session) {
        record.state = State::Gone;
        record.asking = None;
    }
    Some(record)
}

/// Every background run going here, and those that ended in the last day,
/// oldest first. Older ended ones are removed as they are found.
pub fn list() -> Vec<Record> {
    list_in(&dir())
}

fn list_in(dir: &Path) -> Vec<Record> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut records: Vec<Record> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let session: u64 = name.to_str()?.strip_suffix(".json")?.parse().ok()?;
            let record = read(dir, session)?;
            if record.state.running() || !stale(&entry.path()) {
                return Some(record);
            }
            for kind in ["json", "lock", "answer", "stop", "watch"] {
                let _ = std::fs::remove_file(path(dir, session, kind));
            }
            None
        })
        .collect();
    records.sort_by_key(|record| (record.started, record.session));
    records
}

/// Whether the file at `path` was last written over `KEPT` ago.
fn stale(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .is_ok_and(|at| at.elapsed().is_ok_and(|age| age > KEPT))
}

/// Answer the call `call` that session `session`'s run waits on.
pub fn answer(session: u64, call: &str, answer: Answer) -> Result<(), String> {
    answer_in(&dir(), session, call, answer)
}

fn answer_in(dir: &Path, session: u64, call: &str, answer: Answer) -> Result<(), String> {
    let waiting = read(dir, session)
        .and_then(|record| record.asking)
        .is_some_and(|asking| asking.call.id == call);
    if !waiting {
        return Err(format!("Session {session} is no longer waiting on that."));
    }
    let answered = Answered {
        call: call.to_owned(),
        answer,
    };
    let bytes = serde_json::to_vec(&answered).map_err(|e| e.to_string())?;
    put(&path(dir, session, "answer"), &bytes)
        .map_err(|e| format!("Cannot answer session {session}: {e}"))
}

/// Ask session `session`'s background run to stop, which it does within
/// `STOP_GAP`, ending what it started.
pub fn stop(session: u64) -> Result<(), String> {
    stop_in(&dir(), session)
}

fn stop_in(dir: &Path, session: u64) -> Result<(), String> {
    if !held(dir, session) {
        return Err(format!(
            "Session {session} is not working in the background."
        ));
    }
    put(&path(dir, session, "stop"), b"").map_err(|e| format!("Cannot stop session {session}: {e}"))
}

/// Held by a terminal watching session `session`'s run, so the run calls
/// nobody back meanwhile: the card is in front of them already.
pub struct Watching {
    _file: std::fs::File,
}

/// Say that this process watches session `session`'s run until the guard
/// is dropped; `None` when another terminal watches it already, which it
/// still may.
pub fn watching(session: u64) -> Option<Watching> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path(&dir(), session, "watch"))
        .ok()?;
    file.try_lock().ok()?;
    Some(Watching { _file: file })
}

/// Whether a terminal watches session `session`'s run.
pub fn watched(session: u64) -> bool {
    let Ok(file) = std::fs::OpenOptions::new()
        .write(true)
        .open(path(&dir(), session, "watch"))
    else {
        return false;
    };
    match file.try_lock() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(std::fs::TryLockError::Error(_)) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::Scratch;

    fn record(session: u64) -> Record {
        Record {
            session,
            token: "t".into(),
            title: "fix the flaky test".into(),
            mode: "default".into(),
            ..Default::default()
        }
    }

    fn asking(id: &str) -> Asking {
        Asking {
            call: ToolCall {
                id: id.into(),
                name: "run_command".into(),
                arguments: r#"{"command":"cargo test"}"#.into(),
            },
            ..Default::default()
        }
    }

    #[test]
    fn a_session_has_one_background_run_at_a_time_and_reads_as_gone_once_it_dies() {
        let dir = Scratch::new("background-one");
        let run = Run::claim_in(&dir, record(7)).unwrap();
        assert!(held(&dir, 7));
        let again = Run::claim_in(&dir, record(7)).err().unwrap();
        assert!(
            again.contains("already working in the background"),
            "{again}"
        );
        assert_eq!(read(&dir, 7).unwrap().state, State::Working);
        // Dropped still working, as a crash or a kill leaves it.
        drop(run);
        assert!(!held(&dir, 7));
        assert_eq!(read(&dir, 7).unwrap().state, State::Gone);
        assert_eq!(list_in(&dir)[0].state, State::Gone);
        // And free for a new run.
        let run = Run::claim_in(&dir, record(7)).unwrap();
        run.end(State::Done, "Fixed it: the test waited on a clock.");
        let done = read(&dir, 7).unwrap();
        assert_eq!(done.state, State::Done);
        assert!(done.ended.is_some());
        assert_eq!(done.said, "Fixed it: the test waited on a clock.");
    }

    #[test]
    fn a_background_run_waiting_for_a_yes_does_nothing_until_it_is_answered() {
        let dir = Scratch::new("background-wait");
        let run = Arc::new(Run::claim_in(&dir, record(8)).unwrap());
        let (said, heard) = std::sync::mpsc::channel();
        let waiting = run.clone();
        let waiter = std::thread::spawn(move || {
            let answer = waiting.wait(asking("call-2"), &Cancel::default());
            let _ = said.send(answer);
        });
        std::thread::sleep(ANSWER_GAP * 3);
        assert!(heard.try_recv().is_err(), "it went on with nobody's yes");
        let shown = read(&dir, 8).unwrap();
        assert_eq!(shown.state, State::Waiting);
        assert_eq!(shown.asking.unwrap().call.id, "call-2");
        // An answer for another card is no answer to this one.
        let stale = answer_in(&dir, 8, "call-1", Answer::Once);
        assert!(stale.is_err());
        std::thread::sleep(ANSWER_GAP * 2);
        assert!(heard.try_recv().is_err());
        answer_in(
            &dir,
            8,
            "call-2",
            Answer::Deny {
                why: "not now".into(),
            },
        )
        .unwrap();
        waiter.join().unwrap();
        assert_eq!(
            heard.recv().unwrap(),
            Answer::Deny {
                why: "not now".into()
            }
        );
        let after = read(&dir, 8).unwrap();
        assert_eq!(after.state, State::Working);
        assert!(after.asking.is_none());
    }

    #[test]
    fn a_stop_ends_a_wait_and_a_run_that_is_working() {
        let dir = Scratch::new("background-stop");
        let run = Arc::new(Run::claim_in(&dir, record(9)).unwrap());
        let cancel = Cancel::default();
        run.watch(cancel.clone());
        let waiting = run.clone();
        let waiter = std::thread::spawn(move || waiting.wait(asking("call-1"), &Cancel::default()));
        stop_in(&dir, 9).unwrap();
        assert_eq!(waiter.join().unwrap(), Answer::Stop);
        let started = std::time::Instant::now();
        while !cancel.is_set() && started.elapsed() < STOP_GAP * 3 {
            std::thread::sleep(ANSWER_GAP);
        }
        assert!(cancel.is_set());
        run.end(State::Stopped, "");
        assert!(!path(&dir, 9, "stop").exists());
        drop(run);
        let refused = stop_in(&dir, 9).unwrap_err();
        assert!(
            refused.contains("not working in the background"),
            "{refused}"
        );
    }

    #[test]
    fn a_new_run_ignores_what_an_earlier_one_was_told() {
        let dir = Scratch::new("background-leftover");
        std::fs::create_dir_all(&*dir).unwrap();
        put(&path(&dir, 3, "stop"), b"").unwrap();
        let left = Answered {
            call: "call-1".into(),
            answer: Answer::Once,
        };
        put(
            &path(&dir, 3, "answer"),
            &serde_json::to_vec(&left).unwrap(),
        )
        .unwrap();
        let run = Run::claim_in(&dir, record(3)).unwrap();
        assert!(!run.stop_asked());
        assert_eq!(run.answered("call-1"), None);
    }

    #[test]
    fn an_answer_reads_back_as_it_was_written() {
        for answer in [
            Answer::Once,
            Answer::ForRun,
            Answer::Keep { place: 1 },
            Answer::Deny {
                why: "use npm".into(),
            },
            Answer::Stop,
        ] {
            let written = serde_json::to_string(&Answered {
                call: "c".into(),
                answer: answer.clone(),
            })
            .unwrap();
            let read: Answered = serde_json::from_str(&written).unwrap();
            assert_eq!(read.answer, answer, "{written}");
        }
    }
}
