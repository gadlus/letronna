//! Auto mode in the window. The judge can take twenty seconds to answer, so
//! a call in front that only the judge would settle goes through
//! `gate::settle` on the background executor while its row reads
//! "checking…", and its verdict carries a number: a stop or a mode change
//! drops the call's number, so a verdict landing after either is not used.
//! Calls that run with no judge asked still run together. What let a call
//! run becomes the tag under its row; what the judge made of it is kept in
//! the session for the Decisions list.

use crate::prelude::*;
use gpui_component::notification::Notification;
use letronna_core::agent;
use letronna_core::chat::ToolCall;
use letronna_core::gate::{self, Gate, Layer, Mode, Outcome};
use letronna_core::judge;

impl Chat {
    /// Puts `call`, in front in Auto mode, to `gate::settle` off the UI
    /// thread, then goes on as it says. A call already being judged is left
    /// to its verdict.
    pub(crate) fn judge_front(&mut self, call: ToolCall, cx: &mut Context<Self>) {
        if self.judging.as_ref().is_some_and(|(id, _)| *id == call.id) {
            return;
        }
        let Some(turn) = self.turn.as_ref() else {
            return;
        };
        let ctx = letronna_core::tools::ToolContext::of(&turn.connection);
        let connection = turn.connection.clone();
        let (grants, rules) = (self.session_allow.clone(), self.rules.clone());
        // As `decide` has it: a scheduled run's mode, with nobody to answer.
        let unattended = self.streaming_id.and_then(|id| self.unattended(id));
        self.judged += 1;
        let ticket = (call.id.clone(), self.judged);
        self.judging = Some(ticket.clone());
        // A card up for it goes until the verdict says whether it is needed.
        self.asking = None;
        cx.notify();
        let queued = call.clone();
        let settled = cx.background_executor().spawn(async move {
            let gate = Gate::for_window(&ctx, &grants, &rules, Mode::Auto, unattended);
            gate::settle(&queued, &gate, &connection, &connection.typed)
        });
        cx.spawn(async move |this, cx| {
            let (outcome, layer) = settled.await;
            let _ = this.update(cx, |this, cx| {
                this.settled(ticket, &call, outcome, layer, cx)
            });
        })
        .detach();
    }

    /// Goes on with `call` as its verdict says, unless a stop or a mode
    /// change dropped `ticket` meanwhile.
    fn settled(
        &mut self,
        ticket: (String, u64),
        call: &ToolCall,
        outcome: Outcome,
        layer: Option<Layer>,
        cx: &mut Context<Self>,
    ) {
        if self.judging.as_ref() != Some(&ticket) {
            return;
        }
        self.judging = None;
        let in_front = self
            .turn
            .as_ref()
            .and_then(agent::Turn::front)
            .is_some_and(|front| front.id == call.id);
        if !in_front {
            return;
        }
        if let Some(layer) = &layer {
            self.keep_tag(call, layer);
        }
        let Some(turn) = self.turn.as_ref() else {
            return;
        };
        let next = turn.next(|_| outcome.clone());
        // The judge's reason goes on the card it leaves the call to.
        let why = layer
            .filter(|_| outcome == Outcome::Ask)
            .map(|layer| layer.tag());
        self.go_on(next, why, cx);
        cx.notify();
    }

    /// Keeps the tag of `call`, which runs with others in Auto mode with no
    /// judge asked, for its row.
    pub(crate) fn tag_unjudged(&mut self, call: &ToolCall) {
        let Some(turn) = self.turn.as_ref() else {
            return;
        };
        let ctx = letronna_core::tools::ToolContext::of(&turn.connection);
        let unattended = self.streaming_id.and_then(|id| self.unattended(id));
        let gate = Gate::for_window(
            &ctx,
            &self.session_allow,
            &self.rules,
            Mode::Auto,
            unattended,
        );
        if let Some(layer) = gate::reason(call, &gate) {
            self.keep_tag(call, &layer);
        }
    }

    /// Keeps `layer`'s tag for the row `call`'s result will have: the
    /// judge's in the session, for the Decisions list, and the rest until
    /// the window closes. A block also gets a toast.
    fn keep_tag(&mut self, call: &ToolCall, layer: &Layer) {
        let Some(ix) = self.streaming_id.and_then(|id| self.store.index_of(id)) else {
            return;
        };
        let conversation = &mut self.store.conversations[ix];
        let summary = letronna_core::tools::summary(&call.name, &call.arguments);
        let shown = format!("{}({summary})", call.name);
        let decision = Judged::of(&conversation.messages, &call.id, shown, layer.tag());
        let Layer::Judge(verdict) = layer else {
            self.tags.entry(conversation.id).or_default().push(decision);
            return;
        };
        if let judge::Verdict::Block(category) = verdict {
            self.blocked = Some(format!(
                "Auto mode blocked {}: {}.",
                call.name,
                category.meaning()
            ));
        }
        conversation.decisions.push(decision);
    }

