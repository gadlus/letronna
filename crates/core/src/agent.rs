//! The agent loop, with no idea what is driving it.
//!
//! One user message means: call the model, run whatever tools it asks for,
//! call it again with the results, and keep going until it answers in prose.
//! That loop is the same whether a window is drawing approval cards or a
//! terminal is printing `[y/n]`, so it lives here and the frontends only
//! supply the parts that differ: how to reach the user, and how to wait.
//!
//! The shape is a state machine rather than a callback trait on purpose. A
//! window answers an approval later, from a click; a terminal answers it now,
//! from a keypress. Asking the loop to `await` a frontend would force both
//! into whichever model fitted the other worse. Instead the loop says what it
//! needs next and stops, and the caller comes back when it has it.

use crate::chat::{Connection, Event, Message, ToolCall};
use crate::tools;
use std::collections::{HashSet, VecDeque};

/// Tool rounds per user message. Enough for a real investigation, small
/// enough that a looping model stops on its own.
pub const MAX_ROUNDS: usize = 25;

/// Times one user message may ask a reply cut off at the output limit to go
/// on. Three more replies make an answer four times the cap; one still
/// running past that is more likely looping than nearly done.
const MAX_CONTINUATIONS: usize = 3;

/// The notice that asks a reply cut off at the output limit to go on.
const CONTINUE: &str =
    "Your last reply was cut off at the output limit; continue exactly where it stopped.";

/// The notice for a reply cut off inside a tool call, which was not run.
const CALL_AGAIN: &str = "Your last reply was cut off at the output limit in the middle \
    of a tool call, so the call was not run. Make it again with less in it, splitting the \
    work over several calls if it needs more room.";

/// What the caller should do next.
#[derive(Debug, PartialEq)]
pub enum Next {
    /// Send the conversation to the model again.
    Ask,
    /// Run this call. The caller has already decided it may.
    Run(ToolCall),
    /// This call needs the user: an approval, or a question to answer.
    AskUser(ToolCall),
    /// The turn is over.
    Done,
}

/// The reply being streamed in. Kept separate from `Message` so a half-built
/// answer is never mistaken for a finished one.
#[derive(Default, Clone)]
pub struct Live {
    pub content: String,
    pub reasoning: String,
    pub tokens: u64,
    pub per_second: f32,
    pub prompt_tokens: u64,
    pub cached_tokens: u64,
    pub calls: Vec<ToolCall>,
    /// The try about to be made again after a failure, while it waits.
    pub retrying: Option<crate::retry::Retry>,
}

/// One user turn in flight.
pub struct Turn {
    pub connection: Connection,
    pub live: Live,
    rounds: usize,
    limit: usize,
    queue: VecDeque<ToolCall>,
    /// Tool results for the batch being worked through.
    results: Vec<Message>,
    /// What the calls' file writes kept, taken onto each result.
    files: crate::history::Log,
    /// Set when the reply being streamed stopped at the output limit: true
    /// when it stopped inside a tool call.
    cut_off: Option<bool>,
    continued: usize,
}

impl Turn {
    pub fn new(connection: Connection) -> Self {
        Self {
            files: crate::history::Log::new(connection.conversation),
            connection,
            live: Live::default(),
            rounds: 0,
            limit: MAX_ROUNDS,
            queue: VecDeque::new(),
            results: Vec::new(),
            cut_off: None,
            continued: 0,
        }
    }

    /// A turn that may run `rounds` tool rounds instead of `MAX_ROUNDS`.
    pub fn with_limit(connection: Connection, rounds: usize) -> Self {
        Self {
            limit: rounds,
            ..Self::new(connection)
        }
    }

    /// What a call of this turn runs with: its folder and session, and the
    /// log its file writes are noted in.
    pub fn tool_context(&self) -> tools::ToolContext {
        tools::ToolContext {
            files: self.files.clone(),
            ..tools::ToolContext::of(&self.connection)
        }
    }

    /// Fold a streamed event into the reply being built.
    pub fn absorb(&mut self, event: &Event) {
        // Whatever follows a retry comes from the new try, so it is under way.
        self.live.retrying = None;
        match event {
            Event::Delta(text) => self.live.content.push_str(text),
            Event::Reasoning(text) => self.live.reasoning.push_str(text),
            Event::Usage {
                completion,
                per_second,
                prompt,
                cached,
            } => {
                self.live.tokens = *completion;
                self.live.per_second = *per_second;
                self.live.prompt_tokens = *prompt;
                self.live.cached_tokens = *cached;
            }
            Event::ToolCalls(calls) => self.live.calls = calls.clone(),
            Event::Retrying(retry) => self.live.retrying = Some(*retry),
            Event::CutOff { in_call } => self.cut_off = Some(*in_call),
            Event::Finished(_) => {}
        }
    }

