//! The recent-session switcher. Holding Ctrl, Tab lists the sessions used
//! last, the newest first, over the middle of the window; Tab and Shift+Tab
//! move, letting go of Ctrl opens the one picked and Esc stays put. A quick
//! tap therefore swaps to the session before. Which sessions were used is
//! kept in memory for the window's life; nothing is drawn or runs while the
//! list is closed.

use crate::prelude::*;

actions!(
    switcher,
    [
        SwitchSession,
        SwitchSessionBack,
        NextRecent,
        PreviousRecent,
        OpenRecent,
        CloseRecent
    ]
);

/// The most sessions listed, as many as a glance takes in.
const ROWS: usize = 8;
/// A row's height, the sidebar's session row and a little more.
const ROW: Pixels = px(28.);
/// The list's width: room for a title, its folder and an age.
const WIDTH: Pixels = px(360.);

/// The switcher while it shows.
pub(crate) struct Switcher {
    /// The sessions listed, by id, the one on screen first.
    rows: Vec<u64>,
    /// The row picked.
    at: usize,
    /// Opened with Ctrl held, so letting go of it opens the pick; from the
    /// palette it waits for Enter instead.
    held: bool,
    focus: FocusHandle,
    /// Where the keys were before it opened, given back when it closes.
    before: Option<FocusHandle>,
    /// What drops it when the keys or the window go elsewhere.
    _leaving: [Subscription; 2],
}

/// Put `id` at the front of `recent`, the sessions the newest used first.
fn note(recent: &mut Vec<u64>, id: u64) {
    if recent.first() == Some(&id) {
        return;
    }
    recent.retain(|&other| other != id);
    recent.insert(0, id);
}

/// The sessions to list: those used while the window is open, the newest
/// first, then the rest by when they last changed, so the list is full from
/// the first switch. `sessions` is every session's id and update time.
fn listed(recent: &[u64], sessions: &[(u64, u64)]) -> Vec<u64> {
    let mut rest: Vec<(u64, u64)> = sessions
        .iter()
        .copied()
        .filter(|(id, _)| !recent.contains(id))
        .collect();
    rest.sort_by_key(|&(_, updated)| std::cmp::Reverse(updated));
    recent
        .iter()
        .copied()
        .filter(|id| sessions.iter().any(|(known, _)| known == id))
        .chain(rest.into_iter().map(|(id, _)| id))
        .take(ROWS)
        .collect()
}

/// The row after `at` of `count`, or before it with `back`, wrapping round.
fn step(at: usize, count: usize, back: bool) -> usize {
    match (count, back) {
        (0, _) => 0,
        (_, false) => (at + 1) % count,
        (_, true) => (at + count - 1) % count,
    }
}

impl Chat {
    /// Count the session on screen as the one used last.
    pub(crate) fn note_recent(&mut self, id: u64) {
        note(&mut self.recent, id);
    }

