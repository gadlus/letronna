//! Pacing a reply's text onto the screen. A model's stream comes in bursts --
//! a sentence at once, then a pause -- and shown as it lands the reply moves
//! in clumps. So the text is held in a queue and let out a little at every
//! frame, enough each time to empty what waits within `DRAIN`: a burst flows
//! out quickly, a slow model is never left behind, and what came after the
//! text (a tool call, the end of the reply) waits only until the text before
//! it is out.

use std::collections::VecDeque;
use std::time::Duration;

use letronna_core::chat::Event;

/// How often a piece is let out: about a display refresh.
pub(crate) const FRAME: Duration = Duration::from_millis(16);
/// How long whatever waits takes to come out, however much it is.
const DRAIN: Duration = Duration::from_millis(280);

/// The events waiting their turn, text and the rest in the order they came.
#[derive(Default)]
pub(crate) struct Pacer {
    waiting: VecDeque<Event>,
    /// Characters a frame, set as text comes in and kept until the queue is
    /// empty, so it drains at an even pace rather than slowing as it goes.
    rate: usize,
}

impl Pacer {
    pub(crate) fn push(&mut self, event: Event) {
        let text = matches!(event, Event::Delta(_));
        // Text after text is one run to let out, not a run per piece.
        match (event, self.waiting.back_mut()) {
            (Event::Delta(more), Some(Event::Delta(last))) => last.push_str(&more),
            (event, _) => self.waiting.push_back(event),
        }
        if text {
            self.rate = self.rate.max(piece(self.text_waiting()));
        }
    }

    /// Everything waiting at once, for when motion is reduced.
    pub(crate) fn all(&mut self) -> Vec<Event> {
        self.waiting.drain(..).collect()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.waiting.is_empty()
    }

    /// What goes on screen this frame: what waits up to the first text, and a
    /// piece of that text sized to empty the queue within `DRAIN`.
    pub(crate) fn next(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        while let Some(event) = self.waiting.pop_front() {
            let Event::Delta(mut text) = event else {
                out.push(event);
                continue;
            };
            let take = self.rate.max(1);
            let cut = text
                .char_indices()
                .nth(take)
                .map_or(text.len(), |(at, _)| at);
            let rest = text.split_off(cut);
            out.push(Event::Delta(text));
            if !rest.is_empty() {
                self.waiting.push_front(Event::Delta(rest));
            }
            break;
        }
        if self.text_waiting() == 0 {
            self.rate = 0;
        }
        out
    }

    fn text_waiting(&self) -> usize {
        (self.waiting.iter())
            .map(|e| match e {
                Event::Delta(text) => text.chars().count(),
                _ => 0,
            })
            .sum()
    }
}

/// How many characters to let out this frame with `waiting` in the queue.
fn piece(waiting: usize) -> usize {
    let frames = (DRAIN.as_millis() / FRAME.as_millis()).max(1) as usize;
    waiting.div_ceil(frames).max(1)
}

#[cfg(test)]
mod tests {
    use super::{Pacer, piece};
    use letronna_core::chat::Event;

    fn text(events: &[Event]) -> String {
        (events.iter())
            .filter_map(|e| match e {
                Event::Delta(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_burst_comes_out_in_pieces_and_whole() {
        let mut pacer = Pacer::default();
        let burst = "The quick brown fox jumps over the lazy dog, twice over. ".repeat(4);
        pacer.push(Event::Delta(burst.clone()));
        let mut shown = String::new();
        let mut frames = 0;
        while !pacer.is_empty() {
            shown += &text(&pacer.next());
            frames += 1;
        }
        assert_eq!(shown, burst);
        assert!(frames > 5 && frames <= 18, "{frames} frames");
    }

    #[test]
    fn what_follows_the_text_waits_for_it() {
        let mut pacer = Pacer::default();
        pacer.push(Event::Delta("héllo wörld, ".repeat(10)));
        pacer.push(Event::Finished(Ok(())));
        let mut last = None;
        while !pacer.is_empty() {
            for e in pacer.next() {
                last = Some(matches!(e, Event::Finished(_)));
                if matches!(e, Event::Finished(_)) {
                    assert!(pacer.is_empty(), "finished before the text was out");
                }
            }
        }
        assert_eq!(last, Some(true));
    }

    #[test]
    fn a_little_waiting_still_moves() {
        assert_eq!(piece(0), 1);
        assert_eq!(piece(1), 1);
        assert!(piece(1000) >= 50);
    }
}
