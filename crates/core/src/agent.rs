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
use crate::gate::Outcome;
use crate::tools;
use std::collections::VecDeque;

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
/// The answer a call gets when the turn is stopped while it runs; the front
/// end that stopped it records it before `Turn::stop`. The call may have
/// done part of its work, so the model is told to look first.
pub const STOPPED_WHILE_RUNNING: &str = "Stopped while it ran; check before retrying.";

/// The answer a call gets when the turn is stopped before it starts.
pub const STOPPED_BEFORE_START: &str = "Not run: stopped before it started.";

/// What the caller should do next.
#[derive(Debug, PartialEq)]
pub enum Next {
    /// Send the conversation to the model again.
    Ask,
    /// Run this call. The caller has already decided it may.
    Run(ToolCall),
    /// This call needs the user: an approval, or a question to answer.
    AskUser(ToolCall),
    /// This call may not run, and the reason goes back to the model as its
    /// result with no card shown.
    Refuse(ToolCall, String),
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
    /// Set when the reply being streamed stopped at the output limit: true
    /// when it stopped inside a tool call.
    cut_off: Option<bool>,
    continued: usize,
}

impl Turn {
    pub fn new(connection: Connection) -> Self {
        Self {
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

    /// What to do next, given what the gate makes of the call in front.
    pub fn next(&self, gate: impl Fn(&ToolCall) -> Outcome) -> Next {
        let Some(call) = self.queue.front() else {
            return if self.results.is_empty() {
                Next::Done
            } else {
                Next::Ask
            };
        };
        match gate(call) {
            Outcome::Run => Next::Run(call.clone()),
            Outcome::Ask => Next::AskUser(call.clone()),
            Outcome::Refuse(why) => Next::Refuse(call.clone(), why),
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

    /// What to keep of a stopped turn: the reply streamed so far, marked
    /// interrupted, then the batch's results with every call still queued
    /// answered as never started, so the next request answers every call it
    /// carries.
    pub fn stop(&mut self, model: &str) -> Vec<Message> {
        let mut kept = Vec::new();
        if !self.live.content.trim().is_empty() {
            let mut reply = self.take_reply(model);
            // Calls that came with a reply cut short never ran; kept, each
            // would need an answer of its own.
            reply.tool_calls.clear();
            reply.interrupted = true;
            kept.push(reply);
        }
        while !self.queue.is_empty() {
            self.finish_call(Err(STOPPED_BEFORE_START.into()));
        }
        kept.append(&mut self.results);
        kept
    }
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
        let never = |_: &ToolCall| Outcome::Run;
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
        let gate = |queued: &ToolCall| {
            if queued.name == "write_file" {
                Outcome::Ask
            } else {
                Outcome::Run
            }
        };
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
    fn approving_a_plan_ends_plan_mode_and_declining_keeps_it() {
        use crate::gate::{Gate, Grants, decide};
        use std::collections::HashSet;
        let ctx = tools::ToolContext::default();
        let granted = Grants::from([(0, HashSet::from([tools::EXIT_PLAN.to_owned()]))]);
        let decided = |skip_approvals, human| {
            let gate = Gate {
                ctx: &ctx,
                skip_approvals,
                grants: &granted,
                human,
            };
            decide(&call(tools::EXIT_PLAN), &gate)
        };
        // No standing permission approves a plan; with nobody there it runs,
        // which refuses it.
        assert!(matches!(decided(true, true), Outcome::Ask));
        assert!(matches!(decided(false, false), Outcome::Run));

        let offers = |turn: &Turn, name: &str| {
            tools::specs(&turn.connection)
                .iter()
                .any(|t| t["function"]["name"] == name)
        };
        let none = Grants::new();
        let gate = |queued: &ToolCall| {
            let gate = Gate {
                ctx: &ctx,
                skip_approvals: false,
                grants: &none,
                human: true,
            };
            decide(queued, &gate)
        };
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

    #[test]
    fn a_stop_keeps_the_results_of_the_calls_that_finished() {
        let mut turn = turn();
        turn.begin(vec![call("read_file"), call("list_dir")]);
        turn.finish_call(Ok("contents".into()));
        let kept = turn.stop("test");
        assert_eq!(kept[0].tool_call_id, "read_file-1");
        assert_eq!(kept[0].content, "contents");
        assert!(!kept[0].is_error);
    }

    #[test]
    fn the_call_running_at_a_stop_is_told_to_check_before_retrying() {
        let mut turn = turn();
        turn.begin(vec![call("run_command"), call("list_dir")]);
        // The front end that stopped it answers it as it gives up on it.
        turn.finish_call(Err(STOPPED_WHILE_RUNNING.into()));
        let kept = turn.stop("test");
        assert_eq!(kept[0].tool_call_id, "run_command-1");
        assert_eq!(
            kept[0].content,
            "Stopped while it ran; check before retrying."
        );
        assert!(kept[0].is_error);
        assert_eq!(kept[1].content, STOPPED_BEFORE_START);
    }

    #[test]
    fn calls_the_stop_came_before_are_answered_as_not_run() {
        let mut turn = turn();
        turn.begin(vec![call("write_file"), call("list_dir")]);
        // Stopped at the first call's card: neither ran.
        let kept = turn.stop("test");
        let answers: Vec<&str> = kept.iter().map(|m| m.content.as_str()).collect();
        let not_run = "Not run: stopped before it started.";
        assert_eq!(answers, [not_run, not_run]);
        assert!(kept.iter().all(|m| m.is_error));
    }

    #[test]
    fn a_stop_mid_reply_keeps_what_streamed_marked_interrupted() {
        let mut turn = turn();
        turn.absorb(&Event::Delta("Half an ans".into()));
        turn.absorb(&Event::ToolCalls(vec![call("read_file")]));
        let kept = turn.stop("some-model");
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].role, "assistant");
        assert_eq!(kept[0].content, "Half an ans");
        assert_eq!(kept[0].model, "some-model");
        assert!(kept[0].interrupted);
        // The call that came with it never ran, so there is none to answer.
        assert!(kept[0].tool_calls.is_empty());
        // Nothing streamed yet: nothing to keep.
        assert!(
            Turn::new(turn.connection.clone())
                .stop("some-model")
                .is_empty()
        );
    }

    #[test]
    fn after_a_stop_every_call_the_model_made_has_an_answer() {
        let mut turn = turn();
        let calls = vec![call("read_file"), call("run_command"), call("list_dir")];
        turn.begin(calls.clone());
        turn.finish_call(Ok("contents".into()));
        turn.finish_call(Err(STOPPED_WHILE_RUNNING.into()));
        let kept = turn.stop("test");
        let answered: Vec<&str> = kept.iter().map(|m| m.tool_call_id.as_str()).collect();
        let made: Vec<&str> = calls.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(answered, made);
        assert!(kept.iter().all(|m| m.role == "tool"));
        // Nothing is left to run or send.
        assert_eq!(turn.next(|_| Outcome::Run), Next::Done);
    }
}
