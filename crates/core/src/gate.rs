//! Whether a tool call runs, waits for the user or is refused, decided from
//! the whole call.
//!
//! The window and the terminal each asked `agent::needs_user`, which saw
//! only the tool's name, and `tools::run` refused a few calls on its own
//! after that. So a card could come up for a call that was then refused
//! anyway, and with "Skip tool approvals", `--yes` or an unattended schedule
//! nothing looked at a command before it ran. Here one function takes the
//! call with its arguments, the session and whether anyone can answer, and
//! runs the layers in one order for every caller: what is never allowed,
//! then what must ask whatever the setting, then what the user has already
//! allowed, then asking.
//!
//! It is plain code with no network and nothing slow, so the window calls it
//! on its UI thread each time the queue moves. Later layers -- rules by
//! argument, known read-only commands, the modes, the judge -- slot in at
//! the places marked below, in the order the comments give.

use crate::chat::ToolCall;
use crate::command::{self, Dialect, Risk};
use crate::tools::{self, Tool, ToolContext};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// The tools the user stopped being asked about, by the session they said so
/// in; they last until the program closes.
pub type Grants = HashMap<u64, HashSet<String>>;

/// What happens to a tool call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Run it now.
    Run,
    /// Wait for the user: a yes or no, or the answer to a question.
    Ask,
    /// Do not run it; the text is the call's result, which the model reads.
    Refuse(String),
}

/// Everything besides the call itself that decides it.
pub struct Gate<'a> {
    /// The session the call is made in: its folder, plan mode and the chat
    /// app driving it, if one is.
    pub ctx: &'a ToolContext,
    /// Every approval is taken as given: "Skip tool approvals", `--yes` or a
    /// schedule set to run without asking.
    pub skip_approvals: bool,
    /// What the user allowed; only the call's own session counts.
    pub grants: &'a Grants,
    /// Someone can answer right now; false for an unattended schedule and
    /// for `-p` with no terminal to answer in.
    pub human: bool,
}

impl<'a> Gate<'a> {
    /// The window's gate, where an unattended schedule skips every card and
    /// has nobody to answer one.
    pub fn for_window(
        ctx: &'a ToolContext,
        grants: &'a Grants,
        skip_setting: bool,
        unattended: bool,
    ) -> Self {
        Self {
            ctx,
            skip_approvals: skip_setting || unattended,
            grants,
            human: !unattended,
        }
    }

    /// The terminal's gate, from `--yes` and whether a card can be seen and
    /// answered.
    pub fn for_terminal(
        ctx: &'a ToolContext,
        grants: &'a Grants,
        yes: bool,
        answerable: bool,
    ) -> Self {
        Self {
            ctx,
            skip_approvals: yes,
            grants,
            human: answerable,
        }
    }
}

