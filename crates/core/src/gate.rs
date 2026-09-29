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
//! on its UI thread each time the queue moves; it reads the project's small
//! rules file, and its caller hands it the user's. Only `settle`, auto
//! mode's way in, waits on the judge. Later layers -- the modes -- slot in
//! at the places marked below, in the order the comments give.

use crate::chat::{Connection, ToolCall};
use crate::command::{self, Dialect, Risk};
use crate::judge::{self, Verdict};
use crate::permissions::{self, Rule};
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
    /// The user's rules by argument, for every project; `decide` passes
    /// over those for other projects and adds the project's own file.
    pub rules: &'a [Rule],
    /// Why the user's rules file could not be read, if it could not: what it
    /// denies is unknown, so no approval is taken as given.
    pub broken: Option<&'a str>,
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
        rules: &'a Result<Vec<Rule>, String>,
        skip_setting: bool,
        unattended: bool,
    ) -> Self {
        Self {
            ctx,
            skip_approvals: skip_setting || unattended,
            grants,
            rules: rules.as_deref().unwrap_or_default(),
            broken: rules.as_ref().err().map(String::as_str),
            human: !unattended,
        }
    }

    /// The terminal's gate, from `--yes` and whether a card can be seen and
    /// answered.
    pub fn for_terminal(
        ctx: &'a ToolContext,
        grants: &'a Grants,
        rules: &'a Result<Vec<Rule>, String>,
        yes: bool,
        answerable: bool,
    ) -> Self {
        Self {
            ctx,
            skip_approvals: yes,
            grants,
            rules: rules.as_deref().unwrap_or_default(),
            broken: rules.as_ref().err().map(String::as_str),
            human: answerable,
        }
    }
}

/// Decides `call` from its tool, its arguments and `gate`, layer by layer.
pub fn decide(call: &ToolCall, gate: &Gate) -> Outcome {
    decide_with(call, gate, None)
}

/// `decide` in auto mode: what would ask is put to the judge on the
/// session's own model instead, given `typed`, every message the user typed
/// in the session. It waits on the network, so it never runs on a UI thread.
pub fn settle(call: &ToolCall, gate: &Gate, connection: &Connection, typed: &[String]) -> Outcome {
    // Other people's messages would count as the user's wishes there.
    if gate.ctx.shared() {
        return decide(call, gate);
    }
    let judge = |why: &str| judge::judge(call, why, typed, connection, gate.ctx);
    decide_with(call, gate, Some(&judge))
}

/// `decide`, with the judge for what no earlier layer settles, if there is one.
fn decide_with(call: &ToolCall, gate: &Gate, judge: Option<&dyn Fn(&str) -> Verdict>) -> Outcome {
    let Some(tool) = tools::find(&call.name) else {
        return Outcome::Refuse(format!("Unknown tool `{}`.", call.name));
    };
    let args = match tools::parse_args(&call.arguments) {
        Ok(args) => args,
        Err(error) => return Outcome::Refuse(error),
    };
    let risk = risk(tool.name(), &args, gate.ctx);
    // Layer 0: what this session cannot reach at all.
    if let Some(why) = unavailable(tool.as_ref(), risk, gate.ctx) {
        return Outcome::Refuse(why);
    }
    // Layer 1, the floor. It comes before every setting and grant: a command
    // that is hard to take back needs a person's yes even when approvals are
    // skipped or the tool is allowed.
    if let Some(Risk::Hardline(why)) = risk {
        return Outcome::Refuse(never(why));
    }
    // Layer 2, rules by argument. A deny comes before the floor's asking
    // too, since a yes on that card would let the call run.
    let ruling = permissions::check(gate.rules, tool.as_ref(), &args, gate.ctx);
    if let Some(rule) = &ruling.deny {
        return Outcome::Refuse(denied(rule));
    }
    // A line the floor cannot read, such as `git $(echo push)`, could be
    // what a deny rule names, so a person looks at it as at a risky one.
    if let (Some(Risk::Opaque), Some(rule)) = (risk, &ruling.denies) {
        return dangerous(&unreadable(rule), gate);
    }
    if let Some(Risk::Dangerous(why)) = risk {
        return dangerous(why, gate);
    }
    // Reading keys or passwords sends them to the model's provider, so the
    // floor asks about it too.
    if let Some(why) = secret(tool.as_ref(), &args, gate.ctx) {
        return sensitive(&why, gate);
    }
    if let Some(rule) = &ruling.ask {
        return asked(rule, gate);
    }
    // What the floor cannot see into, such as `python -c` or `$(...)`, could
    // be anything, so a yes to other commands does not cover it.
    let opaque = matches!(risk, Some(Risk::Opaque));
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
    // A rules file that cannot be read could deny this call, so no setting,
    // grant or allow answers for it.
    if let Some(why) = gate.broken.or(ruling.broken.as_deref()) {
        return held(why, tool.name(), gate);
    }
    // Layer 3, the mode's refusals, goes here; skipping approvals is what
    // the mode ladder will call Bypass.
    if gate.skip_approvals {
        return Outcome::Run;
    }
    // Layer 4: what the user allowed in this session or by rule.
    let granted = gate
        .grants
        .get(&gate.ctx.conversation)
        .is_some_and(|tools| tools.contains(tool.name()));
    if !opaque && (granted || ruling.allow) {
        return Outcome::Run;
    }
    // Layer 5: a command that only reads, inside the workspace, needs no
    // more of a yes than `read_file` does.
    if risk == Some(Risk::ReadOnly) {
        return Outcome::Run;
    }
    // Layer 6 (edits inside the workspace) goes here, then the judge.
    if let Some(judge) = judge
        && !spends(tool.name())
    {
        let why = if opaque {
            "the command hides what it runs, so no allowance covers it"
        } else {
            "no rule or allowance covers it"
        };
        return self::ruling(tool.name(), judge(why), gate.human);
    }
    if gate.human {
        return Outcome::Ask;
    }
    Outcome::Refuse(format!(
        "Refused: `{}` needs the user's yes, and nobody is here to give it. \
         Carry on without it and say what was left undone.",
        tool.name()
    ))
}

