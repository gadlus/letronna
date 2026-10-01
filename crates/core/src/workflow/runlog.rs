//! A workflow run's log: what happened to it and its agents, in order, with
//! the time and a short reason, as a person reads it back.
//!
//! It is kept beside the journal as `events.jsonl`, so it survives a restart
//! and a resume carries it on, but the journal is what a resume reads; this
//! is only ever shown. A run that makes its thousand calls, each retried,
//! would log thousands of events, so it keeps the newest EVENTS: the file is
//! appended to and, once it holds twice that, rewritten with the newest ones
//! under a marker counting what was dropped. Appending is one write per event
//! and the rewrite is rare, so logging costs a run next to nothing; reading
//! drops the excess a file not yet rewritten holds.

use crate::chat;
use crate::tools::one_line;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Events a run's log keeps: a long run's story, and a bound on its file.
pub(super) const EVENTS: usize = 500;
/// The characters of a reason kept: a failure's first lines, not a stack.
const REASON: usize = 200;
/// The log's file in a run's folder.
pub(super) const FILE: &str = "events.jsonl";

/// One thing that happened to a run or one of its agents.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogEntry {
    /// Seconds since the Unix epoch.
    pub at: u64,
    #[serde(flatten)]
    pub event: Event,
    /// The agent's number among the run's calls, from 1, for an agent's event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<usize>,
    /// The agent's label, empty for the run's own events.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// Why, in a line; empty when there is nothing to say.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
}

/// What happened.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum Event {
    /// The run was started, or picked up again by a resume.
    Launched,
    /// An agent began to run.
    Started,
    /// An agent answered.
    Finished,
    /// An agent ended without an answer: an error, or out of rounds or budget.
    Failed,
    /// An agent was stopped with its run.
    Stopped,
    /// A skip gave the script null for an agent.
    Skipped,
    /// A retry set an agent to run again at the next resume.
    Retried,
    /// The run was held, by a person or by its budget or failures.
    Paused,
    /// A pause was let go.
    Resumed,
    /// The run ended: finished, stopped, failed or paused.
    Ended,
    /// Stands where `count` older events were dropped to keep the log bounded.
    Dropped { count: u64 },
}

/// A run's log, open for appending by whoever changes the run.
pub(super) struct Log {
    path: PathBuf,
    /// Lines the file holds, its marker included.
    lines: usize,
}

impl Log {
    /// The log in run folder `dir`, carried on from any launch before.
    pub(super) fn open(dir: &Path) -> Self {
        let path = dir.join(FILE);
        let bytes = std::fs::read(&path).unwrap_or_default();
        // A kill can tear the last line; the next one starts on its own.
        if bytes.last().is_some_and(|&last| last != b'\n') {
            let _ = append(&path, b"\n");
        }
        let lines = bytes
            .split(|&byte| byte == b'\n')
            .filter(|line| !line.is_empty());
        Self {
            lines: lines.count(),
            path,
        }
    }

    /// Adds `event` now, for the agent `agent` names by number and label, if
    /// any, because of `why`. A log that cannot be written loses the event and
    /// nothing else: it is never what a run depends on.
    pub(super) fn note(&mut self, event: Event, agent: Option<(usize, &str)>, why: &str) {
        let entry = LogEntry {
            at: chat::now(),
            event,
            agent: agent.map(|(number, _)| number),
            // A script names its agents, so a label is as long as it likes;
            // cut, or every event of a run would carry it whole.
            label: agent.map(|(_, label)| reason(label)).unwrap_or_default(),
            why: reason(why),
        };
        let Ok(mut line) = serde_json::to_vec(&entry) else {
            return;
        };
        line.push(b'\n');
        if append(&self.path, &line).is_ok() {
            self.lines += 1;
        }
        if self.lines > EVENTS * 2 {
            self.shorten();
        }
    }

    /// Rewrites the file with its newest EVENTS under a marker for the rest,
    /// through a copy, so a reader never sees half of it.
    fn shorten(&mut self) {
        let kept = read(&self.path);
        let copy = self.path.with_extension("jsonl.new");
        let mut text = Vec::new();
        for entry in &kept {
            let Ok(line) = serde_json::to_vec(entry) else {
                continue;
            };
            text.extend(line);
            text.push(b'\n');
        }
        if std::fs::write(&copy, &text).is_ok() && std::fs::rename(&copy, &self.path).is_ok() {
            self.lines = kept.len();
        } else {
            // Tried again at the next event; the reader bounds it meanwhile.
            let _ = std::fs::remove_file(&copy);
        }
    }
}