/// Decides `call` from its tool, its arguments and `gate`, layer by layer.
pub fn decide(call: &ToolCall, gate: &Gate) -> Outcome {
    let Some(tool) = tools::find(&call.name) else {
        return Outcome::Refuse(format!("Unknown tool `{}`.", call.name));
    };
    let args = match tools::parse_args(&call.arguments) {
        Ok(args) => args,
        Err(error) => return Outcome::Refuse(error),
    };
    // Layer 0: what this session cannot reach at all.
    if let Some(why) = unavailable(tool.as_ref(), gate.ctx) {
        return Outcome::Refuse(why);
    }
    // Layer 1, the floor. It comes before every setting and grant: a command
    // that is hard to take back needs a person's yes even when approvals are
    // skipped or the tool is allowed.
    let risk = risk(tool.name(), &args, gate.ctx);
    match risk {
        Some(Risk::Hardline(why)) => return Outcome::Refuse(never(why)),
        Some(Risk::Dangerous(why)) => return dangerous(why, gate),
        _ => {}
    }
    // What the floor cannot see into, such as `python -c` or `$(...)`, could
    // be anything, so a yes to other commands does not cover it.
    let opaque = matches!(risk, Some(Risk::Opaque));
    // Layer 2, deny and ask rules by argument, goes here.
    if !tool.needs_approval() {
        return Outcome::Run;
    }
    // A question, or a plan put for approval, is not an action to allow, so
    // no setting or grant answers it. With nobody there the run would wait
    // forever; the tool answers, or refuses.
    if tool.name() == tools::ASK_USER || tool.name() == tools::EXIT_PLAN {
        return if gate.human {
            Outcome::Ask
        } else {
            Outcome::Run
        };
    }
    // Layer 3, the mode's refusals, goes here; skipping approvals is what
    // the mode ladder will call Bypass.
    if gate.skip_approvals {
        return Outcome::Run;
    }
    // Layer 4: what the user allowed in this session. Allow rules by
    // argument join the grants here.
    let granted = gate.grants.get(&gate.ctx.conversation);
    if !opaque && granted.is_some_and(|tools| tools.contains(tool.name())) {
        return Outcome::Run;
    }
    // Layers 5 (known read-only commands), 6 (edits inside the workspace)
    // and the judge go here, in that order.
    if gate.human {
        return Outcome::Ask;
    }
    Outcome::Refuse(format!(
        "Refused: `{}` needs the user's yes, and nobody is here to give it. \
         Carry on without it and say what was left undone.",
        tool.name()
    ))
}

/// Why `tool` may not run with `args` in `ctx` whoever answers, or `None`.
pub fn refusal(tool: &dyn Tool, args: &Value, ctx: &ToolContext) -> Option<String> {
    // `tools::run` asks too, for a call that reached it without the gate.
    if let Some(why) = unavailable(tool, ctx) {
        return Some(why);
    }
    match risk(tool.name(), args, ctx) {
        Some(Risk::Hardline(why)) => Some(never(why)),
        _ => None,
    }
}

/// Why the session cannot reach `tool` at all, or `None`.
fn unavailable(tool: &dyn Tool, ctx: &ToolContext) -> Option<String> {
    let name = tool.name();
    // Only fitting tools are offered, but the model can name any of them;
    // refuse here too rather than trust the offer.
    if ctx.shared() && tool.machine() {
        return Some(format!(
            "`{name}` is not available here: other people can read this chat, so \
             nothing on this computer is in reach."
        ));
    }
    if ctx.plan && tool.mutates() {
        return Some(format!(
            "`{name}` is not available in plan mode: it changes things, and plan \
             mode can only look and propose. Describe the change instead."
        ));
    }
    None
}

/// What `run_command`'s line would do where the session works; `None` for
/// every other tool.
fn risk(name: &str, args: &Value, ctx: &ToolContext) -> Option<Risk> {
    if name != "run_command" {
        return None;
    }
    let line = args["command"].as_str().unwrap_or_default();
    Some(command::assess(line, Dialect::here(), ctx.workspace.as_deref()).risk)
}

/// The refusal for a command that never runs from here.
fn never(why: &str) -> String {
    format!(
        "Refused: this command {why}. Letronna never runs that, whatever the \
         approval setting; if it is really meant, the user can run it \
         themselves, outside Letronna."
    )
}

