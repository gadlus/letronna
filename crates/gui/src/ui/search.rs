//! What the sidebar's search finds in what was said, under the sessions
//! whose titles match, and the chat held at the message a hit points to.
//!
//! The ranking is core's `store::search`, which reads every session file,
//! so it runs on the background executor and only once typing pauses: each
//! key replaces the one waiting search, which drops, and so cancels, the
//! last. Nothing is left running between searches.
//!
//! A hit opens its session scrolled to the message, which is found where it
//! is drawn: a marker on its row measures it whenever the chat is drawn and
//! moves the view until it sits at the top, then asks for no more frames.
//! The markdown above it lays out a moment late, so the chat stays held
//! there until it is scrolled by hand.

use crate::prelude::*;
use letronna_core::store::Hit;

actions!(search, [OpenHit]);

/// Characters typed before what was said is searched: fewer match nearly
/// every message, and each search reads every session file.
const MIN_QUERY: usize = 3;
/// How long typing must pause before a search, so a word typed at speed
/// reads the session files once rather than once a key.
const PAUSE: std::time::Duration = std::time::Duration::from_millis(250);
/// Hits listed at most: about a screen of the sidebar, three a session.
const HITS: usize = 12;
/// Room above a message opened from a search, so the line before it shows
/// and the message does not start against the top edge.
const HIT_ABOVE: Pixels = px(24.);
/// Characters of a snippet kept before its first match, so the match sits in
/// the two lines the sidebar shows rather than past them.
const LEAD: usize = 24;

/// A search hit the sidebar lists, with the session it opens.
pub(crate) struct Found {
    hit: Hit,
    /// Makes the hit a tab stop, so the keyboard reaches it from the box.
    focus: FocusHandle,
}