    /// The assistant message for the reply just finished, and a clean slate
    /// for the next one.
    pub fn take_reply(&mut self, model: &str) -> Message {
        let live = std::mem::take(&mut self.live);
        Message {
            role: "assistant".into(),
            content: live.content,
            model: model.to_owned(),
            reasoning: live.reasoning,
            tokens: live.tokens,
            per_second: live.per_second,
            prompt_tokens: live.prompt_tokens,
            cached_tokens: live.cached_tokens,
            tool_calls: live.calls,
            ..Default::default()
        }
    }

    /// The notice asking the model to go on, when the reply just finished
    /// was cut off at the output limit. An error once it has been asked
    /// `MAX_CONTINUATIONS` times already, so the turn ends saying why.
    pub fn continuation(&mut self) -> Result<Option<Message>, String> {
        let Some(in_call) = self.cut_off.take() else {
            return Ok(None);
        };
        if self.continued >= MAX_CONTINUATIONS {
            return Err(format!(
                "The reply was still cut off at the output limit after {MAX_CONTINUATIONS} \
                 continuations, so it stops here."
            ));
        }
        self.continued += 1;
        Ok(Some(Message {
            role: "user".into(),
            content: if in_call { CALL_AGAIN } else { CONTINUE }.into(),
            notice: true,
            ..Default::default()
        }))
    }

    /// Queue the calls the model asked for. False when the turn should stop
    /// instead: nothing to run, or it has gone round too many times.
    pub fn begin(&mut self, calls: Vec<ToolCall>) -> bool {
        if calls.is_empty() || self.rounds >= self.limit {
            return false;
        }
        self.rounds += 1;
        self.queue = calls.into();
        self.results.clear();
        true
    }

    /// What to do next, given whether the user has to be consulted first.
    pub fn next(&self, gate: impl Fn(&str) -> bool) -> Next {
        match self.queue.front() {
            None if self.results.is_empty() => Next::Done,
            None => Next::Ask,
            Some(call) if gate(&call.name) => Next::AskUser(call.clone()),
            Some(call) => Next::Run(call.clone()),
        }
    }

    /// Record the outcome of the call at the front of the queue.
    pub fn finish_call(&mut self, outcome: Result<String, String>) {
        let Some(call) = self.queue.pop_front() else {
            return;
        };
        let (content, is_error) = match outcome {
            Ok(out) => (out, false),
            Err(error) => (error, true),
        };
        self.results.push(Message {
            role: "tool".into(),
            content,
            tool_call_id: call.id,
            name: call.name,
            args: call.arguments,
            is_error,
            files: self.files.take(),
            ..Default::default()
        });
    }

    /// The tool results collected for this batch, ready to append before the
    /// next model call.
    pub fn take_results(&mut self) -> Vec<Message> {
        std::mem::take(&mut self.results)
    }

    /// Record the user's answer to the `exit_plan` call at the front of the
    /// queue; approving turns plan mode off for the rest of the turn, so the
    /// tools that carry the plan out are offered and allowed again.
    pub fn answer_plan(&mut self, approved: bool) {
        let answer = if approved {
            self.connection.plan = false;
            tools::PLAN_APPROVED
        } else {
            tools::KEEP_PLANNING
        };
        self.finish_call(Ok(answer.to_owned()));
    }
}