/// `why` on one line, cut after REASON characters: the start of an error
/// says what it was. A label is cut the same way.
fn reason(why: &str) -> String {
    let why = one_line(why);
    match why.char_indices().nth(REASON) {
        Some((cut, _)) => format!("{}…", &why[..cut]),
        None => why,
    }
}

/// Appends `bytes` to the file at `path` in one write, making it if need be.
fn append(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(bytes)
}

/// The log at `path`, oldest first: its newest EVENTS, after a `Dropped`
/// marker when there were more; empty when there is no log.
pub(super) fn read(path: &Path) -> Vec<LogEntry> {
    let mut bytes = Vec::new();
    if File::open(path)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .is_err()
    {
        return Vec::new();
    }
    // Bytes, as the journal is read: a torn line is skipped, not the file.
    let mut dropped = 0;
    let mut marked = None;
    let mut entries: Vec<LogEntry> = Vec::new();
    for line in bytes.split(|&byte| byte == b'\n') {
        let Ok(entry) = serde_json::from_slice::<LogEntry>(line) else {
            continue;
        };
        match entry.event {
            Event::Dropped { count } => {
                dropped += count;
                marked.get_or_insert(entry.at);
            }
            _ => entries.push(entry),
        }
    }
    let over = entries.len().saturating_sub(EVENTS);
    dropped += over as u64;
    let kept = entries.split_off(over);
    if dropped == 0 {
        return kept;
    }
    let at = marked.or(kept.first().map(|entry| entry.at)).unwrap_or(0);
    let marker = LogEntry {
        at,
        event: Event::Dropped { count: dropped },
        agent: None,
        label: String::new(),
        why: format!("{dropped} older events were dropped: a run's log keeps its newest {EVENTS}."),
    };
    std::iter::once(marker).chain(kept).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run folder of the test's own, never the data folder.
    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("letronna-runlog-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_long_log_keeps_its_newest_events_in_order_after_a_marker() {
        let dir = folder("cap");
        let mut log = Log::open(&dir);
        let events = EVENTS * 2 + 10;
        for number in 1..=events {
            log.note(Event::Started, Some((number, "helper")), "");
        }
        let lines = std::fs::read_to_string(dir.join(FILE))
            .unwrap()
            .lines()
            .count();
        assert!(lines <= EVENTS * 2 + 1, "the file kept {lines} lines");
        // Carried on by the next launch, as a resume does.
        let mut log = Log::open(&dir);
        log.note(Event::Ended, None, "finished");
        let kept = read(&dir.join(FILE));
        assert_eq!(kept.len(), EVENTS + 1);
        let dropped = (events + 1 - EVENTS) as u64;
        assert_eq!(kept[0].event, Event::Dropped { count: dropped });
        assert_eq!(
            kept[0].why,
            format!("{dropped} older events were dropped: a run's log keeps its newest {EVENTS}.")
        );
        let numbers: Vec<Option<usize>> = kept[1..].iter().map(|entry| entry.agent).collect();
        let newest = (events + 2 - EVENTS..=events).map(Some).chain([None]);
        assert!(numbers.into_iter().eq(newest));
        assert_eq!(kept[EVENTS].why, "finished");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_torn_last_line_is_skipped_and_a_long_reason_cut() {
        let dir = folder("torn");
        let mut log = Log::open(&dir);
        log.note(Event::Failed, Some((1, "a")), &"why\n".repeat(REASON));
        append(&dir.join(FILE), br#"{"at":1,"event":"sta"#).unwrap();
        let mut log = Log::open(&dir);
        log.note(Event::Ended, None, "failed");
        let kept = read(&dir.join(FILE));
        let events: Vec<&Event> = kept.iter().map(|entry| &entry.event).collect();
        assert_eq!(events, [&Event::Failed, &Event::Ended]);
        assert!(!kept[0].why.contains('\n'));
        assert_eq!(kept[0].why.chars().count(), REASON + 1);
        assert!(read(&dir.join("missing.jsonl")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_long_label_is_cut_like_a_reason() {
        let dir = folder("label");
        let mut log = Log::open(&dir);
        log.note(Event::Started, Some((1, &"a\nb ".repeat(50_000))), "");
        let size = std::fs::metadata(dir.join(FILE)).unwrap().len();
        assert!(size < 1_000, "one event took {size} bytes");
        let kept = read(&dir.join(FILE));
        assert_eq!(kept[0].label.chars().count(), REASON + 1);
        assert!(!kept[0].label.contains('\n'));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
