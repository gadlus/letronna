//! The keyboard's way round the window: F6 through its parts, Ctrl+L to the
//! composer, and F1 for the list of every key. The keys themselves are one
//! table, `keys` in main.rs, which the list is read from.

use crate::prelude::*;

/// A part of the window F6 moves between.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Region {
    Sidebar,
    /// The conversation, or the page shown in its place.
    Transcript,
    /// The strip over the composer, while it shows.
    Strip,
    Composer,
    Panel,
}

impl Chat {
    /// The parts of the window F6 goes through, in order, as drawn now.
    fn regions(&self, window: &Window, cx: &App) -> Vec<Region> {
        let mut shown = Vec::new();
        if self.sidebar_open {
            shown.push(Region::Sidebar);
        }
        // The browser over the conversation hides it.
        if self.page != Page::Chat || (self.transcript_shown() && !self.panel_covers(window, cx)) {
            shown.push(Region::Transcript);
        }
        if self.strip_shown() {
            shown.push(Region::Strip);
        }
        if self.page == Page::Chat {
            shown.push(Region::Composer);
        }
        if self.tab_on_screen(window, cx).is_some() {
            shown.push(Region::Panel);
        }
        shown
    }

    /// Whether the focus is somewhere inside `region`.
    fn holds_focus(&self, region: Region, window: &Window, cx: &App) -> bool {
        match region {
            Region::Sidebar => self.sidebar_focus.contains_focused(window, cx),
            // The question card is drawn in the transcript, but maybe not
            // yet, when F6 or Alt+Down has only just scrolled to it.
            Region::Transcript if self.page == Page::Chat => {
                self.transcript_focus.contains_focused(window, cx)
                    || self.question_focus.is_focused(window)
            }
            Region::Transcript => self.page_focus.contains_focused(window, cx),
            Region::Strip => self.strip_area.contains_focused(window, cx),
            Region::Composer => self.composer.focus_handle(cx).contains_focused(window, cx),
            Region::Panel => self.panel_focus.contains_focused(window, cx),
        }
    }

    /// Move to the next part of the window, or with `back` the one before.
    pub(crate) fn cycle_region(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) || self.settings_open {
            return;
        }
        let shown = self.regions(window, cx);
        let now = shown
            .iter()
            .copied()
            .find(|&region| self.holds_focus(region, window, cx));
        let Some(region) = step_region(&shown, now, back) else {
            return;
        };
        match region {
            Region::Sidebar => {
                self.picked = self
                    .picked
                    .or(Some(self.store.conversations[self.store.active].id));
                window.focus(&self.sidebar_focus);
            }
            Region::Transcript if self.page != Page::Chat => window.focus(&self.page_focus),
            // The question waiting, else the newest turn, whose buttons
            // then show; the transcript itself only when it has neither, or
            // that turn is scrolled out of the list's reach.
            Region::Transcript => match self.stops().last() {
                Some(&stop) => self.go_to_stop(stop, false, window),
                None => window.focus(&self.transcript_focus),
            },
            Region::Strip => window.focus(&self.strip_focus),
            Region::Composer => self.focus_composer(window, cx),
            Region::Panel => window.focus(&self.panel_focus),
        }
        self.region = Some(region);
        cx.notify();
    }

    /// Let the ring go once the focus has left the part it marks, by a
    /// click, Tab or anything else.
    pub(crate) fn keep_region(&mut self, window: &Window, cx: &App) {
        if let Some(region) = self.region
            && !self.holds_focus(region, window, cx)
        {
            self.region = None;
        }
    }

    /// The ring over `region` while F6 has put the keys there, to lay over
    /// it: drawn on top rather than as its border, so nothing moves.
    pub(crate) fn ring(&self, region: Region, cx: &App) -> Option<Div> {
        (self.region == Some(region)).then(|| {
            div()
                .absolute()
                .inset_0()
                .border_2()
                .border_color(cx.theme().ring)
        })
    }

    /// Ctrl+L: the composer, from anywhere in the window, even another page.
    pub(crate) fn focus_composer_anywhere(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) || self.settings_open {
            return;
        }
        self.page = Page::Chat;
        self.model_open = false;
        self.region = None;
        self.focus_composer(window, cx);
        cx.notify();
    }

    /// The list of every key, read from the table they are bound from.
    pub(crate) fn show_keys(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) || self.settings_open {
            return;
        }
        let rows: Vec<(Keystroke, &'static str)> = crate::keys(cx.keyboard_mapper().as_ref())
            .into_iter()
            .filter_map(|(binding, about)| {
                let stroke = binding.keystrokes().first()?.as_keystroke().clone();
                Some((stroke, about?))
            })
            .collect();
        window.open_dialog(cx, move |dialog, _, cx| {
            dialog
                .title(div().font_family(HEADING_FONT).child("Keys"))
                .w(px(460.))
                .child(
                    v_flex()
                        .id("key-list")
                        .max_h(px(420.))
                        .overflow_y_scrollbar()
                        .gap_1()
                        .children(rows.iter().map(|(stroke, about)| {
                            h_flex()
                                .gap_3()
                                .items_center()
                                .text_sm()
                                .child(div().flex_1().min_w_0().child(*about))
                                .child(Kbd::new(stroke.clone()).text_xs())
                        }))
                        .child(
                            div()
                                .pt_2()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                    "Typing anywhere outside a text box writes in the \
                                     composer. ? in an empty composer opens this list too.",
                                ),
                        ),
                )
        });
    }
}

/// The part after `now` in `shown`, or before it with `back`, wrapping
/// round; from outside them all, the first or the last.
fn step_region(shown: &[Region], now: Option<Region>, back: bool) -> Option<Region> {
    let count = shown.len();
    if count == 0 {
        return None;
    }
    let at = now.and_then(|region| shown.iter().position(|&r| r == region));
    let next = match (at, back) {
        (None, false) => 0,
        (None, true) => count - 1,
        (Some(i), false) => (i + 1) % count,
        (Some(i), true) => (i + count - 1) % count,
    };
    Some(shown[next])
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{Region, step_region};

    #[test]
    fn f6_goes_round_the_parts_shown_and_shift_goes_back() {
        use Region::*;
        let all = [Sidebar, Transcript, Composer, Panel];
        assert_eq!(step_region(&all, Some(Composer), false), Some(Panel));
        assert_eq!(step_region(&all, Some(Panel), false), Some(Sidebar));
        assert_eq!(step_region(&all, Some(Sidebar), true), Some(Panel));
        assert_eq!(step_region(&all, None, false), Some(Sidebar));
        assert_eq!(step_region(&all, None, true), Some(Panel));
        // A part that is hidden now counts as being in none of them.
        let two = [Transcript, Composer];
        assert_eq!(step_region(&two, Some(Sidebar), false), Some(Transcript));
        assert_eq!(step_region(&[], None, false), None);
    }
}
