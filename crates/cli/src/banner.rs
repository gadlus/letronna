//! The opening screen, and putting a session's conversation back on it.

use letronna_core::workflow::saved;

use crate::render;
use crate::session::Runner;

/// Messages shown when a session is picked up: the whole of nearly every
/// session, scrolled back through as it was had, and a bound on the very
/// longest, where /export has the rest.
const REPLAY: usize = 1_000;

/// Put a session's conversation back on the screen. Resuming without this
/// looks like nothing happened: the messages are all there and none of them
/// are in front of you.
pub fn replay(runner: &Runner, session: usize) {
    let messages = &runner.store.conversations[session].messages;
    let from = messages.len().saturating_sub(REPLAY);
    render::blank();
    if from > 0 {
        render::line(&render::dim(&format!(
            "… {from} earlier message{} not shown; /export writes them all",
            if from == 1 { "" } else { "s" }
        )));
    }
    for (at, message) in messages.iter().enumerate().skip(from) {
        match message.role.as_str() {
            "user" if message.notice => render::notice(&message.content),
            "user" => {
                // `/init`, a saved workflow's command and `@` references show
                // as typed; a chat app's message can quote someone else's,
                // fenced.
                let content = letronna_core::references::typed(&message.content);
                let typed = letronna_core::init::typed(content).map(str::to_owned);
                let text = match typed.or_else(|| saved::typed(content)) {
                    Some(typed) => typed,
                    None => letronna_core::untrusted::shown("", content).0,
                };
                said_over_lines(&render::blue("›"), &text, true)
            }
            // Drawn as a reply streamed in is, its code and headings too.
            "assistant" if !message.content.trim().is_empty() => {
                render::blank();
                let mut shown = render::Stream::opening_with(render::bullet());
                shown.push(message.content.trim_end());
                shown.end();
                if message.interrupted {
                    render::line(&render::dim("(stopped)"));
                }
            }
            // A tool's own output is not worth replaying, but what it ran on
            // is, as the call it answers names it, a line apart as when it ran.
            "tool" => {
                let summary = called_with(&messages[..at], &message.tool_call_id)
                    .map(|args| letronna_core::tools::summary(&message.name, args))
                    .unwrap_or_default();
                render::blank();
                render::tool(
                    &message.name,
                    &summary,
                    if message.is_error { "failed" } else { "ran" },
                    message.is_error,
                )
            }
            _ => {}
        }
    }
}

/// The arguments of the call `id` names, from the reply before it that
/// made the call.
fn called_with<'a>(earlier: &'a [letronna_core::chat::Message], id: &str) -> Option<&'a str> {
    (earlier.iter().rev())
        .find(|message| message.role == "assistant")?
        .tool_calls
        .iter()
        .find(|call| call.id == id)
        .map(|call| call.arguments.as_str())
}

/// A message with its mark, its later lines under the first.
fn said_over_lines(mark: &str, text: &str, bold: bool) {
    render::blank();
    let mut parts = text.lines();
    let first = parts.next().unwrap_or_default();
    render::said(
        mark,
        &if bold {
            render::bold(first)
        } else {
            first.to_owned()
        },
    );
    for part in parts {
        render::line(part);
    }
}

pub fn folder_name(workspace: Option<&std::path::Path>) -> String {
    workspace
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "no project".into())
}

/// The least room the details get beside the face; with less they stand
/// alone, as the face beside a column cut to nothing says less than the
/// column would.
const BESIDE: usize = 24;

/// Below this width the two hints are left out: cut to fit, they say
/// nothing.
const HINTS: usize = 30;

pub fn banner(runner: &Runner, folder: &str, session: usize) {
    render::blank();

    let (gateway, model) = runner.store.model(session);
    let conversation = &runner.store.conversations[session];
    let facts = vec![
        ("project", folder.to_owned()),
        ("model", model.to_owned()),
        ("gateway", gateway.to_owned()),
        (
            "session",
            match conversation.messages.len() {
                0 => "new".to_owned(),
                1 => "carrying on, 1 message".to_owned(),
                n => format!("carrying on, {n} messages"),
            },
        ),
        (
            "mode",
            format!(
                "{}: {}",
                runner.current_mode().label(),
                runner.current_mode().about()
            ),
        ),
    ];
    let colour = render::colour();
    // Laid out again whenever the window changes width, so shrinking it
    // never wraps the face into pieces.
    render::block(move |width| laid_out(&facts, width, colour));
}

/// How to start a new line, as this terminal can: shift-enter only where it
/// reaches the box as itself, and a backslash before enter everywhere.
fn new_line_hint() -> String {
    let keys = if crate::screen::shift_enter() {
        "shift+enter or \\+enter"
    } else {
        "\\+enter or ctrl+j"
    };
    format!("{keys} for a new line · ↑ for what you said before")
}

