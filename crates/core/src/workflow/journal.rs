//! A workflow run's journal: each agent it started and what each answered,
//! one JSON line apiece, so a run that stopped part way is picked back up
//! without paying again for what already finished.
//!
//! An agent is known by a key made from what it was given, not from when it
//! was called: SHA-256 over its prompt, its options and how many earlier calls
//! of the run were the same. A chain of keys in call order misses every agent
//! after the first one to move, as pipeline stages do when they finish in a
//! new order; this key replays a finished agent whatever the order. A prompt is
//! all an agent is told, so one that embeds another's changed answer gets a
//! new key and runs again. Label and phase only say where an agent shows, so
//! they are left out. The options' keys are sorted by hand at every depth,
//! because serde_json keeps an object's keys in the order they came whenever
//! anything else in the build asks it to.
//!
//! Lines are only ever appended. A kill can tear the last one, even part way
//! through a character: it is skipped on reading, and the next line starts on
//! a line of its own.

use ring::digest;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

/// Options that only say where an agent shows, so changing them never makes a
/// finished agent run again.
const UNKEYED: [&str; 2] = ["label", "phase"];

/// One event of a run, as a line of its journal.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(super) enum Line {
    /// The script was started or resumed.
    Launched { script_sha256: String, args: Value },
    /// An agent began to run live.
    Started {
        key: String,
        label: String,
        phase: String,
    },
    /// An agent answered, with what that took.
    Result {
        key: String,
        value: Value,
        tokens: u64,
    },
    /// An agent ended without an answer.
    Failed { key: String },
    /// The run paused, and why.
    Paused { reason: String },
}

/// The journal file, open for appending.
pub(super) struct Journal {
    file: File,
}

impl Journal {
    /// Opens the journal at `path`, making it when it is not there, with every
    /// whole line it already holds.
    pub(super) fn open(path: &Path) -> Result<(Self, Vec<Line>), String> {
        let unreadable = |e: std::io::Error| format!("Could not open the workflow's journal: {e}");
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(path)
            .map_err(unreadable)?;
        // Bytes, not text: a torn line that ends inside a character would
        // make the whole file unreadable as text.
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(unreadable)?;
        if bytes.last().is_some_and(|&last| last != b'\n') {
            file.write_all(b"\n").map_err(unreadable)?;
        }
        Ok((Self { file }, parse(&bytes)))
    }

    /// Appends `line` in one write.
    pub(super) fn write(&mut self, line: &Line) -> Result<(), String> {
        let mut text = serde_json::to_string(line).map_err(|e| e.to_string())?;
        text.push('\n');
        self.file
            .write_all(text.as_bytes())
            .map_err(|e| format!("Could not write to the workflow's journal: {e}"))
    }
}

/// The lines of the journal at `path`, read without taking it for writing;
/// none when it cannot be read.
pub(super) fn read(path: &Path) -> Vec<Line> {
    std::fs::read(path)
        .map(|bytes| parse(&bytes))
        .unwrap_or_default()
}

/// The lines of a journal's bytes that parse, skipping a torn one.
fn parse(bytes: &[u8]) -> Vec<Line> {
    bytes
        .split(|&byte| byte == b'\n')
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect()
}

/// The answers `lines` hold, by key, each with the launch of the run that gave
/// it, counted from 1. The last word on a key wins, so one that ran live again
/// after it answered, and was in flight or failed, has none.
pub(super) fn answers(lines: &[Line]) -> HashMap<String, (Value, usize)> {
    let mut answers = HashMap::new();
    let mut launch = 0;
    for line in lines {
        match line {
            Line::Launched { .. } => launch += 1,
            Line::Result { key, value, .. } => {
                answers.insert(key.clone(), (value.clone(), launch));
            }
            Line::Started { key, .. } | Line::Failed { key } => {
                answers.remove(key);
            }
            Line::Paused { .. } => {}
        }
    }
    answers
}

/// The tokens every answer in `lines` took, over all the run's launches.
pub(super) fn spent(lines: &[Line]) -> u64 {
    lines
        .iter()
        .map(|line| match line {
            Line::Result { tokens, .. } => *tokens,
            _ => 0,
        })
        .sum()
}

/// A call's options as its key sees them: without label and phase, with every
/// object's keys in order at every depth, and none at all as `{}`.
pub(super) fn canonical(options: &Value) -> String {
    let Value::Object(map) = options else {
        return "{}".into();
    };
    let kept = Sorted::Object(
        map.iter()
            .filter(|(name, _)| !UNKEYED.contains(&name.as_str()))
            .map(|(name, value)| (name.as_str(), sorted(value)))
            .collect(),
    );
    serde_json::to_string(&kept).unwrap_or_default()
}

/// A call's key: "v1:" and the hex SHA-256 of its prompt, canonical options
/// and occurrence, split by zero bytes. JSON text and a number never hold one,
/// so however a prompt ends, no two calls' inputs make the same bytes.
pub(super) fn key(prompt: &str, canonical: &str, occurrence: u64) -> String {
    let mut hash = digest::Context::new(&digest::SHA256);
    hash.update(prompt.as_bytes());
    hash.update(&[0]);
    hash.update(canonical.as_bytes());
    hash.update(&[0]);
    hash.update(occurrence.to_string().as_bytes());
    format!("v1:{}", hex(hash.finish().as_ref()))
}

/// The hex SHA-256 of `text`, for telling one script from another.
pub(super) fn sha256(text: &str) -> String {
    hex(digest::digest(&digest::SHA256, text.as_bytes()).as_ref())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A JSON value that writes every object's keys in order, whatever order
/// serde_json keeps them in.
#[derive(Serialize)]
#[serde(untagged)]
enum Sorted<'a> {
    Object(BTreeMap<&'a str, Sorted<'a>>),
    Array(Vec<Sorted<'a>>),
    Other(&'a Value),
}

fn sorted(value: &Value) -> Sorted<'_> {
    match value {
        Value::Object(map) => Sorted::Object(
            map.iter()
                .map(|(name, value)| (name.as_str(), sorted(value)))
                .collect(),
        ),
        Value::Array(items) => Sorted::Array(items.iter().map(sorted).collect()),
        other => Sorted::Other(other),
    }
}