    /// Tells the judge the user stopped the call in front while it was
    /// judged or ran, so until they type again the same call asks them. A
    /// verdict still on its way is dropped, and a call stopped while judged
    /// is "stopped" in the Decisions list. Called before `keep_stopped`
    /// answers the queue, while the call is still in front.
    pub(crate) fn stop_judge(&mut self) {
        let judging = self.judging.take().is_some();
        let Some(turn) = self.turn.as_ref() else {
            return;
        };
        // A call waiting on its card was not under way.
        let Some(call) = turn
            .front()
            .filter(|call| self.asking.as_deref() != Some(call.id.as_str()))
            .cloned()
        else {
            return;
        };
        let id = turn.connection.conversation;
        judge::stop(&call, &turn.connection.typed, id);
        let Some(ix) = self.store.index_of(id).filter(|_| judging) else {
            return;
        };
        let conversation = &mut self.store.conversations[ix];
        let summary = letronna_core::tools::summary(&call.name, &call.arguments);
        let shown = format!("{}({summary})", call.name);
        let stopped = Judged::of(&conversation.messages, &call.id, shown, "stopped".into());
        conversation.decisions.push(stopped);
    }

    /// Shows the toast for a call the judge blocked, once.
    pub(crate) fn toast_block(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(note) = self.blocked.take() {
            window.push_notification(Notification::warning(note), cx);
        }
    }

    /// The tag under the tool row at `index` in the session on screen.
    pub(crate) fn row_tag(&self, index: usize) -> Option<String> {
        let conversation = &self.store.conversations[self.store.active];
        let made = self
            .tags
            .get(&conversation.id)
            .map_or(&[][..], Vec::as_slice);
        let messages = &conversation.messages;
        tag_at(made, messages, index)
            .or_else(|| tag_at(&conversation.decisions, messages, index))
            .map(str::to_owned)
    }

    /// Opens the list of what the judge made of the calls in the session at
    /// `index`.
    pub(crate) fn show_decisions(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Listed or let go of, its decisions are still on disk.
        self.fill_now(index);
        let conversation = &self.store.conversations[index];
        let none = none_yet(self.mode(conversation.id));
        let rows = rows(&conversation.decisions);
        window.open_dialog(cx, move |dialog, _, cx| {
            let muted = cx.theme().muted_foreground;
            let list = rows.iter().fold(v_flex().gap_2(), |list, (call, tag)| {
                list.child(
                    v_flex()
                        .min_w_0()
                        .child(div().font_family(MONO_FONT).truncate().child(call.clone()))
                        .child(div().text_color(muted).child(tag.clone())),
                )
            });
            dialog
                .title(div().font_family(HEADING_FONT).child("Decisions"))
                .w(px(520.))
                .child(
                    div()
                        .id("decisions")
                        .max_h(px(360.))
                        .overflow_y_scroll()
                        .text_xs()
                        .when(rows.is_empty(), |body| {
                            body.text_color(muted).child(none.clone())
                        })
                        .child(list),
                )
        });
    }
}

/// The tag under the tool row at `index` of `messages`: the newest of
/// `decisions` made for its call at its place.
pub(crate) fn tag_at<'a>(
    decisions: &'a [Judged],
    messages: &[Message],
    index: usize,
) -> Option<&'a str> {
    let call_id = &messages.get(index)?.tool_call_id;
    decisions
        .iter()
        .rev()
        .find(|decision| decision.at == index && decision.call_id == *call_id)
        .map(|decision| decision.tag.as_str())
}

/// The Decisions list for a session holding `decisions`: each call the judge
/// decided and its tag, oldest first.
pub(crate) fn rows(decisions: &[Judged]) -> Vec<(String, String)> {
    decisions
        .iter()
        .map(|decision| (decision.call.clone(), decision.tag.clone()))
        .collect()
}

