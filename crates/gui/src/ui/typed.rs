//! Slash commands typed in the window's box. The terminal's everyday ones
//! run the window's own way of doing the same thing, so a habit from one
//! works in the other, and one the window has no way for says so rather
//! than reach the model as words.

use crate::prelude::*;
use crate::ui::actions::{Span, turns};
use letronna_core::gate::Mode;

actions!(
    session,
    [
        CompactSession,
        ExportSession,
        RenameSession,
        ForkSession,
        PickModel,
        TogglePlan
    ]
);

/// What a typed command does in the window.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Does {
    New,
    Model,
    Mode,
    Plan,
    Compact,
    Rewind,
    Export,
    Usage,
    Context,
    Rename,
    Copy,
    Retry,
    Fork,
    Changes,
    Resume,
    Palette,
    Pictures,
    Jobs,
    Memory,
    Page(Page),
}

/// The window's way of doing what `name`, without its slash, does in the
/// terminal.
fn does(name: &str) -> Option<Does> {
    Some(match name {
        // A fresh start, as the terminal's /clear is.
        "new" | "clear" => Does::New,
        "model" => Does::Model,
        "mode" => Does::Mode,
        "plan" => Does::Plan,
        "compact" => Does::Compact,
        "rewind" => Does::Rewind,
        "export" => Does::Export,
        "usage" => Does::Usage,
        "context" => Does::Context,
        "rename" => Does::Rename,
        "copy" => Does::Copy,
        "retry" => Does::Retry,
        "branch" | "fork" => Does::Fork,
        "diff" => Does::Changes,
        "resume" => Does::Resume,
        "help" => Does::Palette,
        "image" => Does::Pictures,
        "jobs" => Does::Jobs,
        "memory" => Does::Memory,
        "skills" | "mcp" => Does::Page(Page::Capabilities),
        "plugins" => Does::Page(Page::Plugins),
        "workflows" => Does::Page(Page::Workflows),
        "agents" => Does::Page(Page::Activity),
        "wallet" => Does::Page(Page::Wallet),
        _ => return None,
    })
}

/// The terminal's commands the window has no way of its own for, said to
/// be the terminal's rather than sent to the model as words.
const TERMINAL_ONLY: &[&str] = &[
    "connect",
    "crew",
    "decisions",
    "exit",
    "fallback",
    "gateways",
    "import",
    "insights",
    "login",
    "logout",
    "memory-review",
    "notify",
    "preview",
    "quit",
    "remember",
    "retention",
    "search",
    "status",
    "style",
    "team",
    "thinking",
    "tool-search",
    "undo",
    "verify",
    "voice",
    "where",
    "whoami",
    "work",
];

/// The commands the box lists as it is typed, as (name, what it does).
pub(crate) const LISTED: &[(&str, &str)] = &[
    ("model", "pick the model"),
    ("plan", "switch plan mode on or off"),
    ("compact", "summarize the earlier turns now"),
    ("rewind", "go back to an earlier message"),
    ("retry", "ask the last message again"),
    ("copy", "copy the last reply"),
    ("branch", "carry on in a copy of this session"),
    ("rename", "rename this session"),
    ("export", "save this session as Markdown"),
    ("context", "see what fills the context"),
    ("usage", "see what this session used"),
    ("diff", "show the changes"),
    ("resume", "switch to a recent session"),
    ("clear", "start a fresh session"),
    ("help", "everything the window can do, with its keys"),
];

/// Why `name` was not sent, when it is one of the terminal's commands that
/// the window has no way for.
fn terminal_only(name: &str) -> Option<String> {
    TERMINAL_ONLY.contains(&name).then(|| {
        format!(
            "/{name} is one of the terminal's commands, and the window has no way of \
             its own for it yet: run it in `letronna`, or look in the command palette."
        )
    })
}

/// The newest of the turns that are replies.
fn newest_reply(spans: &[Span]) -> Option<Span> {
    spans.iter().rev().find(|span| !span.yours).copied()
}

