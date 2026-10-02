//! What the computer is told while nobody is looking at Letronna.
//!
//! A reply that ended, a card or a question waiting, and a helper, workflow
//! or scheduled run done are the moments worth calling someone back for.
//! The window and the terminal hear of them on their own paths and show
//! them their own way -- a toast, a banner, an escape sequence -- but
//! whether to say anything is decided once, here: only while nobody is
//! looking, at most one every `GAP`, and never with a key in it, since a
//! notification is read on a lock screen and kept in a list. Showing one is
//! a trait, so the platform's own lives with its front end and a test uses
//! a fake.

use crate::chat::ToolCall;
use crate::tools;
use async_channel::{Receiver, Sender};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// At most one notification in this long. Helpers ending together, or a
/// card that comes up as another run ends, are one reason to come back;
/// a burst like that is over in a few seconds, and a turn it starts takes
/// longer than this to need anyone again.
pub const GAP: Duration = Duration::from_secs(10);

/// A title is cut at this many characters: a banner shows about one line.
const TITLE_CHARS: usize = 60;

/// A body is cut at this many characters: a banner shows two or three
/// lines, and the rest is in the window.
const BODY_CHARS: usize = 180;

/// Ended work waiting for a front end that is slow to take it. More than a
/// burst of helpers ending together; past it `GAP` would drop them anyway.
const WAITING: usize = 16;

/// One notification: whose it is and what it says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    /// The session a click on it opens.
    pub session: u64,
    pub title: String,
    pub body: String,
}

/// Why someone is being called back.
pub enum Why<'a> {
    /// A turn ended, with the last answer it gave.
    Finished(&'a str),
    /// A turn stopped on an error.
    Failed(&'a str),
    /// A scheduled run ended, with the last answer it gave.
    Scheduled(&'a str),
    /// A call waits for the user -- a card, a plan or a question -- from
    /// the session itself or from the helper named.
    Waiting(&'a ToolCall, Option<&'a str>),
    /// Background work ended, in the words `listen` hands over.
    Ended(&'a str),
}

/// Shows a notification the platform's way.
pub trait Notifier {
    fn show(&mut self, note: &Note);
}

/// Decides which notifications are shown, and shows them.
pub struct Notices {
    notifier: Box<dyn Notifier>,
    /// When the last one was shown.
    last: Option<Instant>,
}

impl Notices {
    pub fn new(notifier: Box<dyn Notifier>) -> Self {
        Self {
            notifier,
            last: None,
        }
    }

    /// Show the note `make` builds when notifications are `on`, nobody is
    /// looking and none was shown in the last `GAP`; true when it was.
    /// Built only then, since hiding keys reads them from disk.
    pub fn post(
        &mut self,
        on: bool,
        away: bool,
        now: Instant,
        make: impl FnOnce() -> Note,
    ) -> bool {
        if !on || !away {
            return false;
        }
        if self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < GAP)
        {
            return false;
        }
        self.last = Some(now);
        self.notifier.show(&make());
        true
    }
}

/// The notification for `why` in session `session`, called `title`: keys
/// hidden, one line each, cut to fit.
pub fn note(session: u64, title: &str, why: Why) -> Note {
    let body = match why {
        Why::Finished(answer) => said("Finished", answer),
        Why::Failed(error) => said("Stopped", error),
        Why::Scheduled(answer) => said("Scheduled run finished", answer),
        Why::Waiting(call, helper) => waiting(call, helper),
        Why::Ended(what) => what.to_owned(),
    };
    let title = if title.trim().is_empty() {
        crate::BRAND
    } else {
        title
    };
    Note {
        session,
        title: fit(title, TITLE_CHARS),
        body: fit(&body, BODY_CHARS),
    }
}

/// `what`, and the first line of `text` after it when there is one, with
/// the marks markdown puts at a line's start or around its words left out.
fn said(what: &str, text: &str) -> String {
    let line = text
        .lines()
        .map(|line| line.trim_start_matches(['#', '>', ' ', '\t']).trim())
        .find(|line| !line.is_empty())
        .map(|line| line.replace("**", "").replace('`', ""));
    match line {
        Some(line) => format!("{what}: {line}"),
        None => format!("{what}."),
    }
}

/// What a call waiting for the user wants, said as the card would say it.
fn waiting(call: &ToolCall, helper: Option<&str>) -> String {
    let summary = tools::summary(&call.name, &call.arguments);
    let who = helper.map(|name| format!("Helper {name}"));
    // A tool with nothing to show beside its name, or only the workspace
    // root as list_dir's ".", is named alone.
    let what = match summary.as_str() {
        "" | "." => call.name.clone(),
        _ => format!("{} {summary}", call.name),
    };
    match call.name.as_str() {
        tools::ASK_USER => match who {
            Some(who) => format!("{who} asks: {summary}"),
            None => format!("Asks: {summary}"),
        },
        tools::EXIT_PLAN => format!("A plan is ready for your approval: {summary}"),
        _ => match who {
            Some(who) => format!("{who} is waiting for your approval: {what}"),
            None => format!("Waiting for your approval: {what}"),
        },
    }
}

/// `text` with keys hidden, on one line with no control characters -- an
/// escape in a terminal's notification would be obeyed -- and cut at `max`
/// characters. Hidden before it is cut, which could split a key.
fn fit(text: &str, max: usize) -> String {
    let text: String = tools::one_line(&crate::redact::scrub(text, true))
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    if text.chars().count() <= max {
        return text;
    }
    let cut: String = text.chars().take(max - 1).collect();
    format!("{}\u{2026}", cut.trim_end())
}

/// Set once a front end listens, so ended work is only kept for one.
static LISTENING: AtomicBool = AtomicBool::new(false);

/// Background work that ended: whose session it was and how it went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ended {
    pub session: u64,
    pub what: String,
}

fn ended_channel() -> &'static (Sender<Ended>, Receiver<Ended>) {
    static ENDED: OnceLock<(Sender<Ended>, Receiver<Ended>)> = OnceLock::new();
    ENDED.get_or_init(|| async_channel::bounded(WAITING))
}

