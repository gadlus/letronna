use crate::prelude::*;
use letronna_core::complete::{self, Choice, Kind};

actions!(
    complete,
    [ChoiceUp, ChoiceDown, TakeChoice, OpenChoice, CloseChoices]
);

/// The commands the window runs itself when typed after `/`, as (name, what
/// it does). A command the window has no way to run is left out.
const COMMANDS: &[(&str, &str)] = &[
    ("new", "start a fresh session"),
    ("mode", "change how much runs without asking"),
    ("btw", "ask a side question that stays out of the session"),
    (
        "cloud",
        "run a task in Letronna Cloud, on Letronna's servers",
    ),
    ("init", "study the project and write its AGENTS.md"),
];

/// The rows shown before the list scrolls.
const ROWS_SHOWN: Pixels = px(280.);

/// What the composer offers for a `/` or `@` being typed.
#[derive(Default)]
pub(crate) struct Completer {
    /// The list on screen, while there is one.
    open: Option<Open>,
    /// Where the `/` or `@` whose list was closed with Escape is, so it
    /// stays closed while that word is typed on.
    dismissed: Option<usize>,
    /// References taken from the list and still in the message.
    accepted: Vec<String>,
    scroll: ScrollHandle,
}

struct Open {
    trigger: char,
    start: usize,
    query: String,
    choices: Vec<Choice>,
    selected: usize,
    /// While the project's names are being gathered.
    looking: bool,
    /// Gathering them, dropped when a newer query replaces it.
    _gathering: Option<Task<()>>,
}

impl Chat {
    /// Look again at what is typed before the cursor, and open, change or
    /// close the list to match. Called on every change to the composer.
    pub(crate) fn update_completion(&mut self, cx: &mut Context<Self>) {
        let (text, cursor) = {
            let input = self.composer.read(cx);
            (input.value().to_string(), input.cursor())
        };
        let was_open = self.completer.open.is_some();
        let before = self.completer.accepted.len();
        self.completer
            .accepted
            .retain(|named| text.contains(named.as_str()));
        let changed = before != self.completer.accepted.len();
        let Some(typing) = complete::typing(&text, cursor) else {
            self.completer.open = None;
            self.completer.dismissed = None;
            if was_open || changed {
                cx.notify();
            }
            return;
        };
        if self.completer.dismissed == Some(typing.start) {
            return;
        }
        self.completer.dismissed = None;
        if let Some(open) = &self.completer.open
            && open.start == typing.start
            && open.query == typing.query
        {
            return;
        }
        let mut open = Open {
            trigger: typing.trigger,
            start: typing.start,
            query: typing.query.to_owned(),
            choices: Vec::new(),
            selected: 0,
            looking: false,
            _gathering: None,
        };
        if typing.trigger == '/' {
            open.choices = complete::commands(typing.query, COMMANDS, &self.skills);
        } else {
            // What was offered stays up until the new list is ready, rather
            // than the list emptying on every key.
            if let Some(earlier) = self.completer.open.take()
                && earlier.trigger == '@'
            {
                open.choices = earlier.choices;
            }
            open.looking = true;
            open._gathering = Some(self.gather_references(typing.start, typing.query, cx));
        }
        self.completer.open = Some(open);
        self.completer.scroll.scroll_to_item(0);
        cx.notify();
    }

    /// Find what an `@` followed by `query` could mean on the background
    /// executor, since the first look walks the project.
    fn gather_references(&self, start: usize, query: &str, cx: &mut Context<Self>) -> Task<()> {
        let active = &self.store.conversations[self.store.active];
        let workspace = active.workspace.clone();
        let sessions: Vec<(u64, String)> = self
            .store
            .conversations
            .iter()
            .rev()
            .filter(|c| c.id != active.id)
            .map(|c| (c.id, c.title.clone()))
            .collect();
        let query = query.to_owned();
        cx.spawn(async move |this, cx| {
            let asked = query.clone();
            let choices = cx
                .background_executor()
                .spawn(async move { complete::references(&asked, workspace.as_deref(), &sessions) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(open) = this
                    .completer
                    .open
                    .as_mut()
                    .filter(|open| open.start == start && open.query == query)
                else {
                    return;
                };
                open.choices = choices;
                open.selected = 0;
                open.looking = false;
                this.completer.scroll.scroll_to_item(0);
                cx.notify();
            });
        })
    }

    fn move_choice(&mut self, by: isize, cx: &mut Context<Self>) {
        let Some(open) = self.completer.open.as_mut() else {
            return;
        };
        let count = open.choices.len();
        if count == 0 {
            return;
        }
        open.selected = (open.selected as isize + by).rem_euclid(count as isize) as usize;
        self.completer.scroll.scroll_to_item(open.selected);
        cx.notify();
    }

    fn close_choices(&mut self, cx: &mut Context<Self>) {
        if let Some(open) = self.completer.open.take() {
            self.completer.dismissed = Some(open.start);
            cx.notify();
        }
    }