impl Chat {
    /// Search what was said for the sidebar's query once typing pauses,
    /// off the UI thread, in place of any search still waiting or running.
    pub(crate) fn find_in_messages(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).value().trim().to_owned();
        if query.chars().count() < MIN_QUERY {
            self.finding = None;
            self.found.clear();
            return;
        }
        self.finding = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PAUSE).await;
            let hits = cx
                .background_executor()
                .spawn(
                    async move { letronna_core::store::search(&query, &Within::default(), HITS) },
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                this.found = hits
                    .into_iter()
                    // A title match is already in the sessions above.
                    .filter(|hit| hit.ordinal.is_some())
                    .map(|hit| Found {
                        hit,
                        focus: cx.focus_handle().tab_stop(true),
                    })
                    .collect();
                cx.notify();
            });
        }));
    }

    /// The "In messages" group, a row a hit in a session the window holds,
    /// or nothing while there are none.
    pub(crate) fn message_hits(&self, cx: &mut Context<Self>) -> Option<Div> {
        let query = self.search.read(cx).value().to_string();
        let mut rows = Vec::new();
        for found in &self.found {
            // One the terminal started since the window opened is not in the
            // sidebar, and so not here either.
            let Some(c) = self
                .store
                .conversations
                .iter()
                .find(|c| c.id == found.hit.id)
            else {
                continue;
            };
            rows.push(self.hit_row(rows.len(), found, &c.title, &query, cx));
        }
        if rows.is_empty() {
            return None;
        }
        Some(
            v_flex()
                .child(self.section("In messages", cx))
                .child(v_flex().gap_0p5().children(rows)),
        )
    }

    /// A hit: its session's title over what was said. Enter or a click opens
    /// the session there; a click also goes on to the composer, as a session
    /// row's does, while Enter leaves the keys here for the next hit.
    fn hit_row(
        &self,
        n: usize,
        found: &Found,
        title: &str,
        query: &str,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let (id, ordinal) = (found.hit.id, found.hit.ordinal.unwrap_or_default());
        let accent = cx.theme().sidebar_accent;
        v_flex()
            .id(("hit", n))
            .track_focus(&found.focus)
            .key_context("SearchHit")
            .px_2()
            .py_1()
            .rounded(cx.theme().radius)
            .cursor_pointer()
            .focus(|s| s.bg(accent))
            .active(|s| s.bg(accent.opacity(0.6)))
            .child(div().text_sm().truncate().child(title.to_owned()))
            .child(
                div()
                    .text_xs()
                    .line_clamp(2)
                    .text_color(cx.theme().muted_foreground)
                    .child(from_match(&found.hit.snippet, query)),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_hit(id, ordinal, cx);
                this.focus_composer(window, cx);
            }))
            .on_action(cx.listener(move |this, _: &OpenHit, _, cx| {
                this.open_hit(id, ordinal, cx);
            }))
    }

    /// Open session `id` at message `ordinal`, or at its newest message if
    /// it no longer has that many.
    fn open_hit(&mut self, id: u64, ordinal: usize, cx: &mut Context<Self>) {
        let Some(index) = self.store.conversations.iter().position(|c| c.id == id) else {
            return;
        };
        self.store.active = index;
        self.error = None;
        self.page = Page::Chat;
        if ordinal < self.store.conversations[index].messages.len() {
            self.following = false;
            self.reveal = Some((id, ordinal));
            self.revealed = None;
        } else {
            self.follow();
        }
        self.save();
        cx.notify();
    }

    /// `row` with a marker that scrolls the chat to it, for the message a
    /// search hit opened.
    pub(crate) fn mark_hit(&self, row: AnyElement, cx: &mut Context<Self>) -> AnyElement {
        let chat = cx.entity();
        let marker = canvas(
            move |bounds, window, cx| {
                chat.update(cx, |chat, _| chat.place_hit(bounds.top(), window));
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        div()
            .relative()
            .w_full()
            .child(row)
            .child(marker)
            .into_any_element()
    }

    /// Scroll the chat so the message drawn at `top` sits near the top of
    /// the view, or as near as the end of the chat lets it.
    fn place_hit(&mut self, top: Pixels, window: &mut Window) {
        let offset = self.scroll.offset();
        let max = self.scroll.max_offset().height;
        let wanted = top - self.scroll.bounds().top() - HIT_ABOVE;
        let y = (offset.y - wanted).clamp(-max, px(0.));
        // Within a pixel is there; moving again would ask for frames
        // forever over a rounding difference.
        if (y - offset.y).abs() < px(1.) {
            self.revealed = Some(offset.y);
            return;
        }
        self.scroll.set_offset(point(offset.x, y));
        self.revealed = Some(y);
        window.request_animation_frame();
    }

    /// Let go of a message opened from a search once the chat has been
    /// scrolled away from where it was put, or shows another session; true
    /// while it is still held.
    pub(crate) fn holding_hit(&mut self) -> bool {
        let shown = self.store.conversations[self.store.active].id;
        let scrolled = self.revealed.is_some_and(|y| y != self.scroll.offset().y);
        if scrolled || self.reveal.is_some_and(|(id, _)| id != shown) {
            self.reveal = None;
            self.revealed = None;
        }
        self.reveal.is_some()
    }
}

/// `snippet` from a little before the first of `query`'s words it holds,
/// ignoring case, so the match falls in the lines the sidebar shows.
fn from_match(snippet: &str, query: &str) -> String {
    let lower = snippet.to_lowercase();
    let first = query
        .split(|c: char| c.is_whitespace() || c == '"')
        .map(|word| word.trim_matches(|c: char| c.is_ascii_punctuation()))
        .filter(|word| !word.is_empty())
        .filter_map(|word| lower.find(&word.to_lowercase()))
        .min();
    // Counted in characters, as core cuts its snippets.
    let skip = first.map_or(0, |at| lower[..at].chars().count().saturating_sub(LEAD));
    if skip == 0 {
        return snippet.to_owned();
    }
    let rest: String = snippet.chars().skip(skip).collect();
    format!("…{}", rest.trim_start())
}

#[cfg(test)]
mod tests {
    use super::{LEAD, from_match};

    #[test]
    fn a_hit_is_shown_from_just_before_its_match() {
        let snippet = format!("…{} where the Tauri build failed", "x".repeat(120));
        let shown = from_match(&snippet, "build tauri");
        assert!(shown.starts_with('…'));
        assert!(shown.contains("the Tauri build failed"));
        assert_eq!(shown.chars().count(), 1 + LEAD + "Tauri build failed".len());
        // Near the start already, or holding none of the words: as it is.
        assert_eq!(from_match("tauri build", "build"), "tauri build");
        assert_eq!(from_match("nothing here", "tauri"), "nothing here");
    }
}