/// Asks the user at this computer about a command that is hard to take back,
/// or refuses it when they cannot be asked.
fn dangerous(why: &str, gate: &Gate) -> Outcome {
    // With nobody there it would wait forever, and a yes typed into a chat
    // app is a message, not the user at this computer.
    if !gate.human {
        return Outcome::Refuse(format!(
            "Refused: this command {why}, and nobody is here to approve it. \
             Leave it out and say in your answer that it is needed."
        ));
    }
    if gate.ctx.remote.is_some() {
        return Outcome::Refuse(format!(
            "Refused: this command {why}, which needs a yes from the user at \
             the computer, not from a chat. Say what it would do instead; the \
             user can run it there."
        ));
    }
    Outcome::Ask
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{Next, Turn};
    use crate::chat::{Connection, Remote};

    /// Deletes the home folder, in sh and in PowerShell alike.
    const HARDLINE: &str = "Remove-Item -Recurse -Force ~";
    /// Throws away uncommitted work, in sh and in PowerShell alike.
    const DANGEROUS: &str = "git reset --hard HEAD~1";
    /// Runs code the floor cannot read, in sh and in PowerShell alike.
    const OPAQUE: &str = "python -c 'print(1)'";

    fn call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: format!("{name}-1"),
            name: name.into(),
            arguments: args.to_string(),
        }
    }

    fn command(line: &str) -> ToolCall {
        call("run_command", serde_json::json!({ "command": line }))
    }

    fn write() -> ToolCall {
        call(
            "write_file",
            serde_json::json!({ "path": "notes.txt", "content": "x" }),
        )
    }

    /// `run_command` allowed in session 0, where the other tests' calls are made.
    fn commands_granted() -> Grants {
        Grants::from([(0, HashSet::from(["run_command".to_owned()]))])
    }

    /// A session a chat app is driving, which only its owner can read.
    fn chat() -> ToolContext {
        ToolContext {
            remote: Some(Remote {
                place: "a Telegram chat".into(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn the_test_commands_mean_the_same_to_every_shell_here() {
        for dialect in [Dialect::Sh, Dialect::PowerShell] {
            let hardline = command::assess(HARDLINE, dialect, None).risk;
            assert!(matches!(hardline, Risk::Hardline(_)), "{dialect:?}");
            let dangerous = command::assess(DANGEROUS, dialect, None).risk;
            assert!(matches!(dangerous, Risk::Dangerous(_)), "{dialect:?}");
            let opaque = command::assess(OPAQUE, dialect, None).risk;
            assert_eq!(opaque, Risk::Opaque, "{dialect:?}");
        }
    }

    #[test]
    fn a_hardline_command_is_refused_in_every_mode() {
        let none = Grants::new();
        let granted = commands_granted();
        for ctx in [ToolContext::default(), chat()] {
            for skip_approvals in [false, true] {
                for human in [false, true] {
                    for grants in [&none, &granted] {
                        let gate = Gate {
                            ctx: &ctx,
                            skip_approvals,
                            grants,
                            human,
                        };
                        let Outcome::Refuse(why) = decide(&command(HARDLINE), &gate) else {
                            panic!("ran or asked with skip {skip_approvals}, human {human}");
                        };
                        assert!(why.contains("deletes the home folder"), "{why}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_dangerous_command_asks_even_when_approvals_are_skipped() {
        let ctx = ToolContext::default();
        let none = Grants::new();
        let granted = commands_granted();
        for (skip_approvals, grants) in [(false, &none), (true, &none), (true, &granted)] {
            let gate = Gate {
                ctx: &ctx,
                skip_approvals,
                grants,
                human: true,
            };
            assert_eq!(decide(&command(DANGEROUS), &gate), Outcome::Ask);
            // The setting still means what it says for everything else.
            let ordinary = decide(&command("cargo test"), &gate);
            assert_eq!(ordinary == Outcome::Run, skip_approvals);
        }
        // "Stop asking" for the tool does not cover this command either.
        let gate = Gate {
            ctx: &ctx,
            skip_approvals: false,
            grants: &granted,
            human: true,
        };
        assert_eq!(decide(&command(DANGEROUS), &gate), Outcome::Ask);
        assert_eq!(decide(&command("cargo test"), &gate), Outcome::Run);
    }

    #[test]
    fn a_dangerous_command_is_refused_when_nobody_can_answer() {
        let none = Grants::new();
        let here = ToolContext::default();
        let chat = chat();
        // An unattended schedule, `-p` with no terminal with and without
        // --yes, and a chat app, where the yes would come from a message.
        let nobody = |skip_approvals| Gate {
            ctx: &here,
            skip_approvals,
            grants: &none,
            human: false,
        };
        let from_a_chat = Gate {
            ctx: &chat,
            skip_approvals: true,
            grants: &none,
            human: true,
        };
        for (gate, reason) in [
            (nobody(true), "nobody is here"),
            (nobody(false), "nobody is here"),
            (from_a_chat, "not from a chat"),
        ] {
            let mut turn = Turn::new(Connection::default());
            turn.begin(vec![command(DANGEROUS)]);
            // The reason goes back to the model as the call's result.
            let Next::Refuse(_, why) = turn.next(|queued| decide(queued, &gate)) else {
                panic!("a dangerous command was not refused: {reason}");
            };
            assert!(why.contains("throws away uncommitted changes"), "{why}");
            assert!(why.contains(reason), "{why}");
            turn.finish_call(Err(why));
            assert_eq!(turn.next(|queued| decide(queued, &gate)), Next::Ask);
        }
        // A chat app still gets the card for an ordinary change.
        let gate = Gate {
            ctx: &chat,
            skip_approvals: false,
            grants: &none,
            human: true,
        };
        assert_eq!(decide(&write(), &gate), Outcome::Ask);
    }

    #[test]
    fn plan_mode_refuses_a_write_through_the_gate() {
        let ctx = ToolContext {
            plan: true,
            ..Default::default()
        };
        let none = Grants::new();
        // With approvals skipped there is no card to stop it either, so the
        // refusal has to come from the gate, before anything runs.
        let gate = Gate {
            ctx: &ctx,
            skip_approvals: true,
            grants: &none,
            human: true,
        };
        let mut turn = Turn::new(Connection::default());
        turn.begin(vec![
            write(),
            call("read_file", serde_json::json!({ "path": "a" })),
        ]);
        let Next::Refuse(refused, why) = turn.next(|queued| decide(queued, &gate)) else {
            panic!("plan mode let a write through");
        };
        assert_eq!(refused.name, "write_file");
        assert!(why.contains("plan mode"), "{why}");
        turn.finish_call(Err(why));
        // Looking is still allowed.
        assert!(matches!(
            turn.next(|queued| decide(queued, &gate)),
            Next::Run(looked) if looked.name == "read_file"
        ));
    }

    #[test]
    fn an_opaque_command_is_not_covered_by_a_grant() {
        let ctx = ToolContext::default();
        let granted = commands_granted();
        let decided = |skip_approvals, human| {
            let gate = Gate {
                ctx: &ctx,
                skip_approvals,
                grants: &granted,
                human,
            };
            decide(&command(OPAQUE), &gate)
        };
        assert_eq!(decided(false, true), Outcome::Ask);
        assert!(matches!(decided(false, false), Outcome::Refuse(_)));
        // Skipping approvals still runs it: that is the user's own call.
        assert_eq!(decided(true, true), Outcome::Run);
        assert_eq!(decided(true, false), Outcome::Run);
        // The grant still covers a command the floor can read.
        let gate = Gate {
            ctx: &ctx,
            skip_approvals: false,
            grants: &granted,
            human: true,
        };
        assert_eq!(decide(&command("cargo test"), &gate), Outcome::Run);
    }

    #[test]
    fn a_session_grant_allows_only_in_its_session() {
        let grants = Grants::from([(1, HashSet::from(["write_file".to_owned()]))]);
        let in_session = |conversation, human| {
            let ctx = ToolContext {
                conversation,
                ..Default::default()
            };
            let gate = Gate {
                ctx: &ctx,
                skip_approvals: false,
                grants: &grants,
                human,
            };
            decide(&write(), &gate)
        };
        assert_eq!(in_session(1, true), Outcome::Run);
        assert_eq!(in_session(2, true), Outcome::Ask);
        assert!(matches!(in_session(2, false), Outcome::Refuse(_)));
    }

    #[test]
    fn the_window_and_the_terminal_get_the_same_outcome() {
        // Each fills the gate from its own settings through its constructor,
        // as `Chat::decide` and `Runner::work_through` do, then hands `decide`
        // to `Turn::next`; nothing else stands between a call and its outcome.
        let calls = [
            command(HARDLINE),
            command(DANGEROUS),
            command(OPAQUE),
            command("cargo test"),
            write(),
            call("read_file", serde_json::json!({ "path": "a" })),
            call(tools::ASK_USER, serde_json::json!({ "question": "Which?" })),
        ];
        let granted = Grants::from([(
            0,
            HashSet::from(["write_file".to_owned(), "run_command".to_owned()]),
        )]);
        let none = Grants::new();
        let ctx = ToolContext::default();
        for grants in [&none, &granted] {
            // Asking as usual; "Skip tool approvals" and --yes; and an
            // unattended schedule and `-p --yes` fed from a pipe.
            for (skip, unattended, yes, answerable) in [
                (false, false, false, true),
                (true, false, true, true),
                (false, true, true, false),
            ] {
                let window = Gate::for_window(&ctx, grants, skip, unattended);
                let terminal = Gate::for_terminal(&ctx, grants, yes, answerable);
                for queued in &calls {
                    let mut turn = Turn::new(Connection::default());
                    turn.begin(vec![queued.clone()]);
                    assert_eq!(
                        turn.next(|call| decide(call, &window)),
                        turn.next(|call| decide(call, &terminal)),
                        "{} with skip {skip}, unattended {unattended}",
                        queued.name
                    );
                }
            }
        }
    }

    #[test]
    fn who_gets_asked_and_who_does_not() {
        let ctx = ToolContext::default();
        let none = Grants::new();
        let granted = Grants::from([(0, HashSet::from(["write_file".to_owned()]))]);
        let read = call("read_file", serde_json::json!({ "path": "a" }));
        let question = call(tools::ASK_USER, serde_json::json!({ "question": "Which?" }));
        // (call, skip approvals, grants, someone there, what happens)
        let table = [
            // Reading never asks, whoever is there.
            (&read, false, &none, true, "run"),
            (&read, false, &none, false, "run"),
            // Writing does, until somebody says otherwise.
            (&write(), false, &none, true, "ask"),
            (&write(), true, &none, true, "run"),
            (&write(), false, &granted, true, "run"),
            (&write(), true, &none, false, "run"),
            // With nobody to say it, it is refused rather than left waiting.
            (&write(), false, &none, false, "refuse"),
            // A question is never answered by a setting or a grant, only by
            // the tool when nobody is there to answer it.
            (&question, true, &granted, true, "ask"),
            (&question, false, &none, false, "run"),
            (&question, true, &none, false, "run"),
        ];
        for (queued, skip_approvals, grants, human, expected) in table {
            let gate = Gate {
                ctx: &ctx,
                skip_approvals,
                grants,
                human,
            };
            let outcome = match decide(queued, &gate) {
                Outcome::Run => "run",
                Outcome::Ask => "ask",
                Outcome::Refuse(_) => "refuse",
            };
            assert_eq!(
                outcome, expected,
                "{} with skip {skip_approvals}, human {human}",
                queued.name
            );
        }
    }

    #[test]
    fn an_unattended_question_is_answered_by_the_tool() {
        let ctx = ToolContext::default();
        let none = Grants::new();
        let gate = Gate {
            ctx: &ctx,
            skip_approvals: true,
            grants: &none,
            human: false,
        };
        let question = call(tools::ASK_USER, serde_json::json!({ "question": "Which?" }));
        let mut turn = Turn::new(Connection::default());
        turn.begin(vec![question.clone()]);
        assert_eq!(
            turn.next(|queued| decide(queued, &gate)),
            Next::Run(question)
        );
        let answer = tools::run(tools::ASK_USER, "{}", &ctx).unwrap();
        assert!(
            answer.starts_with("Nobody is available to answer"),
            "{answer}"
        );
    }
}