/// Whether the tool named `name` spends the user's credit, which is theirs
/// to decide rather than a question of safety for the judge.
fn spends(name: &str) -> bool {
    matches!(name, "make_image" | "speak" | "transcribe" | "make_video")
}

/// What the judge's `verdict` on `tool` means for the call.
fn ruling(tool: &str, verdict: Verdict, human: bool) -> Outcome {
    match verdict {
        Verdict::Allow => Outcome::Run,
        Verdict::Block(category) => Outcome::Refuse(format!(
            "Auto mode did not allow this: {}. Do not retry it or reach the \
             same result another way; carry on with other work or ask the user.",
            category.meaning()
        )),
        Verdict::Unsure(_) if human => Outcome::Ask,
        Verdict::Unsure(why) => Outcome::Refuse(format!(
            "Refused: auto mode could not approve `{tool}` ({why}), and nobody is \
             here to decide. Carry on without it and say what was left undone."
        )),
    }
}

/// Why `tool` may not run with `args` in `ctx` whoever answers, or `None`.
pub fn refusal(tool: &dyn Tool, args: &Value, ctx: &ToolContext) -> Option<String> {
    // `tools::run` asks too, for a call that reached it without the gate.
    let risk = risk(tool.name(), args, ctx);
    if let Some(why) = unavailable(tool, risk, ctx) {
        return Some(why);
    }
    match risk {
        Some(Risk::Hardline(why)) => Some(never(why)),
        _ => None,
    }
}

