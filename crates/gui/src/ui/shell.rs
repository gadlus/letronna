//! `!command` in the box runs the command in the session's folder, as the
//! terminal's shell mode does: through the tool the model runs commands
//! with, without asking, since the user typed it. What it printed goes into
//! the chat as a notice, which the model reads with the next message, so
//! it knows what was run and what came of it.

use crate::prelude::*;
use letronna_core::inbox::{self, Notice, When};

/// What `!command` is to say to the model, which also reads as the
/// notice's headline in the chat.
fn ran(command: &str, output: &str) -> Notice {
    Notice {
        from: "the shell".to_owned(),
        when: When::Quiet,
        text: format!("The user ran `{command}` in the window:\n{output}"),
        files: Vec::new(),
    }
}

/// The command a line typed in the box runs, when it starts with `!`.
pub(crate) fn shell_command(line: &str) -> Option<&str> {
    line.strip_prefix('!').map(str::trim)
}

impl Chat {
    /// Run `command` in the session on screen's folder, off the window's
    /// thread, and put what it printed in the chat once it is done: at the
    /// end of the session when nothing runs there, or else with the next
    /// message, as a notice that came in meanwhile is.
    pub(crate) fn shell_line(&mut self, command: &str, cx: &mut Context<Self>) {
        if command.is_empty() {
            self.note = Some("! and a command runs it here, as in a shell: !git status".into());
            return cx.notify();
        }
        if self.shell.is_some() {
            self.error = Some("A command typed with ! is still running. Esc stops it.".into());
            return cx.notify();
        }
        let open = &self.store.conversations[self.store.active];
        let session = open.id;
        let connection = Connection {
            workspace: open.workspace.clone(),
            conversation: session,
            plan: self.mode(session) == letronna_core::gate::Mode::Plan,
            ..Default::default()
        };
        let mut ctx = letronna_core::tools::ToolContext::of(&connection);
        ctx.approved = true;
        let cancel = ctx.cancel.clone();
        self.shell = Some(cancel);
        let command = command.to_owned();
        let args = serde_json::json!({ "command": command }).to_string();
        let running = cx
            .background_executor()
            .spawn(async move { letronna_core::tools::run("run_command", &args, &ctx) });
        self.note = Some(format!("Running {command}… Esc stops it."));
        cx.spawn(async move |this, cx| {
            let output = running.await.unwrap_or_else(|error| error);
            let _ = this.update(cx, |this, cx| {
                this.shell = None;
                this.shell_ran(session, ran(&command, &output), cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// Stop the `!command` running, when one is; false when none is.
    pub(crate) fn stop_shell(&mut self) -> bool {
        let Some(cancel) = self.shell.take() else {
            return false;
        };
        cancel.set();
        true
    }

    /// Put `notice` of a command run in `session` where the model reads it.
    fn shell_ran(&mut self, session: u64, notice: Notice, cx: &mut Context<Self>) {
        let held = (self.busy && self.streaming_id == Some(session))
            || self.runs.contains_key(&session)
            || self.compacting.contains(&session);
        let filled = self
            .store
            .index_of(session)
            .filter(|&ix| !self.store.conversations[ix].messages.is_empty());
        match filled.filter(|_| !held) {
            // In the session at once, so it shows where it was run.
            Some(ix) => {
                self.store.conversations[ix]
                    .messages
                    .extend(inbox::fold(&[notice]));
                self.save();
            }
            // A reply under way, or a session not read in: it goes in with
            // the next message, as anything that came in meanwhile does.
            None => {
                inbox::push(session, notice);
                self.note =
                    Some("Done. What it printed goes to the model with your next message.".into());
            }
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::{ran, shell_command};

    #[test]
    fn a_line_starting_with_a_bang_is_a_command_and_says_what_it_printed() {
        assert_eq!(shell_command("!git status"), Some("git status"));
        assert_eq!(shell_command("!  ls -la "), Some("ls -la"));
        assert_eq!(shell_command("!"), Some(""));
        assert_eq!(shell_command("git status!"), None);
        let notice = ran("git status", "On branch main");
        assert_eq!(
            notice.text,
            "The user ran `git status` in the window:\nOn branch main"
        );
        let folded = letronna_core::inbox::fold(&[notice]).unwrap();
        assert!(folded.notice);
    }
}
