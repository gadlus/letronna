//! The card the chat shows when a tool needs the user: approve a write or a
//! command, answer an `ask_user` question, or approve a plan. A helper's
//! call gets a card of its own under the helper's name.

use crate::prelude::*;
use crate::ui::actions::{NextTurn, PreviousTurn, Stop};
use letronna_core::approvals::Decision;
use letronna_core::memory::{self, Scope};
use letronna_core::{gate, tools};

/// Eight of a code block's 18px lines: a longer preview scrolls, so the
/// answers under it stay in sight.
const PREVIEW_MAX: Pixels = px(8. * 18.);
/// The most room a plan takes on its card before it scrolls, so its
/// buttons stay in sight.
const PLAN_MAX: Pixels = px(320.);
/// The share of the window's height a plan takes at most, for a short
/// window where `PLAN_MAX` would still push the buttons out of sight.
const PLAN_SHARE: f32 = 0.4;
/// The least room the reason box on a card takes: its whole hint, "Why
/// not? (optional)", with room to spare in each platform's font.
const REASON_MIN: Rems = Rems(12.);

/// What the card for a call waiting on the user shows, worked out once when
/// it comes up: the preview reads the user's keys from disk to hide them,
/// and the card is drawn many times a second while it waits.
pub(crate) struct CardFor {
    call: backend::ToolCall,
    args: serde_json::Value,
    preview: String,
    /// Whether "Allow for this session" is offered.
    for_session: bool,
    /// Where the card stands among its reply's calls that wait on the
    /// user, when that is known.
    place: Option<Place>,
    /// Where a memory save may be kept, where it goes now first; empty for
    /// any other call.
    places: Vec<Scope>,
    /// Letronna's own folder, where the call works when the session has no
    /// project folder; `None` when it has one, or has no such folder.
    fallback: Option<PathBuf>,
    /// For a send_job call, the job and what is typed into it.
    typing: Option<tools::Typing>,
}

impl CardFor {
    pub(crate) fn of(call: &backend::ToolCall, ctx: &tools::ToolContext) -> Self {
        let args = tools::parse_args_for(&call.name, &call.arguments).unwrap_or_default();
        // Read once: it looks up the job and what it printed last.
        let typing = (call.name == "send_job").then(|| tools::typing_of(&args, ctx));
        Self {
            call: call.clone(),
            preview: tools::preview(&call.name, &call.arguments, ctx),
            for_session: gate::grants_for_session(&call.name, &args)
                && !tools::asked_each_time(&call.name),
            args,
            place: None,
            places: match call.name.as_str() {
                "memory" => memory::places(ctx),
                _ => Vec::new(),
            },
            fallback: ctx.workspace.clone().filter(|_| ctx.fallback),
            typing,
        }
    }

    /// The card, numbered at `place` among its reply's.
    pub(crate) fn at(mut self, place: Option<Place>) -> Self {
        self.place = place;
        self
    }

    /// Whether this was worked out for `call`, arguments and all: some
    /// gateways number their calls the same in every reply.
    fn is_for(&self, call: &backend::ToolCall) -> bool {
        self.call.id == call.id
            && self.call.name == call.name
            && self.call.arguments == call.arguments
    }
}

/// Where a card stands among the calls of one reply that wait on the user,
/// for its `2 of 3`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Place {
    /// The reply, by its session and its place among the session's messages.
    reply: (u64, usize),
    /// This card's number, from 1.
    n: usize,
    /// How many of the reply's calls wait on the user: those answered, this
    /// one, and those still to come.
    of: usize,
}

/// Picks a question card's answer by its place, for the letter keys.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = question, no_json)]
pub(crate) struct PickOption(pub(crate) usize);

actions!(approval, [AllowCall, DenyCall]);

/// A key that answers a permission card.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum CardKey {
    Enter,
    Escape,
}

impl CardKey {
    /// The key's name as a key-up event gives it.
    fn name(self) -> &'static str {
        match self {
            Self::Enter => "enter",
            Self::Escape => "escape",
        }
    }
}

/// What a press of a card key comes to.
#[derive(Debug, PartialEq)]
enum Press {
    /// Answer the card.
    Answer,
    /// Do nothing, and keep the key from anything else.
    Swallow,
    /// Leave the key to whatever else it does.
    PassOn,
}

impl Chat {
    /// The answers offered by the question waiting on its card, if one is.
    pub(crate) fn question_options(&self) -> Option<Vec<String>> {
        let turn = self.turn.as_ref()?;
        let (call, waiting) = front_call(turn, self.asking.as_deref())?;
        if !waiting || call.name != tools::ASK_USER {
            return None;
        }
        let args = tools::parse_args_for(&call.name, &call.arguments).unwrap_or_default();
        Some(options_in(&args))
    }

    /// Whether the card waiting on the user takes the keys, as a question's
    /// answers and a permission card do; a plan's card has only buttons.
    pub(crate) fn card_takes_keys(&self) -> bool {
        let Some(turn) = self.turn.as_ref() else {
            return false;
        };
        front_call(turn, self.asking.as_deref())
            .is_some_and(|(call, waiting)| waiting && call.name != tools::EXIT_PLAN)
    }

