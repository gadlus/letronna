//! A provider's replies as they came, kept for looking into one that went
//! wrong.
//!
//! What a model is said to have called is what Letronna made of the stream:
//! pieces of several calls put together by their index, their ids and their
//! names. When calls arrive with no arguments, or with two calls' run
//! together, nothing kept says whether the provider sent them so or
//! Letronna put them together wrongly, and a benchmark's review of a model
//! that sent empty calls in a quarter of its batched replies could blame
//! neither. With `LETRONNA_DUMP_STREAMS` naming a folder, each request's
//! body and every line of its reply's stream are written there, one file a
//! request, as they were sent and as they came.
//!
//! The files hold all the model was sent, the user's files and words among
//! it, so nothing is written unless the variable is set, and never the
//! request's headers, which carry the key. A reply that comes whole, not
//! as a stream, is not kept.

use serde_json::Value;
use std::cell::RefCell;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

thread_local! {
    /// The file of the request this thread is sending or reading the reply
    /// of: a request is sent and its stream read on one thread.
    static OPEN: RefCell<Option<BufWriter<File>>> = const { RefCell::new(None) };
}

/// The folder `LETRONNA_DUMP_STREAMS` names, if it names one.
fn folder() -> Option<PathBuf> {
    let named = std::env::var_os("LETRONNA_DUMP_STREAMS")?;
    (!named.is_empty()).then(|| PathBuf::from(named))
}

/// Starts the file of a request about to go to `url` with `body` for
/// session `conversation`, if streams are kept; ends the one before it.
pub(crate) fn begin(conversation: u64, url: &str, body: &Value) {
    begin_in(folder().as_deref(), conversation, url, body);
}

/// `begin`, keeping streams in `folder` when there is one.
fn begin_in(folder: Option<&Path>, conversation: u64, url: &str, body: &Value) -> Option<PathBuf> {
    static REQUESTS: AtomicU64 = AtomicU64::new(0);
    end("");
    let folder = folder?;
    std::fs::create_dir_all(folder).ok()?;
    // By session, then in the order sent; the process, as a helper's
    // requests and a second run's land in the same folder.
    let n = REQUESTS.fetch_add(1, Ordering::Relaxed);
    let path = folder.join(format!("{conversation}-{}-{n:05}.sse", std::process::id()));
    let mut file = BufWriter::new(File::create(&path).ok()?);
    writeln!(file, ">>> POST {url}").ok()?;
    writeln!(file, "{body}").ok()?;
    writeln!(file, "<<<").ok()?;
    OPEN.with(|open| *open.borrow_mut() = Some(file));
    Some(path)
}

/// Keeps `line` of the reply being read, as it came.
pub(crate) fn line(line: &str) {
    OPEN.with(|open| {
        if let Some(file) = open.borrow_mut().as_mut() {
            let _ = writeln!(file, "{line}");
        }
    });
}

/// Ends the file of the request this thread was on, with `how` the reply
/// ended when it did not end well.
pub(crate) fn end(how: &str) {
    OPEN.with(|open| {
        if let Some(mut file) = open.borrow_mut().take() {
            if !how.is_empty() {
                let _ = writeln!(file, "=== {how}");
            }
            let _ = file.flush();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_request_and_its_stream_are_kept_as_sent_and_as_they_came() {
        let folder = std::env::temp_dir().join(format!("letronna-streams-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let body = json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]});
        let path = begin_in(
            Some(&folder),
            42,
            "https://api.test/v1/chat/completions",
            &body,
        )
        .expect("a file");
        line(r#"data: {"choices":[{"delta":{"content":"He"}}]}"#);
        line("");
        line("data: [DONE]");
        end("");
        let kept = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            kept,
            format!(
                ">>> POST https://api.test/v1/chat/completions\n{body}\n<<<\n\
                 data: {{\"choices\":[{{\"delta\":{{\"content\":\"He\"}}}}]}}\n\ndata: [DONE]\n"
            )
        );
        assert!(
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("42-")
        );
        // The next request's file ends this one's; a failure is said last.
        let next = begin_in(Some(&folder), 42, "https://api.test/v1", &body).unwrap();
        assert_ne!(next, path);
        end("Connection interrupted before the response completed.");
        let kept = std::fs::read_to_string(&next).unwrap();
        assert!(kept.ends_with("<<<\n=== Connection interrupted before the response completed.\n"));
        // With no folder named nothing is written, and a line goes nowhere.
        assert_eq!(begin_in(None, 42, "https://api.test/v1", &body), None);
        line("data: lost");
        end("");
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 2);
        std::fs::remove_dir_all(folder).unwrap();
    }
}
