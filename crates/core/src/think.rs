//! Thinking a model writes inline, kept out of its answer.
//!
//! Local servers and some custom ones pass a reasoning model's thinking on as
//! `<think>...</think>` (or `<thinking>...</thinking>`) in the answer itself
//! rather than in a reasoning field. Shown, it buries the answer; kept, it is
//! sent back as what the assistant said, and the model reads its own thinking
//! as something it told the user. So the stream is split as it arrives: a
//! block goes to the reasoning, the rest to the answer.
//!
//! A tag counts only where a block could start -- the start of the reply or
//! of a line, after any indent -- so a reply that mentions `<think>` in a
//! sentence keeps it. A tag split across chunks is waited for, holding back
//! only the few characters that could still become one; everything else
//! passes on as it came. A block never closed is thinking cut short, so it
//! stays reasoning.

/// The tags that open a hidden block, each with the tag that closes it.
const TAGS: [(&str, &str); 2] = [("<think>", "</think>"), ("<thinking>", "</thinking>")];

/// A piece of the reply, in the order it came.
#[derive(Debug, PartialEq)]
pub(crate) enum Piece {
    Answer(String),
    Thought(String),
}

/// A reply's text split into answer and thinking as it streams in.
#[derive(Default)]
pub(crate) struct Scrubber {
    /// Text that could still be part of a tag, held until more arrives.
    held: String,
    /// The tag that ends the block being hidden, while one is open.
    closing: Option<&'static str>,
    /// Whether anything but indent has come since the start or the last
    /// newline, which rules out a block opening here.
    mid_line: bool,
    /// Whether a block has just closed and the answer shown nothing since,
    /// so the blank lines a model leaves after one are dropped.
    after_block: bool,
    /// The indent of the line under way after a block, once a newline has
    /// come, held until that line shows whether it has text to indent.
    indent: Option<String>,
}

impl Scrubber {
    /// Take in one chunk of the answer, giving back what can be passed on.
    pub(crate) fn feed(&mut self, text: &str) -> Vec<Piece> {
        self.held.push_str(text);
        let mut pieces = Vec::new();
        loop {
            if let Some(close) = self.closing {
                let Some(at) = self.held.find(close) else {
                    let keep = self.held.len() - partial_suffix(&self.held, close);
                    let thought = self.held.drain(..keep).collect();
                    push(&mut pieces, Piece::Thought(thought));
                    return pieces;
                };
                let thought = self.held.drain(..at).collect();
                push(&mut pieces, Piece::Thought(thought));
                self.held.drain(..close.len());
                self.closing = None;
                self.mid_line = false;
                self.after_block = true;
                self.indent = None;
                continue;
            }
            let mut stop = None;
            for (at, c) in self.held.char_indices() {
                if !self.mid_line && c == '<' {
                    let rest = &self.held[at..];
                    if let Some((open, close)) =
                        TAGS.iter().find(|(open, _)| rest.starts_with(open))
                    {
                        stop = Some((at, Some((open.len(), *close))));
                        break;
                    }
                    if TAGS.iter().any(|(open, _)| open.starts_with(rest)) {
                        stop = Some((at, None));
                        break;
                    }
                }
                self.mid_line = match c {
                    '\n' => false,
                    c if c.is_whitespace() => self.mid_line,
                    _ => true,
                };
            }
            let end = stop.map_or(self.held.len(), |(at, _)| at);
            let shown = self.held.drain(..end).collect();
            self.answer(&mut pieces, shown);
            let Some((_, Some((open, close)))) = stop else {
                return pieces;
            };
            self.held.drain(..open);
            self.closing = Some(close);
        }
    }

    /// Give back what was held once the stream has ended.
    pub(crate) fn finish(&mut self) -> Vec<Piece> {
        let rest = std::mem::take(&mut self.held);
        let mut pieces = Vec::new();
        if self.closing.is_some() {
            push(&mut pieces, Piece::Thought(rest));
        } else {
            self.answer(&mut pieces, rest);
        }
        pieces
    }