/// The helpers and workflow runs that end from now on, for a front end
/// that notifies.
pub fn listen() -> Receiver<Ended> {
    LISTENING.store(true, Ordering::Relaxed);
    ended_channel().1.clone()
}

/// Say that background work of `session` ended, when a front end listens;
/// dropped when it is too far behind to show it anyway.
pub(crate) fn ended(session: u64, what: String) {
    if LISTENING.load(Ordering::Relaxed) {
        let _ = ended_channel().0.try_send(Ended { session, what });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Keeps what it was asked to show.
    #[derive(Clone, Default)]
    struct Fake(Arc<Mutex<Vec<Note>>>);

    impl Notifier for Fake {
        fn show(&mut self, note: &Note) {
            self.0.lock().unwrap().push(note.clone());
        }
    }

    fn notices() -> (Notices, Fake) {
        let fake = Fake::default();
        (Notices::new(Box::new(fake.clone())), fake)
    }

    fn plain(body: &str) -> Note {
        Note {
            session: 1,
            title: "A session".into(),
            body: body.into(),
        }
    }

    fn call(name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: "call_1".into(),
            name: name.into(),
            arguments: arguments.into(),
        }
    }

    #[test]
    fn nothing_is_shown_or_built_while_someone_looks_or_when_turned_off() {
        let (mut notices, fake) = notices();
        let now = Instant::now();
        assert!(!notices.post(true, false, now, || unreachable!("built while looked at")));
        assert!(!notices.post(false, true, now, || unreachable!("built while off")));
        assert!(fake.0.lock().unwrap().is_empty());
        // Neither counts towards the gap.
        assert!(notices.post(true, true, now, || plain("away")));
        assert_eq!(fake.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn at_most_one_is_shown_in_each_gap() {
        let (mut notices, fake) = notices();
        let start = Instant::now();
        assert!(notices.post(true, true, start, || plain("first")));
        assert!(!notices.post(true, true, start + GAP / 2, || plain("burst")));
        assert!(notices.post(true, true, start + GAP, || plain("later")));
        let shown: Vec<String> = fake
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|n| n.body.clone())
            .collect();
        assert_eq!(shown, ["first", "later"]);
    }

    #[test]
    fn a_note_says_how_a_run_ended_by_the_first_line_of_its_answer() {
        let answer = "\n## **Done** -- the `build` passes.\n\nMore detail.";
        assert_eq!(
            note(4, "Fix the build", Why::Finished(answer)),
            Note {
                session: 4,
                title: "Fix the build".into(),
                body: "Finished: Done -- the build passes.".into(),
            }
        );
        let failed = note(4, " ", Why::Failed(""));
        assert_eq!(
            (failed.title.as_str(), failed.body.as_str()),
            ("Letronna", "Stopped.")
        );
        let scheduled = note(4, "Nightly", Why::Scheduled("All green"));
        assert_eq!(scheduled.body, "Scheduled run finished: All green");
        let ended = note(4, "Nightly", Why::Ended("Helper explore finished"));
        assert_eq!(ended.body, "Helper explore finished");
    }

    #[test]
    fn a_waiting_call_is_said_as_its_card_says_it() {
        let body = |call: &ToolCall, helper| note(1, "t", Why::Waiting(call, helper)).body;
        let run = call("run_command", r#"{"command":"npm test"}"#);
        assert_eq!(
            body(&run, None),
            "Waiting for your approval: run_command npm test"
        );
        assert_eq!(
            body(&run, Some("explore")),
            "Helper explore is waiting for your approval: run_command npm test"
        );
        let ask = call(tools::ASK_USER, r#"{"question":"Which branch?"}"#);
        assert_eq!(body(&ask, None), "Asks: Which branch?");
        assert_eq!(
            body(&ask, Some("explore")),
            "Helper explore asks: Which branch?"
        );
        let plan = call(
            tools::EXIT_PLAN,
            r##"{"plan":"# Move the config\n\n1. Read it"}"##,
        );
        assert_eq!(
            body(&plan, None),
            "A plan is ready for your approval: Move the config"
        );
        let bare = call("mcp__github__list_issues", "{}");
        assert_eq!(
            body(&bare, None),
            "Waiting for your approval: mcp__github__list_issues"
        );
    }

    #[test]
    fn keys_and_control_characters_never_reach_a_notification() {
        let key = format!("sk-{}", "a1B2c3D4".repeat(4));
        let answer = format!("Set the key to {key}\x1b[2J\x07 and\u{9c} done");
        let shown = note(1, "Title\x1b]0;evil\x07", Why::Finished(&answer));
        assert!(!shown.body.contains(&key), "{}", shown.body);
        assert!(shown.body.contains(crate::redact::MARKER));
        for text in [&shown.title, &shown.body] {
            assert!(!text.chars().any(char::is_control), "{text:?}");
        }
    }

    #[test]
    fn a_long_title_or_body_is_cut_to_fit() {
        let shown = note(1, &"t".repeat(100), Why::Ended(&"word ".repeat(100)));
        assert_eq!(shown.title.chars().count(), TITLE_CHARS);
        assert!(shown.body.chars().count() <= BODY_CHARS);
        assert!(shown.title.ends_with('\u{2026}') && shown.body.ends_with('\u{2026}'));
    }
}