/// Why the session cannot reach `tool` at all, or `None`; `risk` is what
/// its command would do, for `run_command`.
fn unavailable(tool: &dyn Tool, risk: Option<Risk>, ctx: &ToolContext) -> Option<String> {
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
        // A command that only reads looks rather than acts.
        if name == "run_command" {
            return (risk != Some(Risk::ReadOnly)).then(|| {
                "`run_command` in plan mode only runs commands that just read inside \
                 the project, such as `ls`, `cat`, `rg` or `git status`, and this one \
                 could change things. Describe it in the plan instead."
                    .into()
            });
        }
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

/// Why reading what `args` names needs the user's yes, as it holds keys,
/// passwords or tokens; `None` for any other call.
fn secret(tool: &dyn Tool, args: &Value, ctx: &ToolContext) -> Option<String> {
    // A write stays in the project, where `resolve_write` keeps it; it is
    // the reads through `tools::resolve` that could carry a key away.
    if matches!(tool.name(), "write_file" | "edit_file") {
        return None;
    }
    let raw = args["path"].as_str()?;
    tools::holds_secrets(raw, ctx).then(|| format!("`{raw}` holds keys, passwords or tokens"))
}

/// Asks the user at this computer before a read of keys or passwords, or
/// refuses it when they cannot be asked, as `dangerous` does.
fn sensitive(why: &str, gate: &Gate) -> Outcome {
    if !gate.human || gate.ctx.remote.is_some() {
        return Outcome::Refuse(format!(
            "Refused: {why}, and only the user at the computer can allow \
             reading it. Ask the user to paste what you need from it."
        ));
    }
    Outcome::Ask
}

/// The refusal for a call a permission rule denies.
fn denied(rule: &Rule) -> String {
    format!(
        "Refused: a permission rule denies {}. Do not look for another way to \
         do it; say in your answer what was left undone.",
        described(rule)
    )
}

/// What a line that cannot be read does while a permission rule denies some
/// of what its tool can do, as `dangerous` words it.
fn unreadable(rule: &Rule) -> String {
    format!(
        "holds a part only known when it runs, which could be what a \
         permission rule denies ({})",
        described(rule)
    )
}

/// Asks about a call a permission rule says to ask about, or refuses it when
/// nobody can answer.
fn asked(rule: &Rule, gate: &Gate) -> Outcome {
    if gate.human {
        return Outcome::Ask;
    }
    Outcome::Refuse(format!(
        "Refused: a permission rule asks the user first about {}, and nobody \
         is here to answer. Carry on without it and say what was left undone.",
        described(rule)
    ))
}

/// Asks about `tool`'s call while a rules file cannot be read, or refuses it
/// when nobody can answer.
fn held(why: &str, tool: &str, gate: &Gate) -> Outcome {
    if gate.human {
        return Outcome::Ask;
    }
    Outcome::Refuse(format!(
        "Refused: {why}, so `{tool}` needs the user's yes until it is fixed, \
         and nobody is here to give it. Carry on without it and say what was \
         left undone."
    ))
}

/// A rule's tool and pattern as a refusal names them.
fn described(rule: &Rule) -> String {
    if rule.whole() {
        format!("`{}`", rule.tool)
    } else {
        format!("`{}` for `{}`", rule.tool, rule.pattern)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{Next, Turn};
    use crate::chat::{Connection, Remote};
    use crate::judge::Category;

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
                            rules: &[],
                            broken: None,
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
                rules: &[],
                broken: None,
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
            rules: &[],
            broken: None,
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
            rules: &[],
            broken: None,
        };
        let from_a_chat = Gate {
            ctx: &chat,
            skip_approvals: true,
            grants: &none,
            human: true,
            rules: &[],
            broken: None,
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
            rules: &[],
            broken: None,
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
            rules: &[],
            broken: None,
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
                rules: &[],
                broken: None,
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
            rules: &[],
            broken: None,
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
                rules: &[],
                broken: None,
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
        let (ctx, rules) = (ToolContext::default(), Ok(Vec::new()));
        for grants in [&none, &granted] {
            // Asking as usual; "Skip tool approvals" and --yes; and an
            // unattended schedule and `-p --yes` fed from a pipe.
            for (skip, unattended, yes, answerable) in [
                (false, false, false, true),
                (true, false, true, true),
                (false, true, true, false),
            ] {
                let window = Gate::for_window(&ctx, grants, &rules, skip, unattended);
                let terminal = Gate::for_terminal(&ctx, grants, &rules, yes, answerable);
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
                rules: &[],
                broken: None,
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
            rules: &[],
            broken: None,
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

    #[test]
    fn auto_mode_puts_what_would_ask_to_the_judge() {
        let ctx = ToolContext::default();
        let none = Grants::new();
        let allow = |_: &str| Verdict::Allow;
        let deny = |_: &str| Verdict::Block(Category::BeyondRequest);
        let unsure = |_: &str| Verdict::Unsure("no answer".into());
        let unasked = |_: &str| -> Verdict { panic!("the judge was asked") };
        let opaque = |why: &str| {
            assert!(why.contains("hides what it runs"), "{why}");
            Verdict::Allow
        };
        for human in [true, false] {
            let gate = Gate {
                ctx: &ctx,
                skip_approvals: false,
                grants: &none,
                human,
                rules: &[],
                broken: None,
            };
            assert_eq!(decide_with(&write(), &gate, Some(&allow)), Outcome::Run);
            assert_eq!(
                decide_with(&command(OPAQUE), &gate, Some(&opaque)),
                Outcome::Run
            );
            let Outcome::Refuse(why) = decide_with(&write(), &gate, Some(&deny)) else {
                panic!("a denied write was not refused");
            };
            assert!(why.contains("beyond what was asked"), "{why}");
            let unsure = decide_with(&write(), &gate, Some(&unsure));
            assert_eq!(unsure == Outcome::Ask, human, "{unsure:?}");
            // The floor comes first, and spending credit is the user's call.
            let hardline = decide_with(&command(HARDLINE), &gate, Some(&unasked));
            assert!(matches!(hardline, Outcome::Refuse(_)));
            let dangerous = decide_with(&command(DANGEROUS), &gate, Some(&unasked));
            assert_eq!(dangerous, decide(&command(DANGEROUS), &gate));
            let image = call(
                "make_image",
                serde_json::json!({ "prompt": "a lighthouse" }),
            );
            let image = decide_with(&image, &gate, Some(&unasked));
            assert_eq!(image == Outcome::Ask, human, "{image:?}");
        }
    }

    #[test]
    fn a_shared_chat_never_gets_the_judge() {
        let ctx = ToolContext {
            remote: Some(Remote {
                shared: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let none = Grants::new();
        let gate = Gate {
            ctx: &ctx,
            skip_approvals: false,
            grants: &none,
            human: false,
            rules: &[],
            broken: None,
        };
        let fetch = call(
            "web_fetch",
            serde_json::json!({ "url": "https://example.com" }),
        );
        let Outcome::Refuse(why) = settle(&fetch, &gate, &Connection::default(), &[]) else {
            panic!("a fetch nobody can approve was not refused");
        };
        assert!(!why.contains("auto mode"), "{why}");
    }

    /// A session working in a project folder, where a read needs no yes.
    fn project(plan: bool) -> ToolContext {
        ToolContext {
            workspace: Some(std::env::temp_dir().join("letronna-gate-project")),
            plan,
            ..Default::default()
        }
    }

    #[test]
    fn a_command_that_only_reads_runs_without_asking_in_every_mode() {
        let none = Grants::new();
        for (plan, skip_approvals, human) in [
            (false, false, true),
            (false, false, false),
            (false, true, false),
            (true, false, true),
            (true, true, false),
        ] {
            let ctx = project(plan);
            let gate = Gate {
                ctx: &ctx,
                skip_approvals,
                grants: &none,
                human,
                rules: &[],
                broken: None,
            };
            for line in ["git status", "ls; cat notes.txt | wc -l"] {
                assert_eq!(
                    decide(&command(line), &gate),
                    Outcome::Run,
                    "{line:?} with plan {plan}, skip {skip_approvals}, human {human}"
                );
            }
        }
        // A read written to a file is a write, and still asks.
        let ctx = project(false);
        let gate = Gate {
            ctx: &ctx,
            skip_approvals: false,
            grants: &none,
            human: true,
            rules: &[],
            broken: None,
        };
        assert_eq!(
            decide(&command("cat notes.txt > copy.txt"), &gate),
            Outcome::Ask
        );
        // Searching the web sends the query out, so it still asks.
        assert!(crate::search::SearchWeb.needs_approval());
    }

    #[test]
    fn a_read_does_not_carry_a_hardline_or_dangerous_command_with_it() {
        let ctx = project(false);
        let none = Grants::new();
        let gate = Gate {
            ctx: &ctx,
            skip_approvals: false,
            grants: &none,
            human: true,
            rules: &[],
            broken: None,
        };
        let hardline = decide(&command(&format!("ls; {HARDLINE}")), &gate);
        assert!(matches!(hardline, Outcome::Refuse(_)), "{hardline:?}");
        assert_eq!(
            decide(&command(&format!("ls; {DANGEROUS}")), &gate),
            Outcome::Ask
        );
    }

    #[test]
    fn plan_mode_refuses_a_command_that_could_change_things() {
        let ctx = project(true);
        let none = Grants::new();
        let gate = Gate {
            ctx: &ctx,
            skip_approvals: true,
            grants: &none,
            human: true,
            rules: &[],
            broken: None,
        };
        for line in ["cargo test", "cat notes.txt > copy.txt", OPAQUE] {
            let Outcome::Refuse(why) = decide(&command(line), &gate) else {
                panic!("plan mode ran {line:?}");
            };
            assert!(why.contains("plan mode"), "{why}");
        }
    }

    #[test]
    fn a_deny_rule_beats_a_session_grant() {
        let rules = [Rule {
            tool: "run_command".into(),
            pattern: "git push".into(),
            effect: permissions::Effect::Deny,
            project: None,
        }];
        let ctx = ToolContext::default();
        let grants = commands_granted();
        for skip_approvals in [false, true] {
            let gate = Gate {
                ctx: &ctx,
                skip_approvals,
                grants: &grants,
                rules: &rules,
                broken: None,
                human: true,
            };
            for line in [
                "git push origin main",
                "cargo test; git push",
                "sh -c 'git push'",
            ] {
                let outcome = decide(&command(line), &gate);
                assert!(matches!(outcome, Outcome::Refuse(_)), "{line}: {outcome:?}");
            }
            assert_eq!(decide(&command("git status"), &gate), Outcome::Run);
        }
    }

    #[test]
    fn a_deny_rule_holds_through_git_options_and_unreadable_lines() {
        let rules = [Rule {
            tool: "run_command".into(),
            pattern: "git push".into(),
            effect: permissions::Effect::Deny,
            project: None,
        }];
        let (here, chat) = (ToolContext::default(), chat());
        let none = Grants::new();
        // Lines that could be what the rule denies, the last risky as well.
        let unreadable = [
            "env -S 'git push'",
            "git $(echo push)",
            "g=git; $g push",
            OPAQUE,
            "git reset --hard HEAD~1; git $(echo push)",
        ];
        for skip_approvals in [false, true] {
            // Someone at the computer, nobody, and a yes from a chat.
            for (ctx, human, answered) in [
                (&here, true, true),
                (&here, false, false),
                (&chat, true, false),
            ] {
                let gate = Gate {
                    ctx,
                    skip_approvals,
                    grants: &none,
                    rules: &rules,
                    broken: None,
                    human,
                };
                for line in ["git -C . push", "git --no-pager push"] {
                    let outcome = decide(&command(line), &gate);
                    assert!(matches!(outcome, Outcome::Refuse(_)), "{line}: {outcome:?}");
                }
                // Asked about as a risky line is, even with approvals skipped.
                for line in unreadable {
                    let outcome = decide(&command(line), &gate);
                    let fits = if answered {
                        outcome == Outcome::Ask
                    } else {
                        matches!(outcome, Outcome::Refuse(_))
                    };
                    assert!(fits, "{line} ({answered}): {outcome:?}");
                }
            }
        }
        // With no deny on the tool, skipped approvals run it as before.
        let free = Gate {
            ctx: &here,
            skip_approvals: true,
            grants: &none,
            rules: &[],
            broken: None,
            human: true,
        };
        assert_eq!(decide(&command(OPAQUE), &free), Outcome::Run);
    }

    #[test]
    fn a_secret_file_asks_instead_of_refusing() {
        let dir = std::env::temp_dir().join(format!("letronna-gate-secret-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".env"), "TOKEN=x").unwrap();
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        let read = call("read_file", serde_json::json!({ "path": ".env" }));
        let none = Grants::new();
        let gate = |skip_approvals, human| Gate {
            ctx: &ctx,
            skip_approvals,
            grants: &none,
            rules: &[],
            broken: None,
            human,
        };
        // Skipping approvals does not skip this one: the key would leave.
        assert_eq!(decide(&read, &gate(false, true)), Outcome::Ask);
        assert_eq!(decide(&read, &gate(true, true)), Outcome::Ask);
        assert!(matches!(
            decide(&read, &gate(true, false)),
            Outcome::Refuse(_)
        ));
        // Only the yes on the card lets the read through.
        assert!(tools::run(&read.name, &read.arguments, &ctx).is_err());
        let approved = ToolContext {
            approved: true,
            ..ctx.clone()
        };
        let out = tools::run(&read.name, &read.arguments, &approved).unwrap();
        assert!(out.contains("TOKEN=x"), "{out}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_broken_rules_file_holds_every_approval_for_a_person() {
        let granted = commands_granted();
        let broken = Err("permissions.json is not a list of rules".to_owned());
        let dir = std::env::temp_dir().join(format!("letronna-gate-broken-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".letronna")).unwrap();
        std::fs::write(dir.join(".letronna/permissions.json"), "[{").unwrap();
        let (here, project) = (
            ToolContext::default(),
            ToolContext {
                workspace: Some(dir.clone()),
                ..Default::default()
            },
        );
        // The user's file broken, then the project's.
        for (ctx, rules) in [(&here, &broken), (&project, &Ok(Vec::new()))] {
            // Asking as usual, approvals skipped, and an unattended schedule.
            for (skip, unattended) in [(false, false), (true, false), (false, true)] {
                let gate = Gate::for_window(ctx, &granted, rules, skip, unattended);
                let outcome = decide(&command("cargo test"), &gate);
                if unattended {
                    let Outcome::Refuse(why) = outcome else {
                        panic!("ran with nobody there: {outcome:?}");
                    };
                    assert!(why.contains("permissions.json"), "{why}");
                } else {
                    assert_eq!(outcome, Outcome::Ask);
                }
                // A tool that never needs a yes still runs.
                let listing = call("list_dir", serde_json::json!({ "path": "." }));
                assert_eq!(decide(&listing, &gate), Outcome::Run);
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