    fn answer(&mut self, pieces: &mut Vec<Piece>, text: String) {
        if !self.after_block {
            push(pieces, Piece::Answer(text));
            return;
        }
        let rest = text.trim_start();
        let blank = &text[..text.len() - rest.len()];
        // Only the blank lines go: the indent of the line after them is the
        // answer's own, and may make it code or a nested list.
        match blank.rfind('\n') {
            Some(at) => self.indent = Some(blank[at + 1..].to_owned()),
            None => {
                if let Some(indent) = &mut self.indent {
                    indent.push_str(blank);
                }
            }
        }
        if rest.is_empty() {
            return;
        }
        self.after_block = false;
        let indent = self.indent.take().unwrap_or_default();
        push(pieces, Piece::Answer(indent + rest));
    }
}

/// Add a piece, joined to the last one when it is of the same kind.
fn push(pieces: &mut Vec<Piece>, piece: Piece) {
    match (pieces.last_mut(), piece) {
        (_, Piece::Answer(t) | Piece::Thought(t)) if t.is_empty() => {}
        (Some(Piece::Answer(last)), Piece::Answer(t)) => last.push_str(&t),
        (Some(Piece::Thought(last)), Piece::Thought(t)) => last.push_str(&t),
        (_, piece) => pieces.push(piece),
    }
}

/// How many bytes at the end of `text` could be the start of `tag`.
fn partial_suffix(text: &str, tag: &str) -> usize {
    (1..tag.len())
        .rev()
        .find(|&n| text.ends_with(&tag[..n]))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stream `text` in chunks of `size` characters, giving back the answer
    /// and the thinking.
    fn stream(text: &str, size: usize) -> (String, String) {
        let chars: Vec<char> = text.chars().collect();
        let mut scrubber = Scrubber::default();
        let mut pieces = Vec::new();
        for chunk in chars.chunks(size) {
            pieces.extend(scrubber.feed(&chunk.iter().collect::<String>()));
        }
        pieces.extend(scrubber.finish());
        let (mut answer, mut thought) = (String::new(), String::new());
        for piece in pieces {
            match piece {
                Piece::Answer(t) => answer.push_str(&t),
                Piece::Thought(t) => thought.push_str(&t),
            }
        }
        (answer, thought)
    }

    /// Stream `text` in chunks of every size, checking each gives the same.
    fn every_size(text: &str, answer: &str, thought: &str) {
        for size in 1..=text.chars().count() {
            let got = stream(text, size);
            assert_eq!(got, (answer.into(), thought.into()), "chunks of {size}");
        }
    }

    #[test]
    fn a_block_split_across_chunks_of_any_size_is_thinking() {
        every_size(
            "<think>\nWork it out.\n</think>\n\nThe answer is 4.",
            "The answer is 4.",
            "\nWork it out.\n",
        );
    }

    #[test]
    fn a_thinking_block_closes_only_on_its_own_tag() {
        every_size(
            "<thinking>a </think> b</thinking>Done.",
            "Done.",
            "a </think> b",
        );
    }

    #[test]
    fn a_tag_mid_sentence_stays_in_the_answer() {
        let text = "Wrap it in <think> tags.\nThen </think> ends it.";
        every_size(text, text, "");
    }

    #[test]
    fn an_unclosed_block_is_thinking() {
        every_size("<think>Still going", "", "Still going");
        every_size("<think>Cut off at </thin", "", "Cut off at </thin");
    }

    #[test]
    fn the_answer_after_a_block_keeps_its_lines() {
        every_size(
            "First.\n  <think>Hmm.</think>\nSecond.\n<thinking>More.</thinking>Third.",
            "First.\n  Second.\nThird.",
            "Hmm.More.",
        );
    }

    #[test]
    fn a_reply_without_blocks_passes_through_byte_for_byte() {
        let text = "Héllo — ✓\n<b>bold</b>\n  <thin air\n<\n<think about it\nend <thin";
        every_size(text, text, "");
    }

    #[test]
    fn only_what_could_be_a_tag_is_held_back() {
        let mut scrubber = Scrubber::default();
        assert_eq!(
            scrubber.feed("Less <"),
            vec![Piece::Answer("Less <".into())]
        );
        assert_eq!(scrubber.feed("\n<thi"), vec![Piece::Answer("\n".into())]);
        assert_eq!(scrubber.feed("s"), vec![Piece::Answer("<this".into())]);
    }

    #[test]
    fn the_first_line_after_a_block_keeps_its_indent() {
        every_size(
            "<think>Plan.</think>\n\n    let x = 1;\n",
            "    let x = 1;\n",
            "Plan.",
        );
        every_size("<think>Plan.</think>  Hello", "Hello", "Plan.");
    }
}