    /// Answer the question waiting with its answer at `n`, if it has one.
    fn pick_option(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pick) = self
            .question_options()
            .and_then(|options| options.into_iter().nth(n))
        else {
            return;
        };
        // What had the keys is about to go with the card.
        self.focus_composer(window, cx);
        self.resolve_call(Verdict::Answer(pick), cx);
    }

    pub(crate) fn submit_answer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.answer.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        self.answer
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.keys_back(false, window, cx);
        self.resolve_call(Verdict::Answer(text), cx);
    }

    /// Denies the call on the card, with what was typed in its reason field.
    pub(crate) fn deny_call(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let reason = self.take_reason(window, cx);
        self.keys_back(false, window, cx);
        self.resolve_call(Verdict::Deny(reason), cx);
    }

    /// Allows the call on the card this once, a memory save kept where
    /// its first button says.
    fn allow_call(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.take_reason(window, cx);
        self.keys_back(false, window, cx);
        // Pinned to the button Enter is shown on, though the chat's scope
        // may have changed since the card came up.
        let place = self
            .turn
            .as_ref()
            .and_then(|turn| front_call(turn, self.asking.as_deref()))
            .and_then(|(call, _)| self.card_for.as_ref().filter(|shown| shown.is_for(&call)))
            .and_then(|shown| shown.places.first().copied());
        let verdict = match place {
            Some(place) => Verdict::Keep(place),
            None => Verdict::Allow { remember: false },
        };
        self.resolve_call(verdict, cx);
    }

    /// Gives the keys back to the composer when they are on the card about
    /// to go with its answer, put there by Tab, F6 or a click in one of its
    /// boxes, or when `keyboard` says a key pressed one of its buttons.
    fn keys_back(&self, keyboard: bool, window: &mut Window, cx: &mut Context<Self>) {
        let on_card = self.question_focus.contains_focused(window, cx)
            || self.answer.focus_handle(cx).is_focused(window);
        if keyboard || on_card {
            self.focus_composer(window, cx);
        }
    }

    /// Enter: allows the call on a permission card this once, from the card
    /// or from an empty composer; anything else Enter does otherwise.
    pub(crate) fn allow_by_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let takes = self.keys_answer(true, window, cx);
        match self.press(CardKey::Enter, takes) {
            Press::Answer => self.allow_call(window, cx),
            Press::Swallow => {}
            Press::PassOn => cx.propagate(),
        }
    }

    /// Escape: denies the call on a permission card, from the card or from
    /// an empty composer; anything else Escape does otherwise.
    pub(crate) fn deny_by_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let takes = self.keys_answer(false, window, cx);
        match self.press(CardKey::Escape, takes) {
            Press::Answer => self.deny_call(window, cx),
            Press::Swallow => {}
            Press::PassOn => cx.propagate(),
        }
    }

    /// Notes `key` pressed, and what the press comes to when a card would
    /// `take` it now.
    fn press(&mut self, key: CardKey, takes: bool) -> Press {
        let (press, held) = pressed(self.card_key, key, takes);
        self.card_key = held;
        press
    }

    /// A key coming back up ends its being held.
    pub(crate) fn card_key_up(&mut self, event: &KeyUpEvent) {
        if self
            .card_key
            .is_some_and(|(key, _)| key.name() == event.keystroke.key)
        {
            self.card_key = None;
        }
    }

    /// Whether Enter, with `enter`, or Escape answers the call waiting now:
    /// a permission card in sight for the session on screen, with the keys
    /// on the card or in an empty composer, and nothing open that Escape
    /// closes first. Enter does not answer a card asking for a project
    /// folder with no folder of Letronna's own to offer, which has no Allow.
    fn keys_answer(&self, enter: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let here = self.store.conversations[self.store.active].id;
        let open = self.settings_open
            || self.model_open
            || self.context_report.is_some()
            || self.editing_message.is_some()
            || window.has_active_dialog(cx);
        if open
            || self.page != Page::Chat
            || self.streaming_id != Some(here)
            || !self.transcript_shown()
            || self.panel_covers(window, cx)
        {
            return false;
        }
        let Some((call, true)) = self
            .turn
            .as_ref()
            .and_then(|turn| front_call(turn, self.asking.as_deref()))
        else {
            return false;
        };
        if call.name == tools::ASK_USER || call.name == tools::EXIT_PLAN {
            return false;
        }
        let own_folder = (self.card_for.as_ref())
            .filter(|shown| shown.is_for(&call))
            .is_some_and(|shown| shown.fallback.is_some());
        if enter && asks_for_folder(&call.name, self.card_folder().is_some()) && !own_folder {
            return false;
        }
        // Enter on a button of the card presses that button instead.
        let on_card = if enter {
            self.question_focus.is_focused(window)
        } else {
            self.question_focus.contains_focused(window, cx)
        };
        // From the composer, Enter allows only a card the chat is kept at:
        // scrolled up to read, the card can be below the fold, unseen.
        on_card || ((!enter || self.following) && self.composer_idle(window, cx))
    }

    /// Whether the keys are in the composer with nothing in it to send.
    fn composer_idle(&self, window: &Window, cx: &App) -> bool {
        self.composer_empty
            && self.attached().is_empty()
            && self.composer.focus_handle(cx).is_focused(window)
    }

    /// The project folder of the session the card waits in.
    fn card_folder(&self) -> Option<PathBuf> {
        self.streaming_id
            .and_then(|id| self.store.index_of(id))
            .and_then(|ix| self.store.conversations[ix].workspace.clone())
    }

    /// Where the card coming up for `call` stands among the calls of its
    /// reply that wait on the user; None where that cannot be told, as in
    /// Auto mode, whose judge decides such calls off this thread.
    pub(crate) fn card_place(&self, call: &backend::ToolCall) -> Option<Place> {
        if self.turn_mode() == gate::Mode::Auto {
            return None;
        }
        let session = self.streaming_id?;
        let messages = &self.store.conversations[self.store.index_of(session)?].messages;
        let at = messages
            .iter()
            .rposition(|message| message.role == "assistant")?;
        let calls = &messages[at].tool_calls;
        // The queue is what is left of the reply's calls, in their order, so
        // the calls after this one are those still to come.
        let front = calls.iter().position(|queued| {
            queued.id == call.id && queued.name == call.name && queued.arguments == call.arguments
        })?;
        let behind = calls[front + 1..]
            .iter()
            .filter(|queued| self.decide(queued, &self.rules) == gate::Outcome::Ask)
            .count();
        let before = self.card_for.as_ref().and_then(|shown| shown.place);
        Some(place((session, at), before, behind))
    }

    /// Empties the card's reason field and hands back what was in it, so a
    /// reason typed on one card cannot go with the next.
    fn take_reason(&mut self, window: &mut Window, cx: &mut Context<Self>) -> String {
        let reason = self.deny_reason.read(cx).value().to_string();
        self.deny_reason
            .update(cx, |input, cx| input.set_value("", window, cx));
        reason
    }

    /// The row under the messages while tools run: a line per running call,
    /// or the card waiting on the user. `None` when nothing is queued.
    pub(crate) fn approval_card(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let turn = self.turn.as_ref()?;
        let (call, waiting) = front_call(turn, self.asking.as_deref())?;
        let ctx = tools::ToolContext::of(&turn.connection);
        let summary = tools::summary(&call.name, &call.arguments);
        if !waiting {
            // Calls from one reply that run together get a line each, which
            // goes as its result comes back; one Auto mode's judge is still
            // deciding says so.
            let lines = running_calls(turn, self.outstanding)
                .into_iter()
                .map(|call| {
                    let checking = self.judging.as_ref().is_some_and(|(id, _)| *id == call.id);
                    h_flex()
                        .gap_2()
                        .items_center()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            crate::ui::busy::Spinner::new()
                                .xsmall()
                                .color(cx.theme().primary),
                        )
                        .child(call.name.clone())
                        .child(
                            div()
                                .font_family(MONO_FONT)
                                .child(tools::summary(&call.name, &call.arguments)),
                        )
                        .when(checking, |row| row.child("checking…"))
                });
            return Some(v_flex().gap_1().children(lines).into_any_element());
        }

        // Worked out when the card came up; here only if it somehow was not.
        let made;
        let shown = match self.card_for.as_ref().filter(|shown| shown.is_for(&call)) {
            Some(shown) => shown,
            None => {
                made = CardFor::of(&call, &ctx);
                &made
            }
        };
        let args = &shown.args;
        let card = v_flex()
            .w_full()
            .gap_3()
            .p_3()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().primary.opacity(0.5))
            .bg(cx.theme().secondary.opacity(0.3));

        if call.name == tools::ASK_USER {
            let question = args["question"].as_str().unwrap_or_default().to_owned();
            // Its own focus, beside the answer box rather than round it, so
            // the letters pick here and type there.
            let mut chips = h_flex()
                .id("question")
                .track_focus(&self.question_focus)
                .key_context("Question")
                .on_action(cx.listener(|this, PickOption(n): &PickOption, window, cx| {
                    this.pick_option(*n, window, cx)
                }))
                .on_action(cx.listener(|this, _: &PreviousTurn, window, cx| {
                    this.step_turn(Some(Stop::Question), false, window, cx)
                }))
                .on_action(cx.listener(|this, _: &NextTurn, window, cx| {
                    this.step_turn(Some(Stop::Question), true, window, cx)
                }))
                .flex_wrap()
                .gap_2();
            for (i, option) in options_in(args).into_iter().enumerate() {
                let pick = option.clone();
                let mut chip = Button::new(("option", i)).framed(cx).small();
                // Its letter, while the keys are on the answers to press it.
                if let Some(letter) =
                    option_letter(i).filter(|_| self.question_focus.is_focused(window))
                {
                    chip = chip.child(
                        Kbd::new(Keystroke {
                            key: letter.to_string(),
                            ..Default::default()
                        })
                        .text_xs(),
                    );
                }
                chips = chips.child(chip.label(option).on_click(cx.listener(
                    move |this, event: &ClickEvent, window, cx| {
                        this.keys_back(event.is_keyboard(), window, cx);
                        this.resolve_call(Verdict::Answer(pick.clone()), cx)
                    },
                )));
            }
            let question = h_flex()
                .gap_2()
                .items_start()
                .child(div().flex_1().min_w_0().text_sm().child(question))
                .children(place_note(shown.place, cx));
            return Some(
                card.child(question)
                    .child(chips)
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .child(Input::new(&self.answer).small().w_full()),
                            )
                            .child(
                                Button::new("answer")
                                    .primary()
                                    .small()
                                    .label("Reply")
                                    .on_click(cx.listener(
                                        |this, event: &ClickEvent, window, cx| {
                                            this.keys_back(event.is_keyboard(), window, cx);
                                            this.submit_answer(window, cx)
                                        },
                                    )),
                            ),
                    )
                    .into_any_element(),
            );
        }

        if call.name == tools::EXIT_PLAN {
            let plan = shown.preview.clone();
            // Its own scroll state, by call, as every scroll box needs.
            let scroll = window
                .use_keyed_state(
                    SharedString::from(format!("plan-scroll-{}", call.id)),
                    cx,
                    |_, _| ScrollHandle::default(),
                )
                .read(cx)
                .clone();
            let most = PLAN_MAX.min(window.viewport_size().height * PLAN_SHARE);
            return Some(
                card.child(
                    h_flex()
                        .gap_2()
                        .items_start()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .font_family(BODY_FONT)
                                .font_weight(FontWeight::MEDIUM)
                                .child("Letronna has a plan"),
                        )
                        .children(place_note(shown.place, cx)),
                )
                .child(
                    // A long plan scrolls rather than pushing the buttons out
                    // of sight; a short one takes only the room it needs.
                    div()
                        .relative()
                        .w_full()
                        .child(
                            div()
                                .id("plan")
                                .w_full()
                                .max_h(most)
                                .overflow_y_scroll()
                                .track_scroll(&scroll)
                                // Clipped as a reply is: the markdown renderer
                                // can report a width wider than the card.
                                .child(div().w_full().overflow_hidden().child(TextView::markdown(
                                    SharedString::from(format!("plan-{}", call.id)),
                                    plan,
                                    window,
                                    cx,
                                ))),
                        )
                        .vertical_scrollbar(&scroll),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("approve-plan")
                                .primary()
                                .small()
                                .label("Approve and carry on")
                                .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                                    this.keys_back(event.is_keyboard(), window, cx);
                                    this.resolve_call(Verdict::Allow { remember: false }, cx)
                                })),
                        )
                        .child(
                            Button::new("keep-planning")
                                .framed(cx)
                                .small()
                                .label("Keep planning")
                                .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                                    this.keys_back(event.is_keyboard(), window, cx);
                                    this.resolve_call(Verdict::Deny(String::new()), cx)
                                })),
                        ),
                )
                .into_any_element(),
            );
        }

        let title = match call.name.as_str() {
            "write_file" => format!("Letronna wants to write {summary}"),
            "edit_file" => format!("Letronna wants to edit {summary}"),
            "run_command" => "Letronna wants to run a command".to_owned(),
            "browse" | "use_page" => format!("Letronna wants to {summary}"),
            "search_web" => "Letronna wants to search the web".to_owned(),
            "memory" => "Letronna wants to change what it remembers".to_owned(),
            "send_job" => "Letronna wants to type into a running job".to_owned(),
            other => format!("Letronna wants to use {other}"),
        };
        let folder = self.card_folder();
        // With no project folder, a command or a write can still work in
        // Letronna's own, which the card says before it is allowed.
        let own_folder =
            (shown.fallback.as_ref()).filter(|_| asks_for_folder(&call.name, folder.is_some()));
        // Enter and Escape answer while the keys are on the card or in an
        // empty composer, and say so on the buttons they press.
        let keyed = self.question_focus.is_focused(window) || self.composer_idle(window, cx);
        let hint = |key: &str| keyed.then(|| key_hint(key));
        let answers = if !shown.places.is_empty() {
            // Where it goes now first, as Enter keeps it; each other place
            // keeps it there instead. The reason goes under them, where the
            // places leave it room.
            let keep = shown.places.iter().enumerate().fold(
                h_flex().gap_2().items_center().flex_wrap(),
                |row, (at, &place)| {
                    let button = Button::new(("keep", at))
                        .small()
                        .label(crate::ui::remember::keep_words(place));
                    let button = if at == 0 {
                        button.primary().children(hint("enter"))
                    } else {
                        button.framed(cx)
                    };
                    row.child(button.on_click(cx.listener(move |this, _, window, cx| {
                        this.take_reason(window, cx);
                        this.keys_back(false, window, cx);
                        this.resolve_call(Verdict::Keep(place), cx)
                    })))
                },
            );
            let refuse = h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&self.deny_reason).small().w_full()),
                )
                .child(
                    Button::new("deny")
                        .plain(cx)
                        .small()
                        .label("Don't save")
                        .children(hint("escape"))
                        .on_click(cx.listener(|this, _, window, cx| this.deny_call(window, cx))),
                );
            v_flex().gap_2().child(keep).child(refuse)
        } else if own_folder.is_some() {
            // Wraps, so a narrow window keeps every answer in sight.
            h_flex()
                .gap_2()
                .items_center()
                .flex_wrap()
                .child(
                    Button::new("allow")
                        .primary()
                        .small()
                        .label(own_folder_words(&call.name))
                        .children(hint("enter"))
                        .on_click(cx.listener(|this, _, window, cx| this.allow_call(window, cx))),
                )
                .children(shown.for_session.then(|| {
                    Button::new("allow-session")
                        .framed(cx)
                        .small()
                        .label("Allow for this session")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.take_reason(window, cx);
                            this.keys_back(false, window, cx);
                            this.resolve_call(Verdict::Allow { remember: true }, cx)
                        }))
                }))
                .child(
                    Button::new("approval-folder")
                        .framed(cx)
                        .small()
                        .icon(Icon::new(AppIcon::Folder))
                        .label("Choose a project folder")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.attach_workspace(window, cx)),
                        ),
                )
                .child(
                    Button::new("deny")
                        .plain(cx)
                        .small()
                        .label("Deny")
                        .children(hint("escape"))
                        .on_click(cx.listener(|this, _, window, cx| this.deny_call(window, cx))),
                )
        } else if asks_for_folder(&call.name, folder.is_some()) {
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    Button::new("approval-folder")
                        .primary()
                        .small()
                        .icon(Icon::new(AppIcon::Folder))
                        .label("Choose a project folder")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.attach_workspace(window, cx)),
                        ),
                )
                .child(
                    Button::new("deny")
                        .plain(cx)
                        .small()
                        .label("Deny")
                        .children(hint("escape"))
                        .on_click(cx.listener(|this, _, window, cx| this.deny_call(window, cx))),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("No project folder is attached yet."),
                )
        } else {
            // Wraps, so a narrow window keeps the reason's whole hint in
            // sight, its box and Deny on a line of their own.
            h_flex()
                .gap_2()
                .items_center()
                .flex_wrap()
                .child(
                    Button::new("allow")
                        .primary()
                        .small()
                        .label("Allow")
                        .children(hint("enter"))
                        .on_click(cx.listener(|this, _, window, cx| this.allow_call(window, cx))),
                )
                // An upload, or a sign-in, is asked about each time, so there
                // is no allowing it for the session.
                .children(shown.for_session.then(|| {
                    Button::new("allow-session")
                        .framed(cx)
                        .small()
                        .label("Allow for this session")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.take_reason(window, cx);
                            this.keys_back(false, window, cx);
                            this.resolve_call(Verdict::Allow { remember: true }, cx)
                        }))
                }))
                .children(self.offer.as_ref().map(|rule| {
                    Button::new("allow-here")
                        .framed(cx)
                        .small()
                        .label(format!("Always allow `{}` here", rule.pattern))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.keys_back(false, window, cx);
                            this.resolve_call(Verdict::AllowHere, cx)
                        }))
                }))
                // Beside Deny, which sends what is typed here, and wrapping
                // with it; enter in the field denies too.
                .child(
                    h_flex()
                        .flex_1()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .min_w(REASON_MIN)
                                .child(Input::new(&self.deny_reason).small().w_full()),
                        )
                        .child(
                            Button::new("deny")
                                .plain(cx)
                                .small()
                                .label("Deny")
                                .children(hint("escape"))
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.deny_call(window, cx)),
                                ),
                        ),
                )
        };
        // A stop for Tab, F6 and Alt+Up, where Enter and Escape answer it.
        let card = card
            .id("permission")
            .track_focus(&self.question_focus.clone().tab_stop(true))
            .key_context("Permission")
            .focus(|style| style.border_color(cx.theme().ring))
            // A press leaves the keys where they were, as on a turn; a box
            // or a button in it still takes them.
            .on_any_mouse_down(|_, window, _| window.prevent_default())
            .on_action(cx.listener(|this, _: &PreviousTurn, window, cx| {
                this.step_turn(Some(Stop::Question), false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NextTurn, window, cx| {
                this.step_turn(Some(Stop::Question), true, window, cx)
            }));
        Some(
            card.child(
                v_flex()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_start()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .font_family(HEADING_FONT)
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(title),
                            )
                            .children(place_note(shown.place, cx)),
                    )
                    // Where a command runs, which its text does not say.
                    .when_some(
                        folder.filter(|_| call.name == "run_command"),
                        |this, folder| {
                            this.child(
                                div()
                                    .text_xs()
                                    .truncate()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("In {}", folder.display())),
                            )
                        },
                    )
                    // Wrapped rather than cut, so the whole path is there to read.
                    .when_some(own_folder, |this, own| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(own_folder_note(&call.name, own)),
                        )
                    })
                    // Which job, and what it shows now, which what is typed
                    // does not say; wrapped, so a long command is all there.
                    .when_some(shown.typing.as_ref(), |this, typing| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(typing.job.clone()),
                        )
                        .when(!typing.last.trim().is_empty(), |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("It shows: {}", typing.last.trim())),
                            )
                        })
                    })
                    // Why Auto mode's judge left it to the user.
                    .when_some(self.why.clone(), |this, why| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(why),
                        )
                    }),
            )
            .child(match shown.typing.as_ref() {
                Some(typing) => typed(typing, window, cx).into_any_element(),
                // Scrolls only once the preview is long, so a one-line command
                // does not sit in a tall empty panel.
                None => div()
                    .p_2()
                    .min_w_0()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().background)
                    .child(crate::ui::code_block(
                        "preview",
                        tools::clamp_lines(&shown.preview, 400),
                        PREVIEW_MAX,
                        window,
                        cx,
                    ))
                    .into_any_element(),
            })
            .child(answers)
            .into_any_element(),
        )
    }

    /// The card for the first helper's call waiting on the user in session
    /// `root`, or `None` when no helper there is waiting.
    pub(crate) fn helper_card(
        &self,
        root: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let request = self
            .agent_asks
            .iter()
            .find(|request| request.root == root)?;
        let call = &request.call;
        let summary = tools::summary(&call.name, &call.arguments);
        let title = wants(&format!("Helper {}", request.label), &call.name, &summary);
        let answer = |decision: Decision| {
            let (label, id) = (request.label.clone(), call.id.clone());
            cx.listener(move |this, _, _, cx| {
                this.answer_agent(root, &label, &id, decision.clone(), cx)
            })
        };
        // A memory save is answered with where to keep it, its first place
        // where it goes now.
        let keep: Vec<_> = (request.places.iter().enumerate())
            .map(|(at, &place)| {
                let button = Button::new(("helper-keep", at))
                    .small()
                    .label(crate::ui::remember::keep_words(place));
                let button = if at == 0 {
                    button.primary()
                } else {
                    button.framed(cx)
                };
                button.on_click(answer(Decision::Keep(place)))
            })
            .collect();
        let (once, for_run, deny) = (
            answer(Decision::Once),
            answer(Decision::ForRun),
            answer(Decision::Deny(String::new())),
        );
        let card = permission(
            frame(cx),
            "helper-preview",
            title,
            &request.preview,
            window,
            cx,
        )
        // Why its gate could not decide it, such as the judge being unsure.
        .when(!request.why.is_empty(), |card| {
            card.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(request.why.clone()),
            )
        });
        if !keep.is_empty() {
            return Some(
                card.child(
                    h_flex().gap_2().children(keep).child(
                        Button::new("helper-deny")
                            .plain(cx)
                            .small()
                            .label("Don't save")
                            .on_click(deny),
                    ),
                )
                .into_any_element(),
            );
        }
        Some(
            card.child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("helper-allow")
                            .primary()
                            .small()
                            .label("Allow")
                            .on_click(once),
                    )
                    .when_some(request.for_run(), |row, pattern| {
                        // Core remembers this for the rule the call falls
                        // under, for the helper and the helpers it starts,
                        // so they ask no more about it; a call no rule
                        // covers safely is offered no such answer.
                        let label = format!("Allow `{pattern}` for this run");
                        row.child(
                            Button::new("helper-allow-run")
                                .framed(cx)
                                .small()
                                .label(label)
                                .on_click(for_run),
                        )
                    })
                    .child(
                        Button::new("helper-deny")
                            .plain(cx)
                            .small()
                            .label("Deny")
                            .on_click(deny),
                    ),
            )
            .into_any_element(),
        )
    }
}