    /// Ctrl+Tab, or Ctrl+Shift+Tab with `back`: open the list on the session
    /// before (or the oldest listed), or move in it while it shows.
    pub(crate) fn switch_session(
        &mut self,
        back: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(switcher) = &mut self.switcher {
            switcher.at = step(switcher.at, switcher.rows.len(), back);
            cx.notify();
            return;
        }
        if window.has_active_dialog(cx) || self.settings_open {
            return;
        }
        let on_screen = self.store.conversations[self.store.active].id;
        self.note_recent(on_screen);
        let sessions: Vec<(u64, u64)> = (self.store.conversations.iter())
            .map(|c| (c.id, c.updated))
            .collect();
        let rows = listed(&self.recent, &sessions);
        if rows.len() < 2 {
            return;
        }
        let focus = cx.focus_handle();
        let blurred = cx.on_blur(&focus, window, |this, _, cx| {
            this.switcher = None;
            cx.notify();
        });
        let away = cx.observe_window_activation(window, |this, window, cx| {
            if !window.is_window_active() && this.switcher.take().is_some() {
                cx.notify();
            }
        });
        self.switcher = Some(Switcher {
            at: if back { rows.len() - 1 } else { 1 },
            rows,
            held: window.modifiers().control,
            before: window.focused(cx),
            focus: focus.clone(),
            _leaving: [blurred, away],
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Close the list and open the session picked, or with `stay` keep the
    /// one on screen; the keys go back where they were unless it changed.
    fn close_switcher(&mut self, stay: bool, window: &mut Window, cx: &mut Context<Self>) {
        // Taken first, so moving the keys below does not close it twice.
        let Some(switcher) = self.switcher.take() else {
            return;
        };
        cx.notify();
        let picked = (!stay)
            .then(|| switcher.rows.get(switcher.at))
            .flatten()
            .and_then(|&id| self.store.index_of(id))
            .filter(|&ix| ix != self.store.active || self.page != Page::Chat);
        let Some(ix) = picked else {
            match switcher.before {
                Some(before) => window.focus(&before, cx),
                None => self.focus_composer(window, cx),
            }
            return;
        };
        self.leave_for(ix);
        self.store.active = ix;
        self.error = None;
        self.page = Page::Chat;
        // The handle is shared across sessions; start at the latest message.
        self.follow();
        self.focus_composer(window, cx);
        self.save();
    }

    /// The list over the middle of the window, while it shows.
    pub(crate) fn switcher(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let switcher = self.switcher.as_ref()?;
        let now = backend::now();
        let theme = cx.theme();
        let (muted, picked) = (theme.muted_foreground, theme.accent);
        let mut rows = Vec::new();
        for (n, &id) in switcher.rows.iter().enumerate() {
            let Some(ix) = self.store.index_of(id) else {
                continue;
            };
            let c = &self.store.conversations[ix];
            let running = (self.busy && self.streaming_id == Some(id))
                || self.runs.contains_key(&id)
                || self.helpers_working_in(id);
            let folder = c
                .workspace
                .as_deref()
                .map(|path| super::sessions::project_label(Some(path)));
            rows.push(
                h_flex()
                    .id(("recent", n))
                    .h(ROW)
                    .px_2()
                    .gap_2()
                    .items_center()
                    .rounded(cx.theme().radius)
                    .cursor_pointer()
                    .when(n == switcher.at, |row| row.bg(picked))
                    // Steady rather than turning: the list is up only a
                    // moment, and a turning mark would draw it all again.
                    .child(
                        div()
                            .size(px(6.))
                            .flex_shrink_0()
                            .rounded_full()
                            .when(running, |dot| dot.bg(cx.theme().green)),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(c.title.clone()))
                    .when_some(folder, |row, folder| {
                        row.child(
                            div()
                                .max_w(px(120.))
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(muted)
                                .child(folder),
                        )
                    })
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(muted)
                            .child(age_label(c.updated, now)),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(switcher) = &mut this.switcher {
                            switcher.at = n;
                        }
                        this.close_switcher(false, window, cx);
                    })),
            );
        }
        let hint: [&str; 3] = if switcher.held {
            ["Tab to move", "Let go to open", "Esc to stay"]
        } else {
            ["Up and down to move", "Enter to open", "Esc to stay"]
        };
        let list = v_flex()
            .id("switcher")
            .key_context("Switcher")
            .track_focus(&switcher.focus)
            .occlude()
            .w(WIDTH)
            .p_1()
            .bg(cx.theme().background)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius_lg)
            .text_sm()
            .text_color(cx.theme().foreground)
            .on_modifiers_changed(
                cx.listener(|this, event: &ModifiersChangedEvent, window, cx| {
                    if this.switcher.as_ref().is_some_and(|s| s.held) && !event.modifiers.control {
                        this.close_switcher(false, window, cx);
                    }
                }),
            )
            .on_action(cx.listener(|this, _: &NextRecent, window, cx| {
                this.switch_session(false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &PreviousRecent, window, cx| {
                this.switch_session(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenRecent, window, cx| {
                this.close_switcher(false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CloseRecent, window, cx| {
                this.close_switcher(true, window, cx)
            }))
            .children(rows)
            .child(
                h_flex()
                    .h(px(26.))
                    .mt_1()
                    .px_2()
                    .gap_3()
                    .items_center()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_xs()
                    .text_color(muted)
                    .children(hint),
            );
        // Over everything, so a click anywhere else closes the list and does
        // nothing more, as Esc does.
        Some(
            div()
                .id("switcher-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_switcher(true, window, cx)),
                )
                .child(list)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{listed, note, step};

    #[test]
    fn a_session_used_again_moves_to_the_front_once() {
        let mut recent = vec![3, 2, 1];
        note(&mut recent, 1);
        assert_eq!(recent, vec![1, 3, 2]);
        note(&mut recent, 1);
        assert_eq!(recent, vec![1, 3, 2]);
        note(&mut recent, 9);
        assert_eq!(recent, vec![9, 1, 3, 2]);
    }

    #[test]
    fn sessions_used_here_come_first_then_the_rest_by_their_last_change() {
        let sessions = [(1, 100), (2, 400), (3, 300), (4, 200)];
        assert_eq!(listed(&[3, 1], &sessions), vec![3, 1, 2, 4]);
    }

    #[test]
    fn a_deleted_session_is_left_out_and_eight_at_most_are_listed() {
        let sessions: Vec<(u64, u64)> = (1..=12).map(|id| (id, id)).collect();
        let shown = listed(&[40, 5], &sessions);
        assert_eq!(shown, vec![5, 12, 11, 10, 9, 8, 7, 6]);
    }

    #[test]
    fn moving_wraps_round_both_ways() {
        assert_eq!(step(2, 3, false), 0);
        assert_eq!(step(0, 3, true), 2);
        assert_eq!(step(0, 0, false), 0);
    }
}