/// What `/plan rest` sets the mode to from `now`, going back to `usual`
/// when it is turned off, or why `rest` is no way to say.
fn planned(rest: &str, now: Mode, usual: Mode) -> Result<Mode, &'static str> {
    let back = if usual == Mode::Plan {
        Mode::Default
    } else {
        usual
    };
    match rest {
        "on" => Ok(Mode::Plan),
        "off" => Ok(back),
        "" if now == Mode::Plan => Ok(back),
        "" => Ok(Mode::Plan),
        _ => Err("/plan on or /plan off; /plan alone switches it."),
    }
}

impl Chat {
    /// Run `/name rest` when the window has a way of its own for it, and
    /// say whether it did; the box is emptied of it either way it runs.
    pub(crate) fn typed_command(
        &mut self,
        prompt: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(typed) = prompt.strip_prefix('/') else {
            return false;
        };
        let (name, rest) = typed.split_once(char::is_whitespace).unwrap_or((typed, ""));
        let Some(does) = does(name) else {
            return false;
        };
        let rest = rest.trim().to_owned();
        self.composer
            .update(cx, |input, cx| input.set_value("", window, cx));
        match does {
            Does::New => self.new_chat(window, cx),
            Does::Model => self.open_model_picker(window, cx),
            Does::Mode => self.open_mode_menu(window, cx),
            Does::Plan => self.plan_command(&rest, cx),
            Does::Compact => self.compact_now(window, cx),
            Does::Rewind => self.go_back(window, cx),
            Does::Export => self.export_session(self.store.active, window, cx),
            Does::Usage => window.dispatch_action(Box::new(super::usage::OpenUsage), cx),
            Does::Context => self.context_command(cx),
            Does::Rename => self.rename_command(&rest, window, cx),
            Does::Copy => match self.newest_reply() {
                Some(span) => self.copy_turn(span, window, cx),
                None => self.note = Some("No reply here to copy yet.".into()),
            },
            Does::Retry => self.retry_command(window, cx),
            Does::Fork => self.fork_now(window, cx),
            Does::Changes => window.dispatch_action(Box::new(super::changes::ChangesTab), cx),
            Does::Resume => window.dispatch_action(Box::new(super::switcher::SwitchSession), cx),
            Does::Palette => window.dispatch_action(Box::new(super::palette::OpenPalette), cx),
            Does::Pictures => self.pick_pictures(window, cx),
            Does::Jobs => window.dispatch_action(Box::new(super::jobs::JobsTab), cx),
            Does::Memory => window.dispatch_action(Box::new(super::remember::MemoryMenu), cx),
            Does::Page(page) => self.open_page(page, window, cx),
        }
        cx.notify();
        true
    }

    /// Say that `/name` is the terminal's, when it is one the window has no
    /// way for; false when it is no command of the terminal's either.
    pub(crate) fn terminal_command(&mut self, prompt: &str, cx: &mut Context<Self>) -> bool {
        let name = prompt
            .strip_prefix('/')
            .and_then(|typed| typed.split_whitespace().next())
            .unwrap_or_default();
        let Some(why) = terminal_only(name) else {
            return false;
        };
        self.error = Some(why);
        cx.notify();
        true
    }

    /// The newest reply in the session on screen.
    fn newest_reply(&self) -> Option<Span> {
        let open = &self.store.conversations[self.store.active];
        newest_reply(&turns(&open.messages, false))
    }

    /// Whether nothing holds the session on screen, said when something does.
    fn free_for(&mut self, what: &str) -> bool {
        let open = self.store.conversations[self.store.active].id;
        let free = !self.busy && !self.compacting.contains(&open) && !self.run_holds_open();
        if !free {
            self.error = Some(format!("{what} once the reply here is done."));
        }
        free
    }

