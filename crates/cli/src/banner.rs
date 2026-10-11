//! The opening screen, and putting a session's conversation back on it.

use letronna_core::workflow::saved;

use crate::render;
use crate::session::Runner;

/// Messages shown when a session is picked up. Enough to remember where you
/// were, not so many that the window fills with what you already read.
const REPLAY: usize = 12;

/// Put a session's conversation back on the screen. Resuming without this
/// looks like nothing happened: the messages are all there and none of them
/// are in front of you.
pub fn replay(runner: &Runner, session: usize) {
    let messages = &runner.store.conversations[session].messages;
    let from = messages.len().saturating_sub(REPLAY);
    render::blank();
    if from > 0 {
        render::line(&render::dim(&format!(
            "… {from} earlier message{} not shown",
            if from == 1 { "" } else { "s" }
        )));
    }
    for message in &messages[from..] {
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
            "assistant" if !message.content.trim().is_empty() => {
                said_over_lines(&render::blue("●"), &message.content, false);
                if message.interrupted {
                    render::line(&render::dim("(stopped)"));
                }
            }
            // A tool's own output is not worth replaying, but that it ran is.
            "tool" => render::tool(
                &message.name,
                "",
                if message.is_error { "failed" } else { "ran" },
                message.is_error,
            ),
            _ => {}
        }
    }
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

pub fn banner(runner: &Runner, folder: &str, session: usize) {
    render::blank();

    let (gateway, model) = runner.store.model(session);
    let conversation = &runner.store.conversations[session];
    let mut facts = vec![
        format!(
            "{}  {}",
            render::bold(letronna_core::BRAND),
            render::dim(env!("CARGO_PKG_VERSION"))
        ),
        String::new(),
        fact("project", folder),
        fact("model", model),
        fact("gateway", gateway),
        fact(
            "session",
            &match conversation.messages.len() {
                0 => "new".to_owned(),
                1 => "carrying on, 1 message".to_owned(),
                n => format!("carrying on, {n} messages"),
            },
        ),
        fact(
            "mode",
            &format!(
                "{}: {}",
                runner.current_mode().label(),
                runner.current_mode().about()
            ),
        ),
        String::new(),
        render::dim("type / for commands"),
        render::dim("shift+enter for a new line · ↑ for what you said before"),
    ];

    // Side by side when the face fits; the details alone when it does not, so
    // a narrow window shows something readable rather than a wrapped mess.
    if !render::colour() || render::width() < render::FACE_WIDTH + render::FACE_GAP + 40 {
        for fact in facts.into_iter().filter(|f| !f.is_empty()) {
            render::line(&fact);
        }
        return;
    }

    // Sit the details against the middle of the face rather than its top.
    let lead = (render::FACE.len().saturating_sub(facts.len())) / 2;
    let mut column = vec![String::new(); lead];
    column.append(&mut facts);

    for row in 0..render::FACE.len().max(column.len()) {
        let face = render::FACE.get(row).copied().unwrap_or("");
        let text = column.get(row).cloned().unwrap_or_default();
        render::out(
            &format!(
                "{}{}{}",
                render::MARGIN,
                render::blue(&format!(
                    "{face:width$}",
                    width = render::FACE_WIDTH + render::FACE_GAP
                )),
                text
            ),
            true,
        );
    }
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

/// One `label  value` line of the startup details.
fn fact(label: &str, value: &str) -> String {
    format!("{}  {value}", render::dim(&format!("{label:<8}")))
}