/// Whether a call has to wait for the user.
///
/// `ask_user` always does, whatever else is permitted: it is a question, not
/// an action to allow. `exit_plan` does whenever someone is there to answer,
/// since no standing permission can approve a plan on the user's behalf; with
/// nobody there it goes on to run, and is refused. Everything else is a matter
/// of who has already said yes, and how widely.
pub fn needs_user(name: &str, skip_all: bool, allowed: &HashSet<String>, unattended: bool) -> bool {
    if !tools::needs_approval(name) {
        return false;
    }
    if name == tools::ASK_USER {
        return !unattended;
    }
    if name == tools::EXIT_PLAN {
        return !unattended;
    }
    !(skip_all || unattended || allowed.contains(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: format!("{name}-1"),
            name: name.into(),
            arguments: "{}".into(),
        }
    }

    fn turn() -> Turn {
        Turn::new(Connection {
            endpoint: "http://localhost:11434/v1".into(),
            model: "test".into(),
            key: "k".into(),
            tools: true,
            workspace: None,
            plan: false,
            remote: None,
            effort: String::new(),
            search: false,
            conversation: 0,
            unattended: false,
        })
    }

    /// Counts the rounds a turn runs before it refuses another, giving up at
    /// 1,000 so a turn that never stops fails the test instead of hanging it.
    fn rounds_before_stopping(mut turn: Turn) -> usize {
        for round in 0..1_000 {
            if !turn.begin(vec![call("read_file")]) {
                return round;
            }
            turn.finish_call(Ok("again".into()));
            turn.take_results();
        }
        1_000
    }

    #[test]
    fn streams_a_reply_then_clears_for_the_next_one() {
        let mut turn = turn();
        turn.absorb(&Event::Delta("Hel".into()));
        turn.absorb(&Event::Delta("lo".into()));
        turn.absorb(&Event::Reasoning("thinking".into()));
        turn.absorb(&Event::Usage {
            completion: 12,
            per_second: 4.0,
            prompt: 300,
            cached: 256,
        });

        let reply = turn.take_reply("some-model");
        assert_eq!(reply.content, "Hello");
        assert_eq!(reply.reasoning, "thinking");
        assert_eq!(reply.model, "some-model");
        assert_eq!(reply.cached_tokens, 256);
        // The next reply must not inherit any of that.
        assert!(turn.live.content.is_empty());
        assert_eq!(turn.take_reply("some-model").tokens, 0);
    }

    #[test]
    fn a_retry_is_shown_until_the_next_try_answers() {
        let mut turn = turn();
        let retry = crate::retry::Retry {
            kind: crate::failure::FailureKind::Overloaded,
            attempt: 2,
            of: 6,
            wait: std::time::Duration::from_secs(1),
        };
        turn.absorb(&Event::Retrying(retry));
        assert_eq!(turn.live.retrying, Some(retry));
        turn.absorb(&Event::Reasoning("thinking".into()));
        assert_eq!(turn.live.retrying, None);
    }

    #[test]
    fn runs_a_batch_then_asks_the_model_again() {
        let mut turn = turn();
        let never = |_: &str| false;
        assert_eq!(turn.next(never), Next::Done);

        assert!(turn.begin(vec![call("read_file"), call("list_dir")]));
        assert_eq!(turn.next(never), Next::Run(call("read_file")));
        turn.finish_call(Ok("contents".into()));
        assert_eq!(turn.next(never), Next::Run(call("list_dir")));
        turn.finish_call(Err("no such folder".into()));

        // Batch drained: the results go back to the model.
        assert_eq!(turn.next(never), Next::Ask);
        let results = turn.take_results();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].content, "contents");
        assert!(!results[0].is_error);
        assert!(results[1].is_error);
        // Having handed them over, there is nothing left to send.
        assert_eq!(turn.next(never), Next::Done);
    }

    #[test]
    fn stops_at_a_call_the_user_must_answer() {
        let mut turn = turn();
        turn.begin(vec![call("write_file")]);
        let gate = |name: &str| name == "write_file";
        assert_eq!(turn.next(gate), Next::AskUser(call("write_file")));
        // Still waiting: asking twice must not skip it.
        assert_eq!(turn.next(gate), Next::AskUser(call("write_file")));
        // A denial is an outcome like any other, so the loop moves on.
        turn.finish_call(Err(tools::DECLINED.into()));
        assert_eq!(turn.next(gate), Next::Ask);
    }

    #[test]
    fn a_model_that_keeps_calling_tools_is_eventually_stopped() {
        let mut turn = turn();
        for round in 0..MAX_ROUNDS {
            assert!(turn.begin(vec![call("read_file")]), "round {round}");
            turn.finish_call(Ok("again".into()));
            turn.take_results();
        }
        assert!(!turn.begin(vec![call("read_file")]), "ran past the cap");
        // An empty batch ends the turn rather than looping on nothing.
        let mut empty = Turn::new(turn.connection.clone());
        assert!(!empty.begin(Vec::new()));
    }

    #[test]
    fn a_turn_with_a_higher_limit_goes_further() {
        let turn = Turn::with_limit(turn().connection, 60);
        assert_eq!(rounds_before_stopping(turn), 60);
    }

    #[test]
    fn a_new_turn_still_stops_at_twenty_five() {
        assert_eq!(rounds_before_stopping(turn()), 25);
    }

    #[test]
    fn who_gets_asked_and_who_does_not() {
        let none = HashSet::new();
        // Reading never asks.
        assert!(!needs_user("read_file", false, &none, false));
        // Writing does, until somebody says otherwise.
        assert!(needs_user("write_file", false, &none, false));
        assert!(!needs_user("write_file", true, &none, false));
        assert!(!needs_user("write_file", false, &none, true));
        let allowed = HashSet::from(["write_file".to_owned()]);
        assert!(!needs_user("write_file", false, &allowed, false));
        // A question is never auto-answered, however wide the permission,
        // unless nobody is there to answer it.
        assert!(needs_user(tools::ASK_USER, true, &allowed, false));
        assert!(!needs_user(tools::ASK_USER, false, &none, true));
    }

    #[test]
    fn an_unattended_question_is_answered_by_the_tool() {
        let mut turn = turn();
        turn.begin(vec![call(tools::ASK_USER)]);
        let none = HashSet::new();
        let unattended = |name: &str| needs_user(name, false, &none, true);
        assert_eq!(turn.next(unattended), Next::Run(call(tools::ASK_USER)));
        let answer = tools::run(tools::ASK_USER, "{}", &tools::ToolContext::default()).unwrap();
        assert!(
            answer.starts_with("Nobody is available to answer"),
            "{answer}"
        );
    }

    #[test]
    fn approving_a_plan_ends_plan_mode_and_declining_keeps_it() {
        let plan = HashSet::from([tools::EXIT_PLAN.to_owned()]);
        // No standing permission approves a plan; with nobody there it runs,
        // which refuses it.
        assert!(needs_user(tools::EXIT_PLAN, true, &plan, false));
        assert!(!needs_user(tools::EXIT_PLAN, false, &plan, true));

        let offers = |turn: &Turn, name: &str| {
            tools::specs(&turn.connection)
                .iter()
                .any(|t| t["function"]["name"] == name)
        };
        let gate = |name: &str| needs_user(name, false, &HashSet::new(), false);
        let mut turn = turn();
        turn.connection.plan = true;
        assert!(offers(&turn, tools::EXIT_PLAN) && !offers(&turn, "write_file"));

        turn.begin(vec![call(tools::EXIT_PLAN)]);
        assert_eq!(turn.next(gate), Next::AskUser(call(tools::EXIT_PLAN)));
        turn.answer_plan(false);
        assert!(turn.connection.plan);
        let kept = turn.take_results();
        assert_eq!(
            kept[0].content,
            "Keep planning: the user wants changes. Ask what to change, or refine the plan."
        );
        assert!(!kept[0].is_error);

        turn.begin(vec![call(tools::EXIT_PLAN)]);
        turn.answer_plan(true);
        assert!(!turn.connection.plan);
        assert_eq!(
            turn.take_results()[0].content,
            "The user approved this plan. Plan mode is off; carry it out now."
        );
        // The rest of the turn can act on the plan, and has no plan to hand over.
        assert!(offers(&turn, "write_file") && !offers(&turn, tools::EXIT_PLAN));
    }

    #[test]
    fn a_reply_cut_off_at_the_limit_goes_on_three_times_then_stops() {
        let mut turn = turn();
        // A reply that ended on its own asks for nothing more.
        turn.absorb(&Event::Delta("Done.".into()));
        turn.take_reply("some-model");
        assert!(matches!(turn.continuation(), Ok(None)));
        for time in 0..MAX_CONTINUATIONS {
            turn.absorb(&Event::Delta("part".into()));
            turn.absorb(&Event::CutOff { in_call: false });
            // The fragment is kept, not thrown away.
            assert_eq!(turn.take_reply("some-model").content, "part");
            let notice = turn.continuation().unwrap().expect("a continuation");
            assert!(notice.notice, "time {time}");
            assert_eq!(notice.role, "user");
            assert_eq!(notice.content, CONTINUE);
        }
        turn.absorb(&Event::CutOff { in_call: false });
        assert!(turn.continuation().is_err(), "went on past the cap");
    }

    #[test]
    fn a_call_cut_off_at_the_limit_is_asked_for_again() {
        let mut turn = turn();
        turn.absorb(&Event::CutOff { in_call: true });
        let reply = turn.take_reply("some-model");
        assert!(reply.tool_calls.is_empty());
        let notice = turn.continuation().unwrap().expect("a continuation");
        assert!(notice.notice);
        assert_eq!(notice.content, CALL_AGAIN);
        // Taken once: the next reply starts clean.
        assert!(matches!(turn.continuation(), Ok(None)));
    }
}