    /// Put the choice at `index` in place of what was typed. With `into`, a
    /// folder is opened instead, listing what is in it; a command the
    /// window runs itself runs.
    fn take_choice(
        &mut self,
        index: usize,
        into: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(open) = self.completer.open.as_ref() else {
            return;
        };
        let Some(choice) = open.choices.get(index).cloned() else {
            return;
        };
        let (text, cursor) = {
            let input = self.composer.read(cx);
            (input.value().to_string(), input.cursor())
        };
        // The word may have moved since the list was made.
        let Some(typing) = complete::typing(&text, cursor).filter(|t| t.start == open.start) else {
            self.completer.open = None;
            cx.notify();
            return;
        };
        let start = typing.start;
        self.completer.open = None;
        if choice.kind == Kind::Command {
            let rest = format!("{}{}", &text[..start], &text[cursor..]);
            match choice.label.as_str() {
                "/new" => {
                    self.replace_composer(rest, start, window, cx);
                    return self.new_chat(window, cx);
                }
                "/mode" => {
                    self.replace_composer(rest, start, window, cx);
                    return self.open_mode_menu(window, cx);
                }
                _ => {}
            }
        }
        // A folder Tab opens is only passed through: its name stays inside
        // whatever is picked in it, so it would stay listed as taken.
        let opened = into && choice.kind == Kind::Folder;
        let inserted = if opened {
            choice.text.trim_end().to_owned()
        } else {
            choice.text.clone()
        };
        if !opened
            && matches!(
                choice.kind,
                Kind::File | Kind::Folder | Kind::Session | Kind::Diff
            )
        {
            let named = choice.text.trim_end().to_owned();
            if !self.completer.accepted.contains(&named) {
                self.completer.accepted.push(named);
            }
        }
        let value = format!("{}{inserted}{}", &text[..start], &text[cursor..]);
        self.replace_composer(value, start + inserted.len(), window, cx);
    }

    /// Set the composer to `value` with the cursor at byte `at` of it.
    fn replace_composer(
        &mut self,
        value: String,
        at: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::input::RopeExt;
        self.composer.update(cx, |input, cx| {
            input.set_value(value, window, cx);
            let at = input.text().offset_to_position(at);
            input.set_cursor_position(at, window, cx);
        });
    }

    /// The composer's key handlers for the list, on the element around the
    /// input, which carries the `Completing` context only while it is up.
    pub(crate) fn completion_keys(&self, around: Div, cx: &mut Context<Self>) -> Div {
        // With nothing to pick, the keys stay the composer's: Enter sends
        // "/tmp is full" rather than being swallowed by an empty list.
        if self
            .completer
            .open
            .as_ref()
            .is_none_or(|o| o.choices.is_empty())
        {
            return around;
        }
        around
            .key_context("Completing")
            .on_action(cx.listener(|this, _: &ChoiceUp, _, cx| this.move_choice(-1, cx)))
            .on_action(cx.listener(|this, _: &ChoiceDown, _, cx| this.move_choice(1, cx)))
            .on_action(cx.listener(|this, _: &CloseChoices, _, cx| this.close_choices(cx)))
            .on_action(cx.listener(|this, _: &TakeChoice, window, cx| {
                let at = this.completer.open.as_ref().map_or(0, |o| o.selected);
                this.take_choice(at, false, window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenChoice, window, cx| {
                let at = this.completer.open.as_ref().map_or(0, |o| o.selected);
                this.take_choice(at, true, window, cx);
            }))
    }

    /// The list over the composer, while there is one.
    pub(crate) fn completion_list(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let open = self.completer.open.as_ref()?;
        let muted = cx.theme().muted_foreground;
        let no_project = open.trigger == '@'
            && self.store.conversations[self.store.active]
                .workspace
                .is_none();
        let note = if open.looking && open.choices.is_empty() {
            Some("Listing the project…")
        } else if open.choices.is_empty() && no_project {
            Some("Pick a project folder to reference its files.")
        } else if open.choices.is_empty() {
            Some("Nothing matches.")
        } else {
            None
        };
        let rows = open.choices.iter().enumerate().map(|(i, choice)| {
            let icon = match choice.kind {
                Kind::Command => AppIcon::Play,
                Kind::Skill => AppIcon::Puzzle,
                Kind::Folder => AppIcon::Folder,
                Kind::File => AppIcon::File,
                Kind::Session => AppIcon::MessageSquare,
                Kind::Diff => AppIcon::Branch,
            };
            h_flex()
                .id(("choice", i))
                .w_full()
                .gap_2()
                .items_center()
                .px_2()
                .py_1()
                .rounded(cx.theme().radius)
                .when(i == open.selected, |row| row.bg(cx.theme().list_active))
                .child(Icon::new(icon).size_3().text_color(muted))
                .child(div().flex_shrink_0().child(choice.label.clone()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(muted)
                        .child(choice.detail.clone()),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.take_choice(i, false, window, cx);
                    this.focus_composer(window, cx);
                }))
        });
        let hint = if open.choices.iter().any(|c| c.kind == Kind::Folder) {
            "Up and Down to move, Tab to open a folder, Enter to pick, Esc to close"
        } else {
            "Up and Down to move, Enter to pick, Esc to close"
        };
        let list = v_flex().occlude().w(px(460.)).pb_1p5().child(
            v_flex()
                .bg(cx.theme().background)
                .border_1()
                .border_color(cx.theme().border)
                .rounded(cx.theme().radius_lg)
                .text_sm()
                .child(
                    v_flex()
                        .id("choices")
                        .max_h(ROWS_SHOWN)
                        .overflow_y_scroll()
                        .track_scroll(&self.completer.scroll)
                        .p_1()
                        .children(rows)
                        .children(
                            note.map(|note| div().px_2().py_1().text_color(muted).child(note)),
                        ),
                )
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .text_xs()
                        .text_color(muted)
                        .child(hint),
                ),
        );
        Some(
            div().absolute().top_0().left_0().child(
                deferred(
                    anchored()
                        .anchor(Anchor::BottomLeft)
                        .snap_to_window_with_margin(px(8.))
                        .child(list),
                )
                .with_priority(1),
            ),
        )
    }

    /// The references taken from the list that are still in the message, in
    /// the signal colour, over the composer.
    pub(crate) fn accepted_row(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if self.completer.accepted.is_empty() {
            return None;
        }
        Some(
            h_flex()
                .flex_wrap()
                .gap_x_2()
                .pb_1()
                .text_xs()
                .text_color(cx.theme().link)
                .children(self.completer.accepted.iter().cloned()),
        )
    }
}
