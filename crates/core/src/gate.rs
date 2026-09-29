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
//! rules file, and its caller hands it the user's. The session's `Mode` sits
//! between the rules and what the user allowed: Plan and Don't ask refuse,
//! Bypass runs what the floor and the rules let through, and Accept edits
//! runs changes to the project's own files. Only `settle`, auto mode's way
//! in, waits on the judge.

use crate::chat::{Connection, ToolCall};
use crate::command::{self, Dialect, Risk};
use crate::judge::{self, Verdict};
use crate::permissions::{self, Rule};
use crate::tools::{self, Tool, ToolContext};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

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

/// How much a session lets run without asking, as its mode chip shows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Asks before every change and command; what new installs start in.
    #[default]
    Default,
    /// Runs edits to the project's own files unasked; commands still ask.
    AcceptEdits,
    /// Looks and proposes only: whatever changes things is refused.
    Plan,
    /// Accept edits with a judge for the rest; not offered until the judge
    /// exists, so until then what it would judge asks.
    Auto,
    /// Refuses whatever would ask.
    DontAsk,
    /// Runs whatever the floor and the deny rules let through.
    Bypass,
}

impl Mode {
    /// The modes a person can pick, in the order they are listed.
    pub const OFFERED: [Mode; 5] = [
        Mode::Default,
        Mode::AcceptEdits,
        Mode::Plan,
        Mode::DontAsk,
        Mode::Bypass,
    ];

    /// The name `--mode`, `/mode` and the saved files use.
    pub fn name(self) -> &'static str {
        match self {
            Mode::Default => "default",
            Mode::AcceptEdits => "accept-edits",
            Mode::Plan => "plan",
            Mode::Auto => "auto",
            Mode::DontAsk => "dont-ask",
            Mode::Bypass => "bypass",
        }
    }

    /// The word on the chip.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Default => "Ask",
            Mode::AcceptEdits => "Edits",
            Mode::Plan => "Plan",
            Mode::Auto => "Auto",
            Mode::DontAsk => "Don't ask",
            Mode::Bypass => "Bypass",
        }
    }

    /// What the mode does, in a few words.
    pub fn about(self) -> &'static str {
        match self {
            Mode::Default => "asks before writing or running",
            Mode::AcceptEdits => "edits in the project run; commands still ask",
            Mode::Plan => "reads and searches only; never writes or runs",
            Mode::Auto => "edits in the project run; the rest asks",
            Mode::DontAsk => "runs only what is allowed; refuses what would ask",
            Mode::Bypass => "runs without asking, bar risky calls and your rules",
        }
    }

    /// The offered mode `text` names, by its name or its chip's word.
    pub fn parse(text: &str) -> Result<Mode, String> {
        let text = text.trim();
        let named = |mode: &Mode| {
            text.eq_ignore_ascii_case(mode.name()) || text.eq_ignore_ascii_case(mode.label())
        };
        if let Some(mode) = Mode::OFFERED.into_iter().find(named) {
            return Ok(mode);
        }
        if named(&Mode::Auto) {
            return Err("Auto mode is not available yet.".into());
        }
        let names: Vec<&str> = Mode::OFFERED.iter().map(|mode| mode.name()).collect();
        Err(format!(
            "There is no `{text}` mode; pick one of {}.",
            names.join(", ")
        ))
    }

    /// The mode a helper started from a session in `parent` runs in, when
    /// it asks for `wanted`: never more than the parent lets through, and
    /// the parent's own when one is Plan and the other Don't ask.
    pub fn narrow(parent: Mode, wanted: Mode) -> Mode {
        // Neither of those is inside the other: Plan asks for a fetch that
        // Don't ask refuses, and Don't ask runs an allowed write that Plan
        // refuses.
        if matches!(
            (parent, wanted),
            (Mode::Plan, Mode::DontAsk) | (Mode::DontAsk, Mode::Plan)
        ) {
            return parent;
        }
        // Helpers run unseen, several at once, so they never bypass. A
        // Bypass parent's are to run in Auto once the judge lands (auto-mode
        // step 11); until then they run in Default.
        let ceiling = if parent == Mode::Bypass {
            Mode::Default
        } else {
            parent
        };
        if wanted.rank() < ceiling.rank() {
            wanted
        } else {
            ceiling
        }
    }

    /// Where the mode sits, from what lets the least through to the most.
    fn rank(self) -> u8 {
        match self {
            Mode::Plan => 0,
            Mode::DontAsk => 1,
            Mode::Default => 2,
            Mode::AcceptEdits => 3,
            Mode::Auto => 4,
            Mode::Bypass => 5,
        }
    }
}

impl Serialize for Mode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for Mode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // The switches the mode replaced, "Skip tool approvals" and a
        // schedule's `unattended`, were saved as true or false, and on meant
        // what Bypass does. A name from a newer Letronna reads as Default
        // rather than failing the whole file it is in, and so does null.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Saved {
            Switch(bool),
            Name(String),
            Null,
        }
        Ok(match Saved::deserialize(deserializer)? {
            Saved::Null => Mode::Default,
            Saved::Switch(true) => Mode::Bypass,
            Saved::Switch(false) => Mode::Default,
            Saved::Name(name) => Mode::OFFERED
                .into_iter()
                .chain([Mode::Auto])
                .find(|mode| mode.name() == name)
                .unwrap_or_default(),
        })
    }
}

/// Drops `old`, the switch the mode replaced, from a saved `object` that
/// also holds `mode`, which is the newer of the two.
pub(crate) fn keep_mode(object: &mut Value, old: &str) {
    // serde refuses a field given twice under its name and its alias, and
    // a file that fails to read is started afresh, losing everything else.
    if let Some(object) = object
        .as_object_mut()
        .filter(|object| object.contains_key("mode"))
    {
        object.remove(old);
    }
}