/// The answers an `ask_user` call offers, in order.
fn options_in(args: &serde_json::Value) -> Vec<String> {
    args["options"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The letter that picks the answer at `n`, for the first 26.
fn option_letter(n: usize) -> Option<char> {
    u8::try_from(n)
        .ok()
        .filter(|&n| n < 26)
        .map(|n| char::from(b'a' + n))
}

/// Whether a call of `name` waits for a project folder before it can be
/// allowed: these work in the folder, so without one Allow could only fail.
fn asks_for_folder(name: &str, has_folder: bool) -> bool {
    !has_folder && matches!(name, "run_command" | "write_file" | "edit_file")
}

/// The button that lets a call of `name` work in Letronna's own folder.
fn own_folder_words(name: &str) -> &'static str {
    match name {
        "write_file" => "Save in Letronna's folder",
        "edit_file" => "Edit in Letronna's folder",
        _ => "Run in Letronna's folder",
    }
}

/// What a card says about a call of `name` working in `folder`, Letronna's
/// own, for want of a project folder.
fn own_folder_note(name: &str, folder: &std::path::Path) -> String {
    let what = match name {
        "write_file" => "the file is saved",
        "edit_file" => "the file is edited",
        _ => "this runs",
    };
    format!(
        "No project folder is attached, so {what} in Letronna's own folder: {}",
        folder.display()
    )
}

/// The key that answers a card, as its button shows it.
fn key_hint(key: &str) -> Kbd {
    // In its own colours and size: any text style given to it replaces its
    // own whole, colour and all, which left a blue button's white text on
    // the hint's page-coloured box, blank in Paper.
    Kbd::new(Keystroke {
        key: key.to_owned(),
        ..Default::default()
    })
}

/// The place of the card coming up in `reply` after the card `before`, with
/// `behind` of the reply's calls still to come that will wait on the user.
fn place(reply: (u64, usize), before: Option<Place>, behind: usize) -> Place {
    let n = before
        .filter(|before| before.reply == reply)
        .map_or(1, |before| before.n + 1);
    Place {
        reply,
        n,
        of: n + behind,
    }
}

/// A card's `2 of 3`, when more than one of its reply's calls wait.
fn place_note(place: Option<Place>, cx: &App) -> Option<Div> {
    let place = place.filter(|place| place.of > 1)?;
    Some(
        div()
            .flex_none()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(format!("{} of {}", place.n, place.of)),
    )
}

/// What a press of `key` comes to, and the key held after it. `held` is
/// the card key down since it was last seen pressed, with whether that
/// press answered a card; `takes` is whether a card would take the key
/// now. A key's repeats while it is held never answer: a held Enter must
/// not allow the card after the one it allowed, nor one that came up while
/// it sent a message, and what a press answered keeps its repeats.
fn pressed(
    held: Option<(CardKey, bool)>,
    key: CardKey,
    takes: bool,
) -> (Press, Option<(CardKey, bool)>) {
    match held {
        Some((down, answered)) if down == key => {
            let press = if answered || takes {
                Press::Swallow
            } else {
                Press::PassOn
            };
            (press, held)
        }
        _ if takes => (Press::Answer, Some((key, true))),
        _ => (Press::PassOn, Some((key, false))),
    }
}

/// What a send_job card says it will type: what an earlier call left on the
/// line, the text exactly, control characters written out, then each key it
/// presses as a key.
fn typed(typing: &tools::Typing, window: &mut Window, cx: &mut App) -> Div {
    let theme = cx.theme();
    let (muted, radius, ground) = (theme.muted_foreground, theme.radius, theme.background);
    let keys: Vec<AnyElement> = (typing.keys.iter())
        .map(|name| match stroke_of(name) {
            Some(stroke) => Kbd::new(stroke).text_xs().into_any_element(),
            None => div()
                .font_family(MONO_FONT)
                .child(name.clone())
                .into_any_element(),
        })
        .collect();
    let then = if typing.text.is_empty() {
        "Presses"
    } else {
        "Then presses"
    };
    v_flex()
        .gap_2()
        // Enter sends it with the text, so it is shown before it.
        .when(!typing.earlier.is_empty(), |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("Already on the line: {}", typing.earlier)),
            )
        })
        .when(!typing.text.is_empty(), |this| {
            this.child(div().p_2().min_w_0().rounded(radius).bg(ground).child(
                crate::ui::code_block(
                    "preview",
                    tools::clamp_lines(&typing.text, 400),
                    PREVIEW_MAX,
                    window,
                    cx,
                ),
            ))
        })
        .when(!keys.is_empty(), |this| {
            this.child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .flex_wrap()
                    .text_xs()
                    .text_color(muted)
                    .child(then)
                    .children(keys),
            )
        })
}

