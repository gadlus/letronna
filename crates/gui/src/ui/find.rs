//! Find in the open chat. Cmd+F (Ctrl+F elsewhere) opens a bar over the
//! chat; what is typed there finds the messages of the session on screen
//! that hold it, and the newest is brought to the top the way a search hit
//! is. Enter goes to the one before it, Shift+Enter back down, and Escape
//! closes the bar. The messages are matched whole rather than the words
//! marked in them, since the chat draws its text as laid-out markdown.

use crate::prelude::*;

actions!(find, [OpenFind]);

/// The find bar, while it is open.
pub(crate) struct Finder {
    pub(crate) input: Entity<InputState>,
    /// Which match is on screen, as a place in the session's messages.
    at: Option<usize>,
    _typed: Subscription,
}

/// The places in `messages` of those whose words hold `query`, ignoring
/// case. Only what was said is looked through: a tool's output is folded
/// away, and can run to megabytes.
fn matching(messages: &[Message], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    let said = messages.iter().enumerate().filter(|(_, m)| {
        let text = match m.role.as_str() {
            "user" => letronna_core::references::typed(&m.content),
            "assistant" => m.content.as_str(),
            _ => return false,
        };
        text.to_lowercase().contains(&query)
    });
    said.map(|(at, _)| at).collect()
}

/// The match to show after `at` among `hits`: the one before it going up,
/// or after it going down, round at the ends, and the newest when none is
/// shown yet.
fn step(hits: &[usize], at: Option<usize>, up: bool) -> Option<usize> {
    let newest = *hits.last()?;
    let Some(at) = at else {
        return Some(newest);
    };
    if up {
        Some(
            hits.iter()
                .rev()
                .find(|&&hit| hit < at)
                .copied()
                .unwrap_or(newest),
        )
    } else {
        Some(
            hits.iter()
                .find(|&&hit| hit > at)
                .copied()
                .unwrap_or(hits[0]),
        )
    }
}

/// What the bar says of the matches: which of them is shown, counted from
/// the top, or that there are none.
fn count(hits: &[usize], at: Option<usize>, typed: bool) -> String {
    match at.and_then(|at| hits.iter().position(|&hit| hit == at)) {
        Some(shown) => format!("{} of {}", shown + 1, hits.len()),
        None if typed && hits.is_empty() => "No matches".into(),
        None => String::new(),
    }
}

impl Chat {
    /// Open the find bar over the chat, or put the keys back in it.
    pub(crate) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.page != Page::Chat || self.settings_open {
            return;
        }
        if let Some(finder) = &self.finder {
            let input = finder.input.clone();
            input.update(cx, |input, cx| {
                input.focus(window, cx);
                input.select_all(window, cx);
            });
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in this chat"));
        let typed = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, _, cx| match event {
                InputEvent::Change => {
                    if let Some(finder) = this.finder.as_mut() {
                        finder.at = None;
                    }
                    this.find_step(true, cx);
                }
                InputEvent::PressEnter { shift, .. } => this.find_step(!*shift, cx),
                _ => {}
            },
        );
        input.update(cx, |input, cx| input.focus(window, cx));
        self.finder = Some(Finder {
            input,
            at: None,
            _typed: typed,
        });
        cx.notify();
    }

    /// Close the find bar and let the chat go from the match it holds.
    pub(crate) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finder = None;
        self.reveal = None;
        self.revealed = None;
        self.hit_at = None;
        self.focus_composer(window, cx);
        cx.notify();
    }

    /// Show the match before the one on screen, going `up`, or after it.
    fn find_step(&mut self, up: bool, cx: &mut Context<Self>) {
        let Some(finder) = self.finder.as_ref() else {
            return;
        };
        let query = finder.input.read(cx).value().to_string();
        let open = &self.store.conversations[self.store.active];
        let (id, hits) = (open.id, matching(&open.messages, &query));
        let at = step(&hits, finder.at, up);
        if let Some(finder) = self.finder.as_mut() {
            finder.at = at;
        }
        if let Some(at) = at {
            self.following = false;
            self.reveal = Some((id, at));
            self.revealed = None;
            self.hit_at = None;
        }
        cx.notify();
    }

    /// The bar over the chat's top corner, while it is open.
    pub(crate) fn find_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let finder = self.finder.as_ref()?;
        let query = finder.input.read(cx).value().to_string();
        let open = &self.store.conversations[self.store.active];
        let hits = matching(&open.messages, &query);
        let said = count(&hits, finder.at, !query.trim().is_empty());
        let none = hits.is_empty();
        Some(
            h_flex()
                .absolute()
                .top_2()
                .right_4()
                .w(px(340.))
                .gap_1()
                .items_center()
                .p_1()
                .bg(cx.theme().popover)
                .border_1()
                .border_color(cx.theme().border)
                .rounded(cx.theme().radius)
                .child(
                    Input::new(&finder.input)
                        .xsmall()
                        .appearance(false)
                        .flex_1()
                        .prefix(
                            Icon::new(AppIcon::Search)
                                .xsmall()
                                .text_color(cx.theme().muted_foreground),
                        ),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(said),
                )
                .child(
                    Button::new("find-older")
                        .plain(cx)
                        .xsmall()
                        .icon(Icon::new(IconName::ChevronUp))
                        .tooltip("Older match (Enter)")
                        .disabled(none)
                        .on_click(cx.listener(|this, _, _, cx| this.find_step(true, cx))),
                )
                .child(
                    Button::new("find-newer")
                        .plain(cx)
                        .xsmall()
                        .icon(Icon::new(IconName::ChevronDown))
                        .tooltip("Newer match (Shift+Enter)")
                        .disabled(none)
                        .on_click(cx.listener(|this, _, _, cx| this.find_step(false, cx))),
                )
                .child(
                    Button::new("find-close")
                        .plain(cx)
                        .xsmall()
                        .icon(Icon::new(IconName::Close))
                        .tooltip("Close (Esc)")
                        .on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx))),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{count, matching, step};
    use letronna_core::chat::Message;

    fn said(role: &str, content: &str) -> Message {
        Message {
            role: role.into(),
            content: content.into(),
            ..Default::default()
        }
    }

    #[test]
    fn what_was_said_is_found_ignoring_case_and_tool_output_is_not() {
        let messages = [
            said("user", "Make the Tetris board wider"),
            said("assistant", "Widened the board to 12 columns."),
            said("tool", "board.rs: 12 columns"),
            said("user", "Now the colours"),
        ];
        assert_eq!(matching(&messages, "BOARD"), vec![0, 1]);
        assert_eq!(matching(&messages, "  colours "), vec![3]);
        assert!(matching(&messages, "").is_empty());
        assert!(matching(&messages, "nothing like it").is_empty());
    }

    #[test]
    fn finding_starts_at_the_newest_match_and_goes_round_both_ways() {
        let hits = [2, 5, 9];
        assert_eq!(step(&hits, None, true), Some(9));
        assert_eq!(step(&hits, Some(9), true), Some(5));
        assert_eq!(step(&hits, Some(2), true), Some(9));
        assert_eq!(step(&hits, Some(9), false), Some(2));
        assert_eq!(step(&hits, Some(5), false), Some(9));
        assert_eq!(step(&[], None, true), None);
        assert_eq!(count(&hits, Some(5), true), "2 of 3");
        assert_eq!(count(&[], None, true), "No matches");
        assert_eq!(count(&[], None, false), "");
    }
}