/// Everything besides the call itself that decides it.
pub struct Gate<'a> {
    /// The session the call is made in: its folder, plan mode and the chat
    /// app driving it, if one is.
    pub ctx: &'a ToolContext,
    /// How much runs without asking: the session's chip, `--mode` or the
    /// mode of the schedule it runs.
    pub mode: Mode,
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
    /// The window's gate for a session in `mode`, or, while it runs a
    /// schedule that decides without the user, in `unattended`'s mode with
    /// nobody to answer.
    pub fn for_window(
        ctx: &'a ToolContext,
        grants: &'a Grants,
        rules: &'a Result<Vec<Rule>, String>,
        mode: Mode,
        unattended: Option<Mode>,
    ) -> Self {
        Self {
            ctx,
            mode: unattended.unwrap_or(mode),
            grants,
            rules: rules.as_deref().unwrap_or_default(),
            broken: rules.as_ref().err().map(String::as_str),
            human: unattended.is_none(),
        }
    }

    /// The terminal's gate, from its mode and whether a card can be seen
    /// and answered.
    pub fn for_terminal(
        ctx: &'a ToolContext,
        grants: &'a Grants,
        rules: &'a Result<Vec<Rule>, String>,
        mode: Mode,
        answerable: bool,
    ) -> Self {
        Self {
            ctx,
            mode,
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
    let judge = |call: &ToolCall, why: &str| judge::judge(call, why, typed, connection, gate.ctx);
    decide_with(call, gate, Some(&judge))
}

/// The judge for a call no earlier layer settles, told why.
type Judge<'a> = &'a dyn Fn(&ToolCall, &str) -> Verdict;

/// `decide`, with the judge for what no earlier layer settles, if there is one.
fn decide_with(call: &ToolCall, gate: &Gate, judge: Option<Judge>) -> Outcome {
    match crate::browser::batch_calls(call) {
        Some(steps) => batched(call, steps, gate, judge),
        None => decide_one(call, gate, judge),
    }
}

/// A batch of browser steps runs only where it would run as a call of its
/// own and each step would run alone: the strictest of those stands. So no
/// step gets past a refusal or a card it would have met by itself -- an
/// upload, an action in another site's frame, a tool not allowed -- and a
/// card, asked for any step, shows them all.
fn batched(
    call: &ToolCall,
    steps: Result<Vec<ToolCall>, String>,
    gate: &Gate,
    judge: Option<Judge>,
) -> Outcome {
    let steps = match steps {
        Ok(steps) => steps,
        Err(why) => return Outcome::Refuse(why),
    };
    let mut outcome = decide_one(call, gate, judge);
    if let Outcome::Refuse(_) = outcome {
        return outcome;
    }
    for (at, step) in steps.iter().enumerate() {
        match decide_one(step, gate, judge) {
            Outcome::Refuse(why) => {
                let what = tools::summary(&step.name, &step.arguments);
                return Outcome::Refuse(format!(
                    "Step {} of the batch ({what}) would be refused on its own, so none of it \
                     ran: {why}",
                    at + 1
                ));
            }
            Outcome::Ask => outcome = Outcome::Ask,
            Outcome::Run => {}
        }
    }
    outcome
}

/// `decide_with` for a call that is not a batch.
fn decide_one(call: &ToolCall, gate: &Gate, judge: Option<Judge>) -> Outcome {
    let judged = judge.map(|judge| move |why: &str| judge(call, why));
    let judged = judged
        .as_ref()
        .map(|judged| judged as &dyn Fn(&str) -> Verdict);
    let outcome = ladder(call, gate, judged);
    // Layer 3's Don't ask turns every card into a refusal, the floor's too.
    // A question, or a plan put for approval, is not an approval to give
    // unasked, so those still ask.
    let question = [tools::ASK_USER, tools::EXIT_PLAN].contains(&call.name.as_str());
    if gate.mode == Mode::DontAsk && outcome == Outcome::Ask && !question {
        return Outcome::Refuse(unasked(&call.name));
    }
    outcome
}

/// `decide_with`'s layers, before Don't ask turns a card into a refusal.
fn ladder(call: &ToolCall, gate: &Gate, judge: Option<&dyn Fn(&str) -> Verdict>) -> Outcome {
    let Some(tool) = tools::find(&call.name) else {
        return Outcome::Refuse(format!("Unknown tool `{}`.", call.name));
    };
    let args = match tools::parse_args(&call.arguments) {
        Ok(args) => args,
        Err(error) => return Outcome::Refuse(error),
    };
    let risk = risk(tool.name(), &args, gate.ctx);
    // Layer 0: what this session cannot reach at all. Plan mode takes the
    // tools that change things away as the plan flag does, so layer 3's
    // refusal for it is made here.
    let plan = gate.ctx.plan || gate.mode == Mode::Plan;
    if let Some(why) = unavailable(tool.as_ref(), risk, gate.ctx, plan) {
        return Outcome::Refuse(why);
    }
    if let Some(why) = marked(tool.as_ref(), &args) {
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
    // Layer 3, the mode: Plan refused at layer 0 and Don't ask refuses in
    // `decide`; Bypass runs whatever got this far.
    if gate.mode == Mode::Bypass {
        return Outcome::Run;
    }
    // Layer 4: what the user allowed in this session or by rule. A yes to
    // using pages is not one to send this computer's files to them, nor to
    // act in a frame another site put in one -- its payment or sign-in
    // form -- so each upload, and each action in such a frame, is asked
    // about, or judged, on its own.
    let granted = gate
        .grants
        .get(&gate.ctx.conversation)
        .is_some_and(|tools| tools.contains(tool.name()));
    let apart = uploads(tool.name(), &args) || in_frame(tool.name(), &args);
    if !opaque && !apart && (granted || ruling.allow) {
        return Outcome::Run;
    }
    // Layer 5: a command that only reads, inside the workspace, needs no
    // more of a yes than `read_file` does.
    if risk == Some(Risk::ReadOnly) {
        return Outcome::Run;
    }
    // Layer 6: an edit that stays among the project's own files.
    let edits = matches!(gate.mode, Mode::AcceptEdits | Mode::Auto);
    if edits && editable(tool.name(), &args, gate.ctx) {
        return Outcome::Run;
    }
    // Then the judge.
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

/// Whether a call to the tool named `name` with `args` sends files from this
/// computer to a web page.
pub(crate) fn uploads(name: &str, args: &Value) -> bool {
    name == "use_page" && args["action"] == "upload"
}

/// Whether a call to the tool named `name` with `args` acts inside a frame
/// from another origin than its page's, as `frame` says; the browser makes
/// sure the element is in a frame from that origin before acting.
pub(crate) fn in_frame(name: &str, args: &Value) -> bool {
    let acts = name == "use_page" || (name == crate::browser::BROWSE && args["action"] == "hover");
    acts && args["frame"]
        .as_str()
        .is_some_and(|frame| !frame.is_empty())
}

/// Whether a yes on the card for a call to `name` with `args` may be kept
/// for the rest of the session: an upload, or an action in another site's
/// frame, is asked about each time, so its card offers no such choice. Nor
/// does a batch's: a yes kept would be one to its tool alone, not to the
/// steps its card showed, which are each decided as they would be alone.
pub fn grants_for_session(name: &str, args: &Value) -> bool {
    let batch = name == "use_page" && args["action"] == "batch";
    !uploads(name, args) && !in_frame(name, args) && !batch
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
    if let Some(why) = unavailable(tool, risk, ctx, ctx.plan) {
        return Some(why);
    }
    if let Some(why) = marked(tool, args) {
        return Some(why);
    }
    match risk {
        Some(Risk::Hardline(why)) => Some(never(why)),
        _ => None,
    }
}

/// Why the session cannot reach `tool` at all, or `None`; `risk` is what
/// its command would do, for `run_command`, and `plan` when the session is
/// in plan mode.
fn unavailable(
    tool: &dyn Tool,
    risk: Option<Risk>,
    ctx: &ToolContext,
    plan: bool,
) -> Option<String> {
    let name = tool.name();
    // Only fitting tools are offered, but the model can name any of them;
    // refuse here too rather than trust the offer.
    if ctx.shared() && tool.machine() {
        return Some(format!(
            "`{name}` is not available here: other people can read this chat, so \
             nothing on this computer is in reach."
        ));
    }
    if plan && tool.mutates() {
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

/// Why a call carrying the marker a hidden key or token became never runs,
/// or `None`: it would write, type or send the marker in the key's place.
/// write_file and edit_file count the file's markers instead, since a file
/// can hold the marker's text itself.
fn marked(tool: &dyn Tool, args: &Value) -> Option<String> {
    let counted = matches!(tool.name(), "write_file" | "edit_file");
    let carried = args.to_string().contains(crate::redact::MARKER);
    (carried && !counted).then(|| crate::redact::carried(tool.name()))
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

/// The refusal for a call that would have asked, in Don't ask mode.
fn unasked(tool: &str) -> String {
    format!(
        "Refused: `{tool}` needs the user's yes, and this session is in Don't \
         ask mode, which refuses instead of asking. Carry on without it and \
         say what was left undone."
    )
}

/// Paths in a project that an edit reaches only with a yes, as `SECRETS`
/// are, matched without case as a run of names anywhere in the path, so
/// that a folder covers everything in it; a trailing `*` takes any rest of
/// the name. Git and its hook managers, CI, a build tool, an editor or
/// launchd runs what they hold, or they hold Letronna's own rules, agents
/// and workflows, other agents' settings (whose hooks run programs) and
/// servers, or keys.
const PROTECTED: &[&str] = &[
    ".git",
    ".gitmodules",
    ".githooks",
    ".husky",
    "lefthook*",
    ".lefthook*",
    ".letronna",
    ".claude",
    ".cursor",
    ".gemini",
    ".codex",
    ".continue",
    ".windsurf",
    ".mcp.json",
    ".devcontainer",
    ".github/workflows",
    ".vscode",
    ".idea",
    ".cargo/config*",
    ".npmrc",
    ".yarnrc*",
    "bunfig.toml",
    ".pre-commit-config.yaml",
    ".env*",
    "Library/LaunchAgents",
];

/// Folders in the home folder's `Library` that cloud drives sync, where a
/// project is as much the user's own as anywhere else.
const SYNCED: &[&str] = &["mobile documents", "cloudstorage"];

/// Whether `write_file` or `edit_file` may change what `args` names without
/// asking: inside the project once links are followed, and neither on a
/// protected path, in the home folder's own settings, nor in Letronna's own
/// folder.
fn editable(name: &str, args: &Value, ctx: &ToolContext) -> bool {
    if !matches!(name, "write_file" | "edit_file") {
        return false;
    }
    let (Some(workspace), Some(raw)) = (&ctx.workspace, args["path"].as_str()) else {
        return false;
    };
    let (Ok(root), Some(path)) = (
        std::fs::canonicalize(workspace),
        landing(&workspace.join(raw)),
    ) else {
        return false;
    };
    let Ok(inside) = path.strip_prefix(&root) else {
        return false;
    };
    let home = tools::home().and_then(|home| std::fs::canonicalize(home).ok());
    !touches_home(&root, &path, home.as_deref())
        && !protected(inside)
        && !tools::private(&path, &crate::chat::data_dir())
        && !linked(&path)
}

/// Whether `path` is a file that other names share, which may lie outside
/// the project: the write changes the file in place, so it changes theirs.
fn linked(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && shared(links(path)))
}

/// Whether a file with `links` names is shared; a count that could not be
/// read counts as shared, so the write asks rather than guessing.
fn shared(links: Option<u64>) -> bool {
    links.is_none_or(|links| links > 1)
}

/// How many names the file at `path` has, or `None` when that cannot be read.
#[cfg(unix)]
fn links(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|meta| meta.nlink())
}

/// How many names the file at `path` has, or `None` when that cannot be
/// read. std's link count is unstable on Windows, so this asks Windows.
#[cfg(windows)]
fn links(path: &Path) -> Option<u64> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    unsafe extern "system" {
        fn GetFileInformationByHandle(file: *mut std::ffi::c_void, info: *mut u32) -> i32;
    }
    // BY_HANDLE_FILE_INFORMATION is thirteen DWORDs: the attributes, three
    // FILETIMEs, the volume, the size's two halves, then the link count.
    const LINKS: usize = 10;
    // No access asked for, as std's own metadata does, so a file the user
    // may not read is still counted.
    let file = std::fs::OpenOptions::new().access_mode(0).open(path).ok()?;
    let mut info = [0u32; 13];
    // SAFETY: `info` is as large and as aligned as the structure the call
    // fills, and `file` keeps the handle open through the call.
    let read = unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) };
    (read != 0).then(|| u64::from(info[LINKS]))
}

/// Whether an edit to `path` in the project at `root` could change what the
/// user's shell, Git, SSH, launchd or apps run: the project is `home`, a
/// folder holding it or a filesystem root, or `path` is among `home`'s
/// dotfiles and dot-folders or in its `Library`.
fn touches_home(root: &Path, path: &Path, home: Option<&Path>) -> bool {
    // Every file there would count as the project's own, `.zshrc` and
    // `.ssh/authorized_keys` among them.
    if root.parent().is_none() || home.is_some_and(|home| home.starts_with(root)) {
        return true;
    }
    let Some(inside) = home.and_then(|home| path.strip_prefix(home).ok()) else {
        return false;
    };
    let mut names = inside.iter().map(fold);
    match names.next().as_deref() {
        Some(first) if first.starts_with('.') => true,
        Some("library") => !names
            .next()
            .is_some_and(|next| SYNCED.contains(&next.as_str())),
        _ => false,
    }
}

/// Where a write to `target` lands with every link followed, or `None` when
/// that cannot be known: a link that points nowhere, which the write would
/// follow, or a `..` after a folder that does not exist yet, which the write
/// would make and then step back out of.
fn landing(target: &Path) -> Option<PathBuf> {
    let mut existing = target;
    let mut rest = Vec::new();
    // `symlink_metadata`, not `exists`, so that a dangling link counts as
    // there and fails to resolve below rather than being written through.
    while std::fs::symlink_metadata(existing).is_err() {
        rest.push(existing.file_name()?);
        existing = existing.parent()?;
    }
    let mut path = std::fs::canonicalize(existing).ok()?;
    path.extend(rest.iter().rev());
    Some(path)
}

/// Whether `path`, taken from the project folder, is or lies in one of
/// `PROTECTED` or `SECRETS`.
fn protected(path: &Path) -> bool {
    let names: Vec<String> = path.iter().map(fold).collect();
    PROTECTED.iter().chain(tools::SECRETS).any(|entry| {
        let parts: Vec<String> = entry.split('/').map(str::to_ascii_lowercase).collect();
        names.windows(parts.len()).any(|run| {
            run.iter()
                .zip(&parts)
                .all(|(name, part)| match part.strip_suffix('*') {
                    Some(start) => name.starts_with(start),
                    None => name == part,
                })
        })
    })
}

/// `name` as the file system finds it, for matching: without case, and
/// without what Windows drops from the end of a name.
fn fold(name: &std::ffi::OsStr) -> String {
    // Lowering alone keeps `ſ` and the Kelvin sign, which macOS opens as
    // `s` and `k`, so `.vscode/taſks.json` would pass for a new file.
    let name = name.to_string_lossy().to_uppercase().to_lowercase();
    // Windows opens `.npmrc.`, `.npmrc ` and `.npmrc::$DATA` as `.npmrc`.
    let name = name.split(':').next().unwrap_or_default();
    name.trim_end_matches(['.', ' ']).to_owned()
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
                            mode: if skip_approvals {
                                Mode::Bypass
                            } else {
                                Mode::Default
                            },
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
                mode: if skip_approvals {
                    Mode::Bypass
                } else {
                    Mode::Default
                },
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
            mode: Mode::Default,
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
            mode: if skip_approvals {
                Mode::Bypass
            } else {
                Mode::Default
            },
            grants: &none,
            human: false,
            rules: &[],
            broken: None,
        };
        let from_a_chat = Gate {
            ctx: &chat,
            mode: Mode::Bypass,
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
            mode: Mode::Default,
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
            mode: Mode::Bypass,
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
                mode: if skip_approvals {
                    Mode::Bypass
                } else {
                    Mode::Default
                },
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
            mode: Mode::Default,
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
                mode: Mode::Default,
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
            // Each mode, picked on the chip or typed after --mode; and an
            // unattended schedule and `-p --yes` fed from a pipe.
            let picked = Mode::OFFERED.map(|mode| (mode, None, mode, true));
            let unattended = (Mode::Default, Some(Mode::Bypass), Mode::Bypass, false);
            for (mode, unattended, typed, answerable) in picked.into_iter().chain([unattended]) {
                let window = Gate::for_window(&ctx, grants, &rules, mode, unattended);
                let terminal = Gate::for_terminal(&ctx, grants, &rules, typed, answerable);
                for queued in &calls {
                    let mut turn = Turn::new(Connection::default());
                    turn.begin(vec![queued.clone()]);
                    assert_eq!(
                        turn.next(|call| decide(call, &window)),
                        turn.next(|call| decide(call, &terminal)),
                        "{} in {mode:?}, unattended {unattended:?}",
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
                mode: if skip_approvals {
                    Mode::Bypass
                } else {
                    Mode::Default
                },
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
            mode: Mode::Bypass,
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
        let allow = |_: &ToolCall, _: &str| Verdict::Allow;
        let deny = |_: &ToolCall, _: &str| Verdict::Block(Category::BeyondRequest);
        let unsure = |_: &ToolCall, _: &str| Verdict::Unsure("no answer".into());
        let unasked = |_: &ToolCall, _: &str| -> Verdict { panic!("the judge was asked") };
        let opaque = |_: &ToolCall, why: &str| {
            assert!(why.contains("hides what it runs"), "{why}");
            Verdict::Allow
        };
        for human in [true, false] {
            let gate = Gate {
                ctx: &ctx,
                mode: Mode::Default,
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
            mode: Mode::Default,
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
                mode: if skip_approvals {
                    Mode::Bypass
                } else {
                    Mode::Default
                },
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
            mode: Mode::Default,
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
            mode: Mode::Default,
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
            mode: Mode::Bypass,
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
                mode: if skip_approvals {
                    Mode::Bypass
                } else {
                    Mode::Default
                },
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
                    mode: if skip_approvals {
                        Mode::Bypass
                    } else {
                        Mode::Default
                    },
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
            mode: Mode::Bypass,
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
            mode: if skip_approvals {
                Mode::Bypass
            } else {
                Mode::Default
            },
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
            let (asking, bypass) = (Mode::Default, Mode::Bypass);
            for (mode, unattended) in [(asking, None), (bypass, None), (asking, Some(bypass))] {
                let gate = Gate::for_window(ctx, &granted, rules, mode, unattended);
                let outcome = decide(&command("cargo test"), &gate);
                if unattended.is_some() {
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

    fn edit(path: &str) -> ToolCall {
        call(
            "write_file",
            serde_json::json!({ "path": path, "content": "x" }),
        )
    }

    /// What `decide` makes of `call` in `mode` with someone there, as the
    /// letter the ladder tests use: R runs, A asks, X is refused.
    fn letter(call: &ToolCall, mode: Mode, ctx: &ToolContext, rules: &[Rule]) -> char {
        let none = Grants::new();
        let gate = Gate {
            ctx,
            mode,
            grants: &none,
            rules,
            broken: None,
            human: true,
        };
        match decide(call, &gate) {
            Outcome::Run => 'R',
            Outcome::Ask => 'A',
            Outcome::Refuse(_) => 'X',
        }
    }

    #[test]
    fn each_mode_runs_asks_or_refuses_each_kind_of_call() {
        let dir = project_dir("ladder");
        let ctx = ToolContext {
            workspace: Some(dir.join("project")),
            ..Default::default()
        };
        let rules = [Rule {
            tool: "run_command".into(),
            pattern: "git push".into(),
            effect: permissions::Effect::Deny,
            project: None,
        }];
        let changed = serde_json::json!({ "path": "notes.txt", "old": "a", "new": "b" });
        // In the order of `Mode::OFFERED`: Default, Accept edits, Plan,
        // Don't ask, Bypass. A line that only reads runs in every mode.
        let table = [
            (command("ls"), "RRRRR"),
            (command("cargo test"), "AAXXR"),
            (edit("src/new.rs"), "ARXXR"),
            (call("edit_file", changed), "ARXXR"),
            (edit(".git/hooks/pre-commit"), "AAXXR"),
            (edit("../outside.txt"), "AAXXR"),
            (command(DANGEROUS), "AAXXA"),
            (command("git push origin main"), "XXXXX"),
        ];
        for (queued, expected) in table {
            let got: String = Mode::OFFERED
                .into_iter()
                .map(|mode| letter(&queued, mode, &ctx, &rules))
                .collect();
            assert_eq!(got, expected, "{}", queued.arguments);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn accept_edits_asks_before_every_protected_path() {
        for path in [
            ".git/config",
            ".GIT/hooks/pre-commit",
            "vendor/lib/.git/config",
            ".git",
            ".letronna/permissions.json",
            ".claude/settings.json",
            ".devcontainer/devcontainer.json",
            ".github/workflows/ci.yml",
            ".vscode/tasks.json",
            ".cargo/config.toml",
            ".npmrc",
            ".npmrc.",
            ".npmrc::$DATA",
            ".yarnrc.yml",
            "bunfig.toml",
            ".pre-commit-config.yaml",
            ".env",
            "app/.env.local",
            ".gitmodules",
            ".githooks/pre-commit",
            ".husky/pre-commit",
            "lefthook.yml",
            ".lefthook-local.yaml",
            ".vscode/settings.json",
            ".vscode/launch.json",
            ".idea/workspace.xml",
            ".mcp.json",
            ".cursor/hooks.json",
            ".gemini/settings.json",
            ".codex/config.toml",
            ".continue/config.yaml",
            ".windsurf/rules/project.md",
            "app/.claude/settings.json",
            ".Cursor/mcp.json",
            ".codex./config.toml",
            ".gemini::$DATA",
            ".ssh/authorized_keys",
            ".aws/credentials",
            "Library/LaunchAgents/agent.plist",
        ] {
            assert!(protected(Path::new(path)), "{path}");
        }
        for path in [
            "src/main.rs",
            "notes.txt",
            ".github/dependabot.yml",
            ".gitignore",
            "docs/husky.md",
            "cargo/config.toml",
            "environment.md",
            "docs/git/config",
        ] {
            assert!(!protected(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn a_name_the_file_system_folds_to_a_protected_one_is_protected() {
        // macOS opens `ſ` as `s` and the Kelvin sign as `k`, and neither is
        // lowered to them by ASCII rules.
        for path in [
            ".vſcode/taſks.json",
            ".github/workflowſ/ci.yml",
            ".github/wor\u{212A}flows/ci.yml",
            "lefthoo\u{212A}.yml",
            "app/.windſurf/hooks.json",
        ] {
            assert!(protected(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn accept_edits_asks_in_the_home_folder_above_it_and_among_its_settings() {
        let home = Path::new("/home/user_a");
        let at = |root: &str, file: &str| {
            let root = Path::new(root);
            touches_home(root, &root.join(file), Some(home))
        };
        for (root, file) in [
            ("/home/user_a", "notes.txt"),
            ("/home", "user_a/.zshrc"),
            ("/", "etc/hosts"),
            ("/home/user_a/.config/nvim", "init.lua"),
            ("/home/user_a/.local/bin", "tool"),
            ("/home/user_a/Library", "LaunchAgents/a.plist"),
            (
                "/home/user_a/Library/Application Support/app",
                "settings.json",
            ),
        ] {
            assert!(at(root, file), "{root} {file}");
        }
        for (root, file) in [
            ("/home/user_a/project", ".zshrc"),
            (
                "/home/user_a/Library/CloudStorage/drive/project",
                "notes.txt",
            ),
            (
                "/home/user_a/Library/Mobile Documents/docs/project",
                "notes.txt",
            ),
            ("/srv/project", "notes.txt"),
        ] {
            assert!(!at(root, file), "{root} {file}");
        }
        // Without a home folder, a filesystem root is still no project.
        assert!(touches_home(Path::new("/"), Path::new("/notes.txt"), None));
        assert!(!touches_home(Path::new("/srv"), Path::new("/srv/a"), None));
    }

    #[test]
    fn accept_edits_asks_before_every_write_when_the_project_is_home() {
        let Some(home) = tools::home() else {
            return;
        };
        let ctx = ToolContext {
            workspace: Some(home),
            ..Default::default()
        };
        for path in [
            "notes.txt",
            ".zshrc",
            ".bash_profile",
            ".profile",
            ".gitconfig",
            ".ssh/authorized_keys",
            ".ssh/config",
            ".config/git/config",
            "Library/LaunchAgents/agent.plist",
        ] {
            assert_eq!(
                letter(&edit(path), Mode::AcceptEdits, &ctx, &[]),
                'A',
                "{path}"
            );
        }
        let root = ToolContext {
            workspace: Some(PathBuf::from("/")),
            ..Default::default()
        };
        assert_eq!(
            letter(&edit("notes.txt"), Mode::AcceptEdits, &root, &[]),
            'A'
        );
    }

    #[test]
    fn accept_edits_asks_when_a_write_would_leave_the_project() {
        let dir = project_dir("links");
        let inside = dir.join("project");
        let ctx = ToolContext {
            workspace: Some(inside.clone()),
            ..Default::default()
        };
        // A folder made on the way and stepped back out of lands in `.git`.
        assert_eq!(
            letter(&edit("new/../.git/config"), Mode::AcceptEdits, &ctx, &[]),
            'A'
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(dir.join("outside.txt"), inside.join("out.txt")).unwrap();
            symlink(&dir, inside.join("up")).unwrap();
            symlink(dir.join("missing.txt"), inside.join("dangling.txt")).unwrap();
            symlink(inside.join(".git"), inside.join("hidden")).unwrap();
            // A hard link canonicalizes inside, but writing it changes the
            // file outside in place.
            std::fs::hard_link(dir.join("outside.txt"), inside.join("linked.txt")).unwrap();
            for path in [
                "out.txt",
                "up/outside.txt",
                "dangling.txt",
                "hidden/config",
                "linked.txt",
            ] {
                assert_eq!(
                    letter(&edit(path), Mode::AcceptEdits, &ctx, &[]),
                    'A',
                    "{path}"
                );
            }
        }
        assert_eq!(
            letter(&edit("notes.txt"), Mode::AcceptEdits, &ctx, &[]),
            'R'
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_file_with_a_second_name_or_an_unknown_count_is_shared() {
        assert!(!shared(Some(1)));
        assert!(shared(Some(2)));
        assert!(shared(None));
        // A real hard link, counted the way this OS counts them.
        let dir = project_dir("counted");
        let file = dir.join("outside.txt");
        assert_eq!(links(&file), Some(1));
        assert!(!linked(&file));
        std::fs::hard_link(&file, dir.join("project/second.txt")).unwrap();
        assert_eq!(links(&file), Some(2));
        assert!(linked(&file));
        assert!(linked(&dir.join("project/second.txt")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn dont_ask_refuses_every_card_but_a_question() {
        let ctx = ToolContext::default();
        let granted = commands_granted();
        let gate = Gate {
            ctx: &ctx,
            mode: Mode::DontAsk,
            grants: &granted,
            rules: &[],
            broken: None,
            human: true,
        };
        let Outcome::Refuse(why) = decide(&write(), &gate) else {
            panic!("a write asked in Don't ask mode");
        };
        assert!(why.contains("Don't ask mode"), "{why}");
        // What the user allowed still runs, and the floor still refuses.
        assert_eq!(decide(&command("cargo test"), &gate), Outcome::Run);
        assert!(matches!(
            decide(&command(DANGEROUS), &gate),
            Outcome::Refuse(_)
        ));
        let question = call(tools::ASK_USER, serde_json::json!({ "question": "Which?" }));
        assert_eq!(decide(&question, &gate), Outcome::Ask);
    }

    #[test]
    fn a_waiting_call_is_decided_again_in_the_new_mode() {
        let dir = project_dir("again");
        let ctx = ToolContext {
            workspace: Some(dir.join("project")),
            ..Default::default()
        };
        let mut turn = Turn::new(Connection::default());
        turn.begin(vec![edit("notes.txt")]);
        let none = Grants::new();
        let next = |mode| {
            let gate = Gate {
                ctx: &ctx,
                mode,
                grants: &none,
                rules: &[],
                broken: None,
                human: true,
            };
            turn.next(|queued| decide(queued, &gate))
        };
        assert!(matches!(next(Mode::Default), Next::AskUser(_)));
        // The card goes and the call runs, or it shows the refusal.
        assert!(matches!(next(Mode::AcceptEdits), Next::Run(_)));
        assert!(matches!(next(Mode::DontAsk), Next::Refuse(..)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn old_switches_read_as_modes_and_modes_save_by_name() {
        use crate::schedule::{Schedule, Trigger};
        use crate::store::Settings;
        let settings = |field: &str| {
            let text = format!(r#"{{"endpoint": "", "model": ""{field}}}"#);
            serde_json::from_str::<Settings>(&text).unwrap().mode
        };
        assert_eq!(settings(r#", "skip_approvals": true"#), Mode::Bypass);
        assert_eq!(settings(r#", "skip_approvals": false"#), Mode::Default);
        assert_eq!(settings(""), Mode::Default);
        assert_eq!(settings(r#", "mode": "accept-edits""#), Mode::AcceptEdits);
        // A mode from a newer Letronna does not cost the rest of the file.
        assert_eq!(settings(r#", "mode": "sideways""#), Mode::Default);
        let saved = serde_json::to_value(Settings {
            mode: Mode::Bypass,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(saved["mode"], "bypass");
        assert!(saved.get("skip_approvals").is_none());

        let schedule = Schedule {
            id: 1,
            name: "nightly".into(),
            prompt: "tidy up".into(),
            workspace: None,
            trigger: Trigger::Every { minutes: 5 },
            enabled: true,
            last_run: 0,
            mode: Mode::DontAsk,
        };
        let mut saved = serde_json::to_value(&schedule).unwrap();
        assert_eq!(saved["mode"], "dont-ask");
        let old = saved.as_object_mut().unwrap();
        old.remove("mode");
        for (unattended, mode) in [(true, Mode::Bypass), (false, Mode::Default)] {
            old.insert("unattended".into(), unattended.into());
            let read: Schedule = serde_json::from_value(old.clone().into()).unwrap();
            assert_eq!(read.mode, mode);
        }
        for mode in Mode::OFFERED.into_iter().chain([Mode::Auto]) {
            let text = serde_json::to_string(&mode).unwrap();
            assert_eq!(serde_json::from_str::<Mode>(&text).unwrap(), mode);
        }
    }

    #[test]
    fn a_file_with_the_mode_and_the_old_switch_keeps_the_mode_and_the_rest() {
        use crate::{schedule, store};
        for text in [
            r#"{"endpoint": "", "model": "m1", "mode": "plan", "skip_approvals": true}"#,
            r#"{"endpoint": "", "model": "m1", "skip_approvals": true, "mode": "plan"}"#,
        ] {
            let read = store::read_settings(text.as_bytes()).unwrap();
            assert_eq!((read.model.as_str(), read.mode), ("m1", Mode::Plan));
        }
        let null = r#"{"endpoint": "", "model": "m1", "mode": null}"#;
        let read = store::read_settings(null.as_bytes()).unwrap();
        assert_eq!((read.model.as_str(), read.mode), ("m1", Mode::Default));
        let old = r#"{"endpoint": "", "model": "m1", "skip_approvals": true}"#;
        assert_eq!(
            store::read_settings(old.as_bytes()).unwrap().mode,
            Mode::Bypass
        );

        let job = |fields: &str| {
            format!(
                r#"{{"id": 1, "name": "nightly", "prompt": "tidy up", "workspace": null,
                "trigger": {{"Every": {{"minutes": 5}}}}{fields}}}"#
            )
        };
        let text = format!(
            "[{}, {}, {}]",
            job(r#", "unattended": true, "mode": "dont-ask""#),
            job(r#", "mode": null"#),
            job(r#", "unattended": true"#)
        );
        let read = schedule::read(text.as_bytes()).unwrap();
        let modes: Vec<Mode> = read.iter().map(|job| job.mode).collect();
        assert_eq!(modes, [Mode::DontAsk, Mode::Default, Mode::Bypass]);
    }

    #[test]
    fn a_helper_never_runs_in_more_than_its_parent_nor_in_bypass() {
        let every = Mode::OFFERED.into_iter().chain([Mode::Auto]);
        for parent in every.clone() {
            for wanted in every.clone() {
                let narrowed = Mode::narrow(parent, wanted);
                assert!(narrowed.rank() <= parent.rank(), "{parent:?} {wanted:?}");
                // Plan and Don't ask are not ranked against each other.
                let unranked = (parent, wanted) == (Mode::DontAsk, Mode::Plan);
                assert!(
                    unranked || narrowed.rank() <= wanted.rank(),
                    "{parent:?} {wanted:?}"
                );
                assert_ne!(narrowed, Mode::Bypass);
            }
        }
        // Auto is not offered yet, so a Bypass parent's helpers ask.
        assert_eq!(Mode::narrow(Mode::Bypass, Mode::Bypass), Mode::Default);
        assert_eq!(Mode::narrow(Mode::Bypass, Mode::Plan), Mode::Plan);
        assert_eq!(
            Mode::narrow(Mode::AcceptEdits, Mode::Bypass),
            Mode::AcceptEdits
        );
        assert_eq!(Mode::narrow(Mode::Plan, Mode::AcceptEdits), Mode::Plan);
        assert_eq!(Mode::narrow(Mode::DontAsk, Mode::Plan), Mode::DontAsk);
        assert_eq!(Mode::narrow(Mode::Plan, Mode::DontAsk), Mode::Plan);
    }

    #[test]
    fn a_schedule_runs_in_its_own_mode_whatever_the_chip_says() {
        let (ctx, none, rules) = (ToolContext::default(), Grants::new(), Ok(Vec::new()));
        for (chip, schedule) in [(Mode::Bypass, Mode::Plan), (Mode::Plan, Mode::Bypass)] {
            let gate = Gate::for_window(&ctx, &none, &rules, chip, Some(schedule));
            assert_eq!(gate.mode, schedule);
            assert!(!gate.human);
        }
        let gate = Gate::for_window(&ctx, &none, &rules, Mode::DontAsk, None);
        assert_eq!(gate.mode, Mode::DontAsk);
        assert!(gate.human);
    }

    #[test]
    fn a_mode_is_picked_by_its_name_or_its_chip_but_auto_is_not_offered() {
        assert_eq!(Mode::parse("accept-edits"), Ok(Mode::AcceptEdits));
        assert_eq!(Mode::parse("Edits"), Ok(Mode::AcceptEdits));
        assert_eq!(Mode::parse(" don't ask "), Ok(Mode::DontAsk));
        assert_eq!(Mode::parse("BYPASS"), Ok(Mode::Bypass));
        assert_eq!(Mode::parse("ask"), Ok(Mode::Default));
        assert!(Mode::parse("auto").unwrap_err().contains("not available"));
        let unknown = Mode::parse("fast").unwrap_err();
        assert!(unknown.contains("accept-edits, plan"), "{unknown}");
    }

    /// A project folder for one test holding `notes.txt` and a `.git`
    /// folder, with a file beside it outside the project.
    fn project_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("letronna-gate-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("project/.git/hooks")).unwrap();
        std::fs::write(dir.join("project/notes.txt"), "a").unwrap();
        std::fs::write(dir.join("outside.txt"), "a").unwrap();
        dir
    }

    #[test]
    fn a_yes_to_using_pages_does_not_cover_sending_files_to_them() {
        let ctx = ToolContext::default();
        let granted = Grants::from([(0, HashSet::from(["use_page".to_owned()]))]);
        let gate = |mode: Mode| Gate {
            ctx: &ctx,
            mode,
            grants: &granted,
            rules: &[],
            broken: None,
            human: true,
        };
        let click = call(
            "use_page",
            serde_json::json!({ "action": "click", "ref": "e1" }),
        );
        let upload = call(
            "use_page",
            serde_json::json!({ "action": "upload", "ref": "e1", "files": ["cv.pdf"] }),
        );
        assert_eq!(decide(&click, &gate(Mode::Default)), Outcome::Run);
        assert_eq!(decide(&upload, &gate(Mode::Default)), Outcome::Ask);
        // Bypass runs it, as it runs everything the floor lets through; plan
        // mode and Don't ask refuse it.
        assert_eq!(decide(&upload, &gate(Mode::Bypass)), Outcome::Run);
        for mode in [Mode::Plan, Mode::DontAsk] {
            assert!(matches!(decide(&upload, &gate(mode)), Outcome::Refuse(_)));
        }
        // So an upload's card offers no yes for the session.
        let args = |call: &ToolCall| serde_json::from_str::<Value>(&call.arguments).unwrap();
        assert!(grants_for_session("use_page", &args(&click)));
        assert!(!grants_for_session("use_page", &args(&upload)));
    }

    #[test]
    fn a_yes_or_an_allow_for_the_page_does_not_cover_acting_in_another_sites_frame() {
        let ctx = ToolContext::default();
        let names = ["use_page".to_owned(), crate::browser::BROWSE.to_owned()];
        let granted = Grants::from([(0, HashSet::from(names))]);
        let allowed = [Rule {
            tool: "use_page".into(),
            pattern: "*".into(),
            effect: permissions::Effect::Allow,
            project: None,
        }];
        let typing = |frame: Option<&str>| {
            let mut args = serde_json::json!({ "action": "type", "ref": "e9", "text": "4242" });
            if let Some(frame) = frame {
                args["frame"] = frame.into();
            }
            call("use_page", args)
        };
        let paying = typing(Some("https://pay.example"));
        let none = Grants::new();
        for (grants, rules) in [(&granted, &[][..]), (&none, &allowed[..])] {
            let gate = |mode: Mode| Gate {
                ctx: &ctx,
                mode,
                grants,
                rules,
                broken: None,
                human: true,
            };
            assert_eq!(decide(&typing(None), &gate(Mode::Default)), Outcome::Run);
            assert_eq!(decide(&paying, &gate(Mode::Default)), Outcome::Ask);
            // Bypass runs it as it runs an upload; plan mode and Don't ask
            // refuse it as they refuse any action on the page.
            assert_eq!(decide(&paying, &gate(Mode::Bypass)), Outcome::Run);
            for mode in [Mode::Plan, Mode::DontAsk] {
                assert!(matches!(decide(&paying, &gate(mode)), Outcome::Refuse(_)));
            }
        }
        // A hover there is asked about too; a look that names a frame is not
        // an action in it.
        let gate = Gate {
            ctx: &ctx,
            mode: Mode::Default,
            grants: &granted,
            rules: &[],
            broken: None,
            human: true,
        };
        let browse = |action: &str| {
            let args = serde_json::json!({ "action": action, "ref": "e9", "frame": "https://pay.example" });
            call(crate::browser::BROWSE, args)
        };
        assert_eq!(decide(&browse("hover"), &gate), Outcome::Ask);
        assert_eq!(decide(&browse("look"), &gate), Outcome::Run);
        let args = |call: &ToolCall| serde_json::from_str::<Value>(&call.arguments).unwrap();
        assert!(grants_for_session("use_page", &args(&typing(None))));
        assert!(!grants_for_session("use_page", &args(&paying)));
        assert!(!grants_for_session(
            crate::browser::BROWSE,
            &args(&browse("hover"))
        ));
    }

    #[test]
    fn a_batch_of_browser_steps_is_decided_as_each_step_would_be_alone() {
        let ctx = ToolContext::default();
        let granted = Grants::from([(0, HashSet::from(["use_page".to_owned()]))]);
        let gate = |mode: Mode| Gate {
            ctx: &ctx,
            mode,
            grants: &granted,
            rules: &[],
            broken: None,
            human: true,
        };
        let batch = |steps: serde_json::Value| {
            call(
                "use_page",
                serde_json::json!({ "action": "batch", "steps": steps }),
            )
        };
        let click = serde_json::json!({ "action": "click", "ref": "e1" });
        let typing = serde_json::json!({ "action": "type", "ref": "e2", "text": "Ada" });
        let upload = serde_json::json!({ "action": "upload", "ref": "e3", "files": ["cv.pdf"] });
        let paying = serde_json::json!({ "action": "type", "ref": "e4", "text": "4242",
                                         "frame": "https://pay.example" });
        let hover = serde_json::json!({ "action": "hover", "ref": "e5" });
        // Steps each allowed alone run together unasked.
        let plain = batch(serde_json::json!([click, typing]));
        assert_eq!(decide(&plain, &gate(Mode::Default)), Outcome::Run);
        // A step that would be asked about alone is asked about in a batch:
        // an upload, an action in another site's frame, and a hover, which
        // is browse's, not the use_page allowed here.
        for asks in [&upload, &paying, &hover] {
            let mixed = batch(serde_json::json!([click, asks]));
            assert_eq!(decide(&mixed, &gate(Mode::Default)), Outcome::Ask);
            // Refused where it would be refused alone, naming the step.
            let Outcome::Refuse(why) = decide(&mixed, &gate(Mode::DontAsk)) else {
                panic!("{asks} got through Don't ask in a batch");
            };
            assert!(why.starts_with("Step 2 of the batch"), "{why}");
        }
        // Its card never offers to stop asking, which would be a yes to
        // use_page alone.
        let args = |call: &ToolCall| serde_json::from_str::<Value>(&call.arguments).unwrap();
        assert!(!grants_for_session("use_page", &args(&plain)));
        // A deny rule on a step's tool, plan mode, and a batch that cannot
        // run are refused before any card.
        let no_browse = [Rule {
            tool: crate::browser::BROWSE.into(),
            pattern: String::new(),
            effect: permissions::Effect::Deny,
            project: None,
        }];
        let denied = Gate {
            rules: &no_browse,
            ..gate(Mode::Default)
        };
        let waits = batch(serde_json::json!([click, { "action": "wait", "text": "Done" }]));
        let refused = decide(&waits, &denied);
        assert!(matches!(refused, Outcome::Refuse(ref why) if why.contains("Step 2")));
        assert!(matches!(
            decide(&plain, &gate(Mode::Plan)),
            Outcome::Refuse(_)
        ));
        let many = batch(serde_json::Value::Array(vec![click.clone(); 21]));
        let Outcome::Refuse(why) = decide(&many, &gate(Mode::Bypass)) else {
            panic!("21 steps ran");
        };
        assert!(why.contains("at most 20"), "{why}");
        let script = batch(serde_json::json!([{ "action": "eval", "script": "1" }]));
        assert!(matches!(
            decide(&script, &gate(Mode::Bypass)),
            Outcome::Refuse(_)
        ));
        // The judge is shown the batch and each step as it would be alone,
        // and one step it blocks refuses the whole.
        let judged = std::cell::RefCell::new(Vec::new());
        let judge = |call: &ToolCall, _: &str| {
            judged.borrow_mut().push(call.name.clone());
            match call.arguments.contains("upload") && call.name == "use_page" {
                true if !call.arguments.contains("batch") => {
                    Verdict::Block(Category::BeyondRequest)
                }
                _ => Verdict::Allow,
            }
        };
        let none = Grants::new();
        let auto = Gate {
            grants: &none,
            ..gate(Mode::Auto)
        };
        let sends = batch(serde_json::json!([hover, upload]));
        let outcome = decide_with(&sends, &auto, Some(&judge));
        assert!(matches!(outcome, Outcome::Refuse(ref why) if why.contains("Step 2")));
        assert_eq!(*judged.borrow(), ["use_page", "browse", "use_page"]);
    }

    #[test]
    fn a_batch_whose_steps_come_as_text_is_decided_on_the_steps_it_runs() {
        let ctx = ToolContext::default();
        let granted = Grants::from([(0, HashSet::from(["use_page".to_owned()]))]);
        let gate = Gate {
            ctx: &ctx,
            mode: Mode::Default,
            grants: &granted,
            rules: &[],
            broken: None,
            human: true,
        };
        // Some models send an array inside a call as its JSON text, which
        // the tool reads as the array before it runs.
        let batch = |steps: serde_json::Value| {
            call(
                "use_page",
                serde_json::json!({ "action": "batch", "steps": steps.to_string() }),
            )
        };
        let click = serde_json::json!({ "action": "click", "ref": "e1" });
        let upload = serde_json::json!({ "action": "upload", "ref": "e3", "files": ["cv.pdf"] });
        let plain = batch(serde_json::json!([click]));
        assert_eq!(decide(&plain, &gate), Outcome::Run);
        let sends = batch(serde_json::json!([click, upload]));
        assert_eq!(decide(&sends, &gate), Outcome::Ask);
    }

    #[test]
    fn a_call_carrying_the_secret_marker_is_refused_before_any_card() {
        let ctx = ToolContext::default();
        let none = Grants::new();
        let gate = Gate {
            ctx: &ctx,
            mode: Mode::Bypass,
            grants: &none,
            rules: &[],
            broken: None,
            human: true,
        };
        let marker = crate::redact::MARKER;
        for carrying in [
            command(&format!("echo '{marker}' > .env")),
            call(
                "use_page",
                serde_json::json!({ "action": "type", "text": marker }),
            ),
            call(
                "web_fetch",
                serde_json::json!({ "url": format!("https://x.test/?k={marker}") }),
            ),
        ] {
            let Outcome::Refuse(why) = decide(&carrying, &gate) else {
                panic!("{} was not refused", carrying.name);
            };
            assert!(why.contains("in the key's place"), "{why}");
        }
        // The two writes count the file's markers instead.
        let kept = serde_json::json!({ "path": "notes.txt", "content": marker });
        assert_eq!(decide(&call("write_file", kept), &gate), Outcome::Run);
    }
}