/// The key send_job's key `name` stands for, to show it as one.
fn stroke_of(name: &str) -> Option<Keystroke> {
    // gpui names Escape in full; send_job's other names are gpui's own.
    let name = if name == "esc" { "escape" } else { name };
    Keystroke::parse(name).ok()
}

/// The box an approval card sits in.
fn frame(cx: &App) -> Div {
    v_flex()
        .w_full()
        .gap_3()
        .p_3()
        .rounded(cx.theme().radius_lg)
        .border_1()
        .border_color(cx.theme().primary.opacity(0.5))
        .bg(cx.theme().secondary.opacity(0.3))
}

/// What a card says `who` wants to do with a call of `name`.
fn wants(who: &str, name: &str, summary: &str) -> String {
    match name {
        "write_file" => format!("{who} wants to write {summary}"),
        "edit_file" => format!("{who} wants to edit {summary}"),
        "run_command" => format!("{who} wants to run a command"),
        "browse" | "use_page" => format!("{who} wants to {summary}"),
        "search_web" => format!("{who} wants to search the web"),
        "memory" => format!("{who} wants to change what it remembers"),
        "send_job" => format!("{who} wants to type into a running job"),
        other => format!("{who} wants to use {other}"),
    }
}

/// `card` with what a call wants and the tool's preview of it, ready for its
/// answers; `id` keeps the preview's scrolling apart from another card's.
fn permission(
    card: Div,
    id: &'static str,
    title: String,
    preview: &str,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    card.child(
        div()
            .text_sm()
            .font_family(HEADING_FONT)
            .font_weight(FontWeight::MEDIUM)
            .child(title),
    )
    .child(
        // Scrolls only once the preview is long, so a one-line command
        // does not sit in a tall empty panel.
        div()
            .p_2()
            .min_w_0()
            .rounded(cx.theme().radius)
            .bg(cx.theme().background)
            .child(crate::ui::code_block(
                id,
                tools::clamp_lines(preview, 400),
                px(240.),
                window,
                cx,
            )),
    )
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{
        CardFor, CardKey, Press, asks_for_folder, key_hint, option_letter, options_in,
        own_folder_note, own_folder_words, place, pressed, stroke_of,
    };
    use letronna_core::tools::ToolContext;
    use std::path::PathBuf;

    #[test]
    fn a_question_offers_its_answers_in_order_each_under_a_letter() {
        let args = serde_json::json!({"question": "Which?", "options": ["Red", 3, "Blue"]});
        assert_eq!(options_in(&args), ["Red", "Blue"]);
        assert_eq!(options_in(&serde_json::json!({})), Vec::<String>::new());
        assert_eq!(option_letter(0), Some('a'));
        assert_eq!(option_letter(25), Some('z'));
        assert_eq!(option_letter(26), None);
    }

    #[test]
    fn the_cards_of_one_reply_are_counted_in_order() {
        let first = place((7, 4), None, 2);
        assert_eq!((first.n, first.of), (1, 3));
        let second = place((7, 4), Some(first), 1);
        assert_eq!((second.n, second.of), (2, 3));
        // Allowed for the session, the last one asked no more.
        let last = place((7, 4), Some(second), 0);
        assert_eq!((last.n, last.of), (3, 3));
        // A later reply, or another session, counts from one again.
        let next = place((7, 9), Some(last), 0);
        assert_eq!((next.n, next.of), (1, 1));
        assert_eq!(place((8, 4), Some(last), 1).n, 1);
    }

    #[test]
    fn a_held_key_never_answers_the_next_card() {
        use CardKey::{Enter, Escape};
        // A fresh press answers the card, and its repeats answer nothing,
        // whether a card takes them or not.
        let (press, held) = pressed(None, Enter, true);
        assert_eq!((press, held), (Press::Answer, Some((Enter, true))));
        assert_eq!(pressed(held, Enter, true).0, Press::Swallow);
        assert_eq!(pressed(held, Enter, false).0, Press::Swallow);
        // Held down to send a message, Enter still sends, but its repeats
        // never answer a card that comes up meanwhile.
        let (press, held) = pressed(None, Enter, false);
        assert_eq!((press, held), (Press::PassOn, Some((Enter, false))));
        assert_eq!(pressed(held, Enter, true).0, Press::Swallow);
        assert_eq!(pressed(held, Enter, false).0, Press::PassOn);
        // Another key is a press of its own.
        assert_eq!(pressed(held, Escape, true).0, Press::Answer);
    }

    #[test]
    fn a_command_or_a_write_waits_for_a_project_folder() {
        assert!(asks_for_folder("run_command", false));
        assert!(asks_for_folder("edit_file", false));
        assert!(!asks_for_folder("run_command", true));
        assert!(!asks_for_folder("search_web", false));
    }

    #[test]
    fn a_command_with_no_project_folder_is_offered_letronnas_own() {
        let call = letronna_core::chat::ToolCall {
            id: "1".into(),
            name: "run_command".into(),
            arguments: r#"{"command":"ls"}"#.into(),
        };
        let own = PathBuf::from("/home/me/Letronna");
        let mut ctx = ToolContext::default();
        ctx.workspace = Some(own.clone());
        ctx.fallback = true;
        assert_eq!(CardFor::of(&call, &ctx).fallback, Some(own.clone()));
        // A folder the user attached is the project, not Letronna's own.
        ctx.fallback = false;
        assert_eq!(CardFor::of(&call, &ctx).fallback, None);
        assert_eq!(own_folder_words("run_command"), "Run in Letronna's folder");
        assert_eq!(own_folder_words("write_file"), "Save in Letronna's folder");
        assert_eq!(
            own_folder_note("run_command", &own),
            "No project folder is attached, so this runs in Letronna's own folder: \
             /home/me/Letronna"
        );
        assert!(own_folder_note("edit_file", &own).contains("the file is edited in"));
    }

    #[test]
    fn a_key_hint_keeps_its_own_text_colour_on_any_button() {
        // A text style of the card's would stand in for the hint's own whole,
        // and its text would take the button's colour over the hint's box.
        let mut hint = key_hint("enter");
        assert!(gpui::Styled::style(&mut hint).text.is_none());
    }

    #[test]
    fn each_key_send_job_presses_is_shown_as_that_key() {
        let stroke = |name: &str| stroke_of(name).unwrap();
        assert_eq!(stroke("enter").key, "enter");
        assert_eq!(stroke("tab").key, "tab");
        assert_eq!(stroke("up").key, "up");
        assert_eq!(stroke("down").key, "down");
        // Esc is gpui's escape, and a control key is the letter held with Ctrl.
        assert_eq!(stroke("esc").key, "escape");
        let ctrl_c = stroke("ctrl-c");
        assert!(ctrl_c.modifiers.control && !ctrl_c.modifiers.shift);
        assert_eq!(ctrl_c.key, "c");
        assert_eq!(stroke("ctrl-d").key, "d");
    }
}