/// The opening screen for a window `width` columns wide: the face with the
/// details beside it where both fit, else the details alone, every row cut
/// to fit rather than wrapped.
fn laid_out(facts: &[(&str, String)], width: usize, colour: bool) -> Vec<String> {
    let face = render::MARGIN.len() + render::FACE_WIDTH + render::FACE_GAP;
    let beside = colour && width >= face + BESIDE;
    let room = match beside {
        true => width - face,
        false => width.saturating_sub(render::PAD.len()),
    };
    let mut column = vec![title(room), String::new()];
    column.extend(facts.iter().map(|(label, value)| fact(label, value, room)));
    if width >= HINTS {
        column.push(String::new());
        for hint in ["type / for commands", &new_line_hint()] {
            column.push(render::dim(&render::clip(hint, room)));
        }
    }

    // A narrow window shows something readable rather than a wrapped mess.
    if !beside {
        return column
            .into_iter()
            .filter(|row| !row.is_empty())
            .map(|row| format!("{}{row}", render::PAD))
            .collect();
    }

    // Sit the details against the middle of the face rather than its top.
    let lead = (render::FACE.len().saturating_sub(column.len())) / 2;
    let mut rows = vec![String::new(); lead];
    rows.append(&mut column);
    (0..render::FACE.len().max(rows.len()))
        .map(|row| {
            let face = render::FACE.get(row).copied().unwrap_or("");
            let text = rows.get(row).map(String::as_str).unwrap_or_default();
            format!(
                "{}{}{text}",
                render::MARGIN,
                render::blue(&format!(
                    "{face:width$}",
                    width = render::FACE_WIDTH + render::FACE_GAP
                ))
            )
        })
        .collect()
}

/// The name and version, or the name alone cut to `room` when both do not
/// fit.
fn title(room: usize) -> String {
    let (brand, version) = (letronna_core::BRAND, env!("CARGO_PKG_VERSION"));
    if brand.chars().count() + 2 + version.chars().count() <= room {
        return format!("{}  {}", render::bold(brand), render::dim(version));
    }
    render::bold(&render::clip(brand, room))
}

/// The standing note under the input box: who the question is going to, and
/// anything about this session that changes what happens when you press
/// enter.
pub fn footer(runner: &Runner) -> String {
    let settings = &runner.store.settings;
    // The session on screen is always the store's active one here.
    let session = runner.store.active;
    let (gateway, model) = runner.store.model(session);
    let mut parts = vec![model.to_owned(), gateway.to_owned()];
    // The mode is not here: the bar shows it at the front, with its key.
    let crew =
        (runner.store.conversations.get(session)).is_some_and(|c| letronna_core::crew::on(c.id));
    if crew {
        parts.push("crew".into());
    }
    let effort = runner.store.effort(session);
    if !effort.is_empty() {
        parts.push(format!("thinking {effort}"));
    }
    if settings.search {
        parts.push("web".into());
    }
    if settings.voice {
        parts.push("voice".into());
    }
    parts.join("  ·  ")
}

/// One `label  value` line of the startup details, cut to `room` columns.
fn fact(label: &str, value: &str, room: usize) -> String {
    let label = format!("{label:<8}  ");
    let value = render::clip(value, room.saturating_sub(label.chars().count()));
    format!("{}{value}", render::dim(&render::clip(&label, room)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Vec<(&'static str, String)> {
        vec![
            ("project", "a-project-folder-with-a-rather-long-name".into()),
            ("model", "provider/a-model-with-a-long-identifier".into()),
            ("gateway", "Letronna".into()),
            ("session", "carrying on, 12 messages".into()),
            (
                "mode",
                "Ask: asks before anything that changes files or runs commands".into(),
            ),
        ]
    }

    /// Whether the face is among `rows`.
    fn shows_face(rows: &[String]) -> bool {
        rows.iter().any(|row| row.contains(render::FACE[1]))
    }

    #[test]
    fn the_banner_never_runs_past_the_window() {
        for colour in [true, false] {
            for width in 10..=160 {
                for row in laid_out(&facts(), width, colour) {
                    assert!(render::visible(&row) <= width, "{width}: {row:?}");
                }
            }
        }
    }

    #[test]
    fn the_face_shows_only_where_it_fits_beside_the_details() {
        let least = render::MARGIN.len() + render::FACE_WIDTH + render::FACE_GAP + BESIDE;
        assert!(shows_face(&laid_out(&facts(), least, true)));
        assert!(shows_face(&laid_out(&facts(), 160, true)));
        assert!(!shows_face(&laid_out(&facts(), least - 1, true)));
        assert!(!shows_face(&laid_out(&facts(), 160, false)));
        // Alone, the details keep the title, every fact and both hints,
        // each on a row of its own.
        let alone = laid_out(&facts(), 40, true);
        assert_eq!(alone.len(), 1 + facts().len() + 2);
        assert!(alone.iter().any(|row| row.contains("project")));
    }

    #[test]
    fn a_replayed_tool_row_names_what_it_ran_on() {
        use letronna_core::chat::{Message, ToolCall};
        let asked = Message {
            role: "assistant".into(),
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                name: "read_file".into(),
                arguments: r#"{"path": "src/main.rs"}"#.into(),
            }],
            ..Default::default()
        };
        let earlier = [asked];
        assert_eq!(
            super::called_with(&earlier, "call_1"),
            Some(r#"{"path": "src/main.rs"}"#)
        );
        assert_eq!(super::called_with(&earlier, "call_2"), None);
    }
}