    /// Summarize the earlier turns of the session on screen now.
    pub(crate) fn compact_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.free_for("Summarize") {
            return cx.notify();
        }
        match self.newest_reply() {
            Some(span) => self.summarize_turn(span, window, cx),
            None => self.note = Some("Nothing to summarize here yet.".into()),
        }
        cx.notify();
    }

    /// Carry on in a copy of the whole session on screen.
    pub(crate) fn fork_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.free_for("Fork") {
            return cx.notify();
        }
        let messages = self.store.conversations[self.store.active].messages.len();
        if messages == 0 {
            self.note = Some("Nothing to fork here yet.".into());
            return cx.notify();
        }
        self.fork_at(messages, window, cx);
        self.note = Some("Forked: this is the copy, and the original is in the sidebar.".into());
        cx.notify();
    }

    fn retry_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.free_for("Retry") {
            return;
        }
        match self.newest_reply() {
            Some(span) => self.retry_turn(span, window, cx),
            None => self.note = Some("Nothing to retry here yet.".into()),
        }
    }

    /// Rename the session on screen: to `title` when one is given, or in
    /// the dialog the sidebar's Rename opens.
    pub(crate) fn rename_command(
        &mut self,
        title: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if title.is_empty() {
            return self.rename_session(self.store.active, window, cx);
        }
        self.store.conversations[self.store.active].title = title.to_owned();
        self.save();
        self.note = Some(format!("Renamed to {title}."));
    }

    /// Switch plan mode as `/plan rest` asks, from the session's mode.
    pub(crate) fn plan_command(&mut self, rest: &str, cx: &mut Context<Self>) {
        let now = self.active_mode();
        match planned(rest, now, self.store.settings.mode) {
            Ok(mode) => {
                self.set_mode(mode, cx);
                self.note = Some(if mode == Mode::Plan {
                    "Plan mode on: it reads and proposes, and changes nothing.".into()
                } else {
                    format!("Plan mode off: {}.", mode.label())
                });
            }
            Err(how) => self.error = Some(how.into()),
        }
    }

    /// Open what fills the context, once the model has answered here and
    /// there is something to add up.
    fn context_command(&mut self, cx: &mut Context<Self>) {
        let answered = self.store.conversations[self.store.active]
            .messages
            .iter()
            .any(|m| m.role == "assistant" && m.window > 0 && m.input_tokens() > 0);
        if answered {
            self.open_context_popover(cx);
        } else {
            self.note =
                Some("What fills the context shows once the model has answered here.".into());
        }
    }
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{
        Does, LISTED, Mode, Span, TERMINAL_ONLY, does, newest_reply, planned, terminal_only,
    };

    #[test]
    fn a_terminal_command_runs_the_windows_own_way_or_says_it_is_the_terminals() {
        assert_eq!(does("clear"), Some(Does::New));
        assert_eq!(does("branch"), Some(Does::Fork));
        assert_eq!(does("fork"), Some(Does::Fork));
        assert_eq!(does("diff"), Some(Does::Changes));
        assert_eq!(does("hello"), None);
        assert!(terminal_only("doctor").is_none());
        assert!(terminal_only("undo").is_some_and(|why| why.contains("/undo")));
        // Never both: a command the window does is not called the terminal's.
        for name in TERMINAL_ONLY {
            assert_eq!(does(name), None, "{name}");
        }
        for (name, _) in LISTED {
            assert!(does(name).is_some(), "{name}");
        }
    }

    #[test]
    fn plan_switches_on_and_back_to_the_usual_mode() {
        assert_eq!(planned("", Mode::Default, Mode::Default), Ok(Mode::Plan));
        assert_eq!(
            planned("", Mode::Plan, Mode::AcceptEdits),
            Ok(Mode::AcceptEdits)
        );
        assert_eq!(planned("off", Mode::Plan, Mode::Plan), Ok(Mode::Default));
        assert_eq!(planned("on", Mode::Plan, Mode::Default), Ok(Mode::Plan));
        assert!(planned("maybe", Mode::Default, Mode::Default).is_err());
    }

    #[test]
    fn the_newest_reply_is_the_last_turn_that_is_not_yours() {
        let span = |first, last, yours| Span { first, last, yours };
        let spans = [span(0, 0, true), span(1, 3, false), span(4, 4, true)];
        assert_eq!(newest_reply(&spans), Some(span(1, 3, false)));
        assert_eq!(newest_reply(&spans[..1]), None);
    }
}
