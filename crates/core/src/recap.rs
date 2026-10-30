//! A few lines on where a session stands, for someone coming back to it.
//!
//! Back at a session left an hour or a day ago, the user has to scroll up
//! to remember what it was doing. `recap` asks a quick look for two or
//! three short lines instead: what was asked, what was done, what is left
//! or waiting on them. It is `agents::quick` -- the side model when one is
//! set, no tools, little thinking -- and what it costs counts with the
//! session's. It reads the session's own words, the user's messages and the
//! replies' text, the latest ones up to a bound and never a tool's output,
//! so it costs the same however long the session ran. Nothing is kept: the
//! front ends show it as a note.
//!
//! A front end offers one by itself where `due` says so, when the last
//! reply finished more than `AWAY` ago, and `/recap` asks for one any time.

use crate::cancel::Cancel;
use crate::chat::{Connection, Message};
use std::time::Duration;

/// How long a session must have been left for coming back to it to bring a
/// recap by itself: past a coffee, short of a meeting.
pub const AWAY: Duration = Duration::from_secs(30 * 60);

/// Characters of the session's words sent, the latest kept: a few turns,
/// which is what someone coming back needs, at a few cents at most.
const WORDS_CHARS: usize = 6_000;

/// Characters kept of any one message, so one long reply cannot crowd out
/// the turns around it.
const MESSAGE_CHARS: usize = 1_200;

/// Characters of the recap kept: three short lines.
const RECAP_CHARS: usize = 600;

/// How long the look gets before the front end gives up on it.
const WAIT: Duration = Duration::from_secs(30);

/// What the look is told it is for.
const INSTRUCTIONS: &str = "You write a recap for someone coming back to a working session after \
a break. In two or three short lines of plain text, say what they asked for, what has been done, \
and what is left or waiting on them. No greeting, headings, lists or markdown. Name files and \
things as the session does.";

/// Whether coming back to the session in `messages` at `now`, in seconds
/// since 1970, is worth a recap by itself: it has a reply, and the last
/// one finished more than `AWAY` before.
pub fn due(messages: &[Message], now: u64) -> bool {
    messages
        .iter()
        .rev()
        .find(|message| message.role == "assistant" && message.at > 0)
        .is_some_and(|last| now.saturating_sub(last.at) >= AWAY.as_secs())
}

/// Two or three lines on where the session in `messages` stands, from a
/// quick look on `connection`. It waits on the network, so it never runs on
/// a UI thread; `cancel` gives up on it, as does `WAIT` passing.
pub fn recap(
    connection: &Connection,
    messages: &[Message],
    cancel: &Cancel,
) -> Result<String, String> {
    let words = words(messages);
    if words.is_empty() {
        return Err("Nothing has been said in this session yet.".into());
    }
    let bounded = cancel.child();
    let timer = bounded.clone();
    std::thread::spawn(move || {
        if timer.sleep(WAIT) {
            timer.set();
        }
    });
    let answer = crate::agents::quick(connection, INSTRUCTIONS, &words, bounded.clone());
    // Wakes the timer, whose thread would otherwise sleep on.
    bounded.set();
    let recap = plain(&answer?);
    if recap.is_empty() {
        return Err("No recap came.".into());
    }
    Ok(recap)
}

/// The session's words a recap reads, oldest first: each message the user
/// typed and each reply's text, the latest that fit in `WORDS_CHARS`.
fn words(messages: &[Message]) -> String {
    let mut kept = Vec::new();
    let mut room = WORDS_CHARS;
    for message in messages.iter().rev() {
        let who = match message.role.as_str() {
            "user" if !message.notice => "User",
            "assistant" => "Assistant",
            _ => continue,
        };
        let text = message.content.trim();
        if text.is_empty() {
            continue;
        }
        let text: String = text.chars().take(MESSAGE_CHARS.min(room)).collect();
        room -= text.chars().count();
        kept.push(format!("{who}: {text}"));
        if room == 0 {
            break;
        }
    }
    kept.reverse();
    kept.join("\n\n")
}

/// The look's answer as plain lines: no markdown marks it was told not to
/// use, no blank lines, and no more than `RECAP_CHARS`.
fn plain(answer: &str) -> String {
    let lines: Vec<&str> = answer
        .lines()
        .map(|line| line.trim().trim_start_matches(['#', '-', '*', '•']).trim())
        .filter(|line| !line.is_empty())
        .collect();
    let text = lines.join("\n").replace("**", "");
    match text.char_indices().nth(RECAP_CHARS) {
        Some((at, _)) => format!("{}…", text[..at].trim_end()),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::{AWAY, MESSAGE_CHARS, WORDS_CHARS, due, plain, words};
    use crate::chat::Message;

    fn said(role: &str, content: &str, at: u64) -> Message {
        Message {
            role: role.into(),
            content: content.into(),
            at,
            ..Default::default()
        }
    }

    #[test]
    fn a_recap_is_due_only_once_the_last_reply_is_older_than_the_time_away() {
        let now = 1_800_000_000;
        let away = AWAY.as_secs();
        let left = |ago: u64| {
            vec![
                said("user", "fix it", 0),
                said("assistant", "fixed", now - ago),
            ]
        };
        assert!(due(&left(away), now));
        assert!(due(&left(3 * away), now));
        assert!(!due(&left(away - 1), now));
        // A session with no reply yet, or one from before replies kept a time.
        assert!(!due(&[said("user", "fix it", 0)], now));
        assert!(!due(&[said("assistant", "fixed", 0)], now));
    }

    #[test]
    fn a_recap_reads_what_was_said_and_never_a_tool_s_output_or_a_notice() {
        let mut notice = said("user", "Job 3 finished.", 0);
        notice.notice = true;
        let messages = [
            said("user", "Make the login page", 0),
            said("tool", "secret build log", 0),
            notice,
            said("assistant", "  Made login.html.  ", 1),
            said("assistant", "", 2),
        ];
        assert_eq!(
            words(&messages),
            "User: Make the login page\n\nAssistant: Made login.html."
        );
    }

    #[test]
    fn a_long_session_sends_only_its_latest_words_within_the_bound() {
        let long = "x".repeat(5 * MESSAGE_CHARS);
        let mut messages: Vec<Message> = (0..50).map(|_| said("assistant", &long, 1)).collect();
        messages.push(said("user", "and now the tests", 0));
        let sent = words(&messages);
        assert!(sent.ends_with("User: and now the tests"), "{sent}");
        let text: usize = sent
            .split("\n\n")
            .map(|part| {
                part.split_once(": ")
                    .map_or(0, |(_, text)| text.chars().count())
            })
            .sum();
        assert_eq!(text, WORDS_CHARS);
        assert!(
            sent.split("\n\n")
                .all(|part| part.chars().count() <= MESSAGE_CHARS + 11)
        );
    }

    #[test]
    fn a_recap_comes_back_as_plain_short_lines() {
        let answer = "## Recap\n\n- You asked for **dark mode**.\n* It is done in `theme.rs`.\n\n";
        assert_eq!(
            plain(answer),
            "Recap\nYou asked for dark mode.\nIt is done in `theme.rs`."
        );
        let long = "word ".repeat(400);
        let cut = plain(&long);
        assert!(cut.ends_with('…') && cut.chars().count() <= super::RECAP_CHARS + 1);
    }
}