/// What the Decisions list says with nothing in it, for a session in `mode`.
pub(crate) fn none_yet(mode: Mode) -> String {
    let none = "Nothing in this session has gone to the judge yet.";
    if mode == Mode::Auto {
        none.to_owned()
    } else {
        format!("{none} It decides only in Auto mode.")
    }
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{none_yet, rows, tag_at};
    use letronna_core::agent::{STOPPED_BEFORE_START, Turn};
    use letronna_core::chat::{Connection, Message, ToolCall};
    use letronna_core::gate::{Layer, Mode};
    use letronna_core::judge::{Category, Verdict};
    use letronna_core::store::Judged;

    /// A reply from the model making calls with these ids.
    fn calling(ids: &[&str]) -> Message {
        Message {
            role: "assistant".into(),
            tool_calls: ids
                .iter()
                .map(|id| ToolCall {
                    id: (*id).into(),
                    name: "run_command".into(),
                    arguments: "{}".into(),
                })
                .collect(),
            ..Default::default()
        }
    }

    fn result(id: &str) -> Message {
        Message {
            role: "tool".into(),
            tool_call_id: id.into(),
            ..Default::default()
        }
    }

    fn decided(messages: &[Message], id: &str, layer: Layer) -> Judged {
        Judged::of(messages, id, format!("run_command({id})"), layer.tag())
    }

    #[test]
    fn each_layer_reads_as_its_tag_under_the_row() {
        let layers = [
            Layer::NoApproval,
            Layer::Granted,
            Layer::Rule("cargo test".into()),
            Layer::ReadOnly,
            Layer::Workspace,
            Layer::Judge(Verdict::Allow),
            Layer::Judge(Verdict::Block(Category::SendsOut)),
        ];
        let ids = ["a", "b", "c", "d", "e", "f", "g"];
        let mut messages = vec![calling(&ids)];
        let made: Vec<Judged> = ids
            .iter()
            .zip(layers)
            .map(|(id, layer)| decided(&messages, id, layer))
            .collect();
        messages.extend(ids.iter().map(|id| result(id)));
        let tags: Vec<_> = (1..=7).map(|at| tag_at(&made, &messages, at)).collect();
        assert_eq!(
            tags,
            [
                Some("no approval needed"),
                Some("allowed this session"),
                Some("rule: cargo test"),
                Some("only reads"),
                Some("in workspace"),
                Some("auto: allowed"),
                Some("auto: blocked — it sends data out"),
            ]
        );
        assert_eq!(tag_at(&made, &messages, 0), None);
    }

    #[test]
    fn a_row_takes_only_the_tag_made_for_its_own_call_at_its_place() {
        // Models that send no ids use `call_0` in every reply.
        let mut messages = vec![calling(&["call_0"]), result("call_0")];
        messages.push(calling(&["call_0"]));
        let made = [decided(&messages, "call_0", Layer::Judge(Verdict::Allow))];
        messages.push(result("call_0"));
        assert_eq!(tag_at(&made, &messages, 1), None);
        assert_eq!(tag_at(&made, &messages, 3), Some("auto: allowed"));
        // A row at that place that is not the call's result takes nothing.
        let mut other = messages.clone();
        other.truncate(3);
        other.push(Message::default());
        assert_eq!(tag_at(&made, &other, 3), None);
    }

    #[test]
    fn the_decisions_list_holds_the_judges_calls_oldest_first() {
        let messages = [calling(&["a", "b"])];
        let made = [
            decided(&messages, "a", Layer::Judge(Verdict::Allow)),
            decided(
                &messages,
                "b",
                Layer::Judge(Verdict::Block(Category::SendsOut)),
            ),
        ];
        assert_eq!(
            rows(&made),
            [
                ("run_command(a)".to_owned(), "auto: allowed".to_owned()),
                (
                    "run_command(b)".to_owned(),
                    "auto: blocked — it sends data out".to_owned()
                ),
            ]
        );
        assert!(!none_yet(Mode::Auto).contains("Auto mode"));
        assert!(none_yet(Mode::Default).contains("only in Auto mode"));
    }

    #[test]
    fn auto_is_offered_in_the_windows_lists() {
        let offered: Vec<Mode> = crate::ui::composer::offered().collect();
        assert!(offered.contains(&Mode::Auto), "{offered:?}");
        assert_eq!(offered, Mode::OFFERED);
    }

    #[test]
    fn a_call_stopped_while_judged_gets_its_row_and_tag_after_what_finished() {
        let reply = calling(&["a", "b", "c"]);
        let mut turn = Turn::new(Connection::default());
        turn.begin(reply.tool_calls.clone());
        turn.finish_call(Ok("done".into()));
        let mut messages = vec![reply];
        // Kept before the stop answers the queue, as `stop_judge` keeps it.
        let shown = "run_command()".to_owned();
        let made = [Judged::of(&messages, "b", shown, "stopped".into())];
        messages.extend(turn.stop("model"));
        let rows: Vec<_> = messages[1..]
            .iter()
            .map(|row| (row.tool_call_id.as_str(), row.is_error))
            .collect();
        assert_eq!(rows, [("a", false), ("b", true), ("c", true)]);
        assert_eq!(messages[2].content, STOPPED_BEFORE_START);
        assert_eq!(tag_at(&made, &messages, 2), Some("stopped"));
        assert!(turn.front().is_none());
    }
}
