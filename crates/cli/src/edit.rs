//! The line being typed.
//!
//! Text, a cursor in it, the pastes standing behind their placeholders, and
//! the lines sent earlier. Kept apart from the drawing so it can be tested
//! without a terminal, which is most of what there is to get wrong here.

/// A pasted block is not put into the line. A placeholder goes there instead
/// and the text waits behind it, so pasting sixty lines does not bury the
/// sentence you were writing around it. The whole thing goes when you send.
#[derive(Clone)]
pub struct Paste {
    pub lines: usize,
    pub text: String,
}

/// What the last change to the line was, so a run of typing, or of
/// deleting, is undone as one.
#[derive(Clone, Copy, Default, PartialEq)]
enum Edit {
    #[default]
    Other,
    Typing,
    Deleting,
}

/// The most changes undo goes back through.
const UNDO: usize = 100;

/// Prompts an opened session puts back under up, the newest of them: more
/// than anyone steps back through a key at a time, and few enough that a
/// session of thousands of messages is not copied whole into the box.
const HISTORY: usize = 100;

#[derive(Default)]
pub struct Line {
    text: String,
    /// Where the next character goes, counted in characters rather than
    /// bytes, because that is how the cursor moves on the screen.
    cursor: usize,
    pastes: Vec<Paste>,
    /// Lines already sent, oldest first.
    history: Vec<String>,
    /// How far back through them we are; `None` is the line being written.
    recalled: Option<usize>,
    /// What was being written before the first recall, so it can come back.
    draft: String,
    /// The line as it was before each change, for undo: its text, cursor
    /// and pastes.
    undone: Vec<(String, usize, Vec<Paste>)>,
    last: Edit,
    /// What ctrl-u, ctrl-k or ctrl-w last cut, which ctrl-y puts back.
    killed: String,
}

impl Line {
    pub fn text(&self) -> &str {
        &self.text
    }

    #[cfg(test)]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn chars(&self) -> Vec<char> {
        self.text.chars().collect()
    }

    fn byte_at(&self, index: usize) -> usize {
        self.text
            .char_indices()
            .nth(index)
            .map(|(at, _)| at)
            .unwrap_or(self.text.len())
    }

    /// Keep the line as it is for undo before a change of `kind`: a change
    /// that goes on a run of the same kind is undone with it, and a word
    /// typed is a run of its own.
    fn keep(&mut self, kind: Edit, starts_a_word: bool) {
        if kind == Edit::Other || kind != self.last || starts_a_word {
            self.undone
                .push((self.text.clone(), self.cursor, self.pastes.clone()));
            let over = self.undone.len().saturating_sub(UNDO);
            self.undone.drain(..over);
        }
        self.last = kind;
    }

    /// Put the line back as it was before the last change, or the run of
    /// changes it ended. Says whether there was one.
    pub fn undo(&mut self) -> bool {
        let Some((text, cursor, pastes)) = self.undone.pop() else {
            return false;
        };
        self.text = text;
        self.cursor = cursor;
        self.pastes = pastes;
        self.last = Edit::Other;
        true
    }

    /// Put back what ctrl-u, ctrl-k or ctrl-w last cut, at the cursor.
    pub fn yank(&mut self) {
        let killed = self.killed.clone();
        self.insert_str(&killed);
    }

    pub fn insert(&mut self, ch: char) {
        self.keep(Edit::Typing, ch == ' ');
        let at = self.byte_at(self.cursor);
        self.text.insert(at, ch);
        self.cursor += 1;
    }

    pub fn insert_str(&mut self, text: &str) {
        self.keep(Edit::Other, false);
        self.insert_raw(text);
    }

    fn insert_raw(&mut self, text: &str) {
        let at = self.byte_at(self.cursor);
        self.text.insert_str(at, text);
        self.cursor += text.chars().count();
    }

    /// Take a pasted block. One line goes in as it is -- a placeholder would
    /// be longer than the thing it stood for.
    pub fn paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let lines = text.lines().count();
        if lines < 2 {
            self.insert_str(&text);
            return;
        }
        self.pastes.push(Paste { lines, text });
        let marker = format!("[Pasted #{} +{lines} lines]", self.pastes.len());
        self.insert_str(&marker);
    }

    /// Where the cursor is, for marking the start of something arriving.
    pub fn at(&self) -> usize {
        self.cursor
    }

    /// Take everything typed since `from` and stand a placeholder in its
    /// place, if there is enough of it to be worth hiding.
    ///
    /// This is how a paste is caught on a Windows console, where pasted text
    /// arrives as ordinary keystrokes and there is no paste event to listen
    /// for: a burst that put several lines in was not typed by a person.
    pub fn fold_burst(&mut self, from: usize) {
        if from >= self.cursor {
            return;
        }
        let chars = self.chars();
        let burst: String = chars[from..self.cursor].iter().collect();
        if burst.lines().count() < 2 {
            return;
        }
        self.cut(from, self.cursor);
        self.paste(&burst);
    }

    /// The line as it should be sent: placeholders swapped back for what they
    /// stand for.
    pub fn expand(&self) -> String {
        let mut out = self.text.clone();
        for (i, paste) in self.pastes.iter().enumerate() {
            out = out.replace(
                &format!("[Pasted #{} +{} lines]", i + 1, paste.lines),
                &paste.text,
            );
        }
        out
    }

    pub fn clear(&mut self) {
        if !self.text.is_empty() {
            self.keep(Edit::Other, false);
        }
        self.text.clear();
        self.cursor = 0;
        self.pastes.clear();
        self.recalled = None;
    }

    pub fn set(&mut self, text: &str) {
        if text != self.text {
            self.keep(Edit::Other, false);
        }
        self.put(text);
    }

    /// The line made `text` with its pastes written out in it, as the editor
    /// hands a draft back: the pastes behind the old placeholders go, so the
    /// next paste is numbered from one and a placeholder left in the text
    /// stays words. An undo brings them back with the line.
    pub fn set_expanded(&mut self, text: &str) {
        self.set(text);
        self.pastes.clear();
    }

    /// The line made `text`, the cursor at its end, not kept for undo: for
    /// stepping through what was sent before, which up and down undo.
    fn put(&mut self, text: &str) {
        self.text = text.to_owned();
        self.move_to(self.text.chars().count());
    }

    /// Put the cursor at `at`. A move ends a run of typing or deleting, so
    /// what is typed after it is undone apart from what came before.
    fn move_to(&mut self, at: usize) {
        self.cursor = at;
        self.last = Edit::Other;
    }

    /// Put the cursor at character `at`, as a move between rows does.
    pub fn put_cursor(&mut self, at: usize) {
        self.cursor = at.min(self.text.chars().count());
    }

    pub fn left(&mut self) {
        self.move_to(self.cursor.saturating_sub(1));
    }

    pub fn right(&mut self) {
        self.move_to((self.cursor + 1).min(self.text.chars().count()));
    }

    pub fn home(&mut self) {
        self.move_to(0);
    }

    pub fn end(&mut self) {
        self.move_to(self.text.chars().count());
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.keep(Edit::Deleting, false);
        // A placeholder goes whole: deleting it one character at a time would
        // leave wreckage that still stands for sixty lines.
        if let Some(start) = self.marker_before() {
            self.cut(start, self.cursor);
            return;
        }
        self.cut(self.cursor - 1, self.cursor);
    }

    pub fn delete(&mut self) {
        let len = self.text.chars().count();
        if self.cursor < len {
            self.keep(Edit::Deleting, false);
            self.cut(self.cursor, self.cursor + 1);
        }
    }

    /// Delete back to the start of the word the cursor is in.
    pub fn backspace_word(&mut self) {
        if let Some(start) = self.marker_before() {
            self.cut_kept(start, self.cursor);
            return;
        }
        let start = self.word_start();
        self.cut_kept(start, self.cursor);
    }

    /// Delete forward to the end of the word ahead of the cursor.
    pub fn delete_word(&mut self) {
        let end = self.word_end();
        self.cut_kept(self.cursor, end);
    }

    /// Move to the start of the row the cursor is on.
    pub fn row_start(&mut self) {
        self.move_to(self.row().0);
    }

    /// Move to the end of the row the cursor is on.
    pub fn row_end(&mut self) {
        self.move_to(self.row().1);
    }

    /// Delete back to the start of the row the cursor is on.
    pub fn backspace_to_row_start(&mut self) {
        let (start, _) = self.row();
        self.cut_kept(start, self.cursor);
    }

    /// Delete forward to the end of the row the cursor is on.
    pub fn delete_to_row_end(&mut self) {
        let (_, end) = self.row();
        self.cut_kept(self.cursor, end);
    }

    /// Cut from `from` to `to`, keeping the line for undo and what was cut
    /// for ctrl-y, as a shell does.
    fn cut_kept(&mut self, from: usize, to: usize) {
        if from >= to {
            return;
        }
        self.keep(Edit::Other, false);
        self.killed = self.chars()[from..to].iter().collect();
        self.cut(from, to);
    }

    pub fn word_left(&mut self) {
        self.move_to(self.word_start());
    }

    pub fn word_right(&mut self) {
        self.move_to(self.word_end());
    }

    /// What follows the `@` of the reference the cursor ends, if it ends
    /// one; a space after a backslash is part of it.
    pub fn reference_before(&self) -> Option<String> {
        let start = self.reference_start();
        let word: String = self.chars()[start..self.cursor].iter().collect();
        word.strip_prefix('@').map(str::to_owned)
    }

    /// Put `with` in place of what follows the `@` the cursor's word starts with.
    pub fn replace_reference(&mut self, with: &str) {
        self.keep(Edit::Other, false);
        let start = self.reference_start() + 1;
        self.cut(start.min(self.cursor), self.cursor);
        self.insert_raw(with);
    }

    fn reference_start(&self) -> usize {
        let chars = self.chars();
        let mut at = self.cursor;
        while at > 0 && !(chars[at - 1].is_whitespace() && (at < 2 || chars[at - 2] != '\\')) {
            at -= 1;
        }
        at
    }

    /// Where the row the cursor is on starts and ends, between line endings.
    fn row(&self) -> (usize, usize) {
        let chars = self.chars();
        let start = chars[..self.cursor]
            .iter()
            .rposition(|c| *c == '\n')
            .map_or(0, |at| at + 1);
        let end = chars[self.cursor..]
            .iter()
            .position(|c| *c == '\n')
            .map_or(chars.len(), |at| self.cursor + at);
        (start, end)
    }

    fn word_start(&self) -> usize {
        let chars = self.chars();
        let mut at = self.cursor;
        while at > 0 && chars[at - 1].is_whitespace() {
            at -= 1;
        }
        while at > 0 && !chars[at - 1].is_whitespace() {
            at -= 1;
        }
        at
    }

    fn word_end(&self) -> usize {
        let chars = self.chars();
        let mut at = self.cursor;
        while at < chars.len() && chars[at].is_whitespace() {
            at += 1;
        }
        while at < chars.len() && !chars[at].is_whitespace() {
            at += 1;
        }
        at
    }

    /// The start of a paste placeholder ending right at the cursor.
    fn marker_before(&self) -> Option<usize> {
        let chars = self.chars();
        if self.cursor == 0 || chars.get(self.cursor - 1) != Some(&']') {
            return None;
        }
        let open = chars[..self.cursor - 1].iter().rposition(|c| *c == '[')?;
        let inside: String = chars[open + 1..self.cursor - 1].iter().collect();
        inside.starts_with("Pasted #").then_some(open)
    }

    fn cut(&mut self, from: usize, to: usize) {
        let (from_byte, to_byte) = (self.byte_at(from), self.byte_at(to));
        self.text.replace_range(from_byte..to_byte, "");
        self.cursor = from;
    }

    /// Where the cursor sits as a row and column within the wrapped line.
    pub fn line_of_cursor(&self) -> (usize, usize) {
        let before: String = self.chars()[..self.cursor].iter().collect();
        let row = before.matches('\n').count();
        let column = before
            .rsplit('\n')
            .next()
            .unwrap_or_default()
            .chars()
            .count();
        (row, column)
    }

    pub fn up(&mut self) {
        self.step(-1);
    }

    pub fn down(&mut self) {
        self.step(1);
    }

    /// Move the cursor a line, within the text where there is one to move to
    /// and through what was sent earlier otherwise.
    fn step(&mut self, by: isize) {
        if self.text.contains('\n') {
            let (row, column) = self.line_of_cursor();
            let rows: Vec<&str> = self.text.split('\n').collect();
            let wanted = row as isize + by;
            if wanted >= 0 && (wanted as usize) < rows.len() {
                let wanted = wanted as usize;
                let before: usize = rows[..wanted].iter().map(|r| r.chars().count() + 1).sum();
                self.move_to(before + column.min(rows[wanted].chars().count()));
                return;
            }
        }
        self.recall(by);
    }

    /// Step through what was sent earlier. Up goes back, and coming forward
    /// past the newest brings back whatever was being written at the time.
    fn recall(&mut self, by: isize) {
        if self.history.is_empty() {
            return;
        }
        let newest = self.history.len() - 1;
        let next = match (self.recalled, by) {
            (None, -1) => {
                self.draft = self.text.clone();
                Some(newest)
            }
            // Nothing is newer than the line being written, so it stays put
            // rather than being swapped for an empty draft.
            (None, _) => return,
            (Some(0), -1) => Some(0),
            (Some(at), -1) => Some(at - 1),
            (Some(at), _) if at >= newest => None,
            (Some(at), _) => Some(at + 1),
        };
        self.recalled = next;
        match next {
            Some(at) => {
                let line = self.history[at].clone();
                self.put(&line);
            }
            None => {
                let draft = std::mem::take(&mut self.draft);
                self.put(&draft);
            }
        }
    }

    /// Clear the line, keeping what was typed among the lines sent earlier,
    /// pastes and all, so up brings it back. Says whether there was anything.
    pub fn shelve(&mut self) -> bool {
        let typed = self.expand();
        self.clear();
        if typed.trim().is_empty() {
            return false;
        }
        if self.history.last() != Some(&typed) {
            self.history.push(typed);
        }
        self.draft.clear();
        true
    }

    /// Take the line to send, remembering it, and start a fresh one.
    pub fn take(&mut self) -> String {
        let sent = self.expand();
        let typed = std::mem::take(&mut self.text);
        if !typed.trim().is_empty() && self.history.last() != Some(&typed) {
            self.history.push(typed);
        }
        self.cursor = 0;
        self.pastes.clear();
        self.recalled = None;
        self.draft.clear();
        // A line sent is no longer there to undo changes to.
        self.undone.clear();
        self.last = Edit::Other;
        sent
    }

    /// Start the lines sent earlier over from `sent`, oldest first, keeping
    /// the newest `HISTORY` of them.
    pub fn set_history(&mut self, sent: impl IntoIterator<Item = String>) {
        self.history.clear();
        for line in sent {
            if !line.trim().is_empty() && self.history.last() != Some(&line) {
                self.history.push(line);
            }
        }
        let over = self.history.len().saturating_sub(HISTORY);
        self.history.drain(..over);
        // A place among the old lines means nothing among these.
        self.recalled = None;
        self.draft.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{HISTORY, Line};

    fn typed(text: &str) -> Line {
        let mut line = Line::default();
        line.insert_str(text);
        line
    }

    #[test]
    fn undo_goes_back_a_word_or_a_run_of_deletes_at_a_time() {
        let mut line = Line::default();
        for ch in "hello world".chars() {
            line.insert(ch);
        }
        assert!(line.undo());
        assert_eq!(line.text(), "hello");
        for _ in 0..3 {
            line.backspace();
        }
        assert_eq!(line.text(), "he");
        assert!(line.undo());
        assert_eq!(line.text(), "hello");
        assert!(line.undo());
        assert_eq!(line.text(), "");
        assert!(!line.undo());
    }

    #[test]
    fn a_move_or_a_recalled_line_ends_a_run_of_typing() {
        let mut line = Line::default();
        line.insert('a');
        line.insert('b');
        line.left();
        line.insert('c');
        assert_eq!(line.text(), "acb");
        assert!(line.undo());
        assert_eq!(line.text(), "ab");
        line.end();
        line.take();
        line.insert('x');
        line.up();
        line.insert('!');
        assert_eq!(line.text(), "ab!");
        assert!(line.undo());
        assert_eq!(line.text(), "ab");
    }

    #[test]
    fn what_a_cut_took_is_put_back_by_yank() {
        let mut line = typed("one two three");
        line.backspace_word();
        assert_eq!(line.text(), "one two ");
        line.home();
        line.yank();
        assert_eq!(line.text(), "threeone two ");
        assert!(line.undo());
        assert_eq!(line.text(), "one two ");
        line.end();
        line.backspace_to_row_start();
        assert_eq!(line.text(), "");
        assert!(line.undo());
        assert_eq!(line.text(), "one two ");
        // A line sent leaves nothing to undo.
        line.take();
        assert!(!line.undo());
    }

    #[test]
    fn the_reference_the_cursor_ends_is_found_and_replaced() {
        let mut line = typed("see me@home and @src/ma");
        assert_eq!(line.reference_before().as_deref(), Some("src/ma"));
        line.replace_reference("src/main.rs");
        assert_eq!(line.text(), "see me@home and @src/main.rs");
        let mut spaced = typed("@my\\ no");
        assert_eq!(spaced.reference_before().as_deref(), Some("my\\ no"));
        spaced.replace_reference("my\\ notes.md");
        assert_eq!(spaced.text(), "@my\\ notes.md");
        assert_eq!(typed("mail me@home").reference_before(), None);
        assert_eq!(typed("word ").reference_before(), None);
    }

    #[test]
    fn a_pasted_block_stands_behind_a_placeholder() {
        let mut line = typed("before ");
        line.paste("one\ntwo\nthree");
        line.insert_str(" after");
        // Short in the box.
        assert_eq!(line.text(), "before [Pasted #1 +3 lines] after");
        // Whole when it goes.
        assert_eq!(line.expand(), "before one\ntwo\nthree after");
    }

    #[test]
    fn a_burst_of_keystrokes_is_a_paste() {
        // What a Windows console does with a paste: one character at a time.
        let mut line = typed("note: ");
        let mark = line.at();
        for ch in "one
two
three"
            .chars()
        {
            line.insert(ch);
        }
        line.fold_burst(mark);
        assert_eq!(line.text(), "note: [Pasted #1 +3 lines]");
        assert_eq!(
            line.expand(),
            "note: one
two
three"
        );
    }

    #[test]
    fn a_burst_on_one_line_is_left_alone() {
        // Typing quickly is not pasting.
        let mut line = Line::default();
        let mark = line.at();
        for ch in "quick".chars() {
            line.insert(ch);
        }
        line.fold_burst(mark);
        assert_eq!(line.text(), "quick");
    }

    #[test]
    fn one_pasted_line_needs_no_placeholder() {
        let mut line = Line::default();
        line.paste("just this");
        assert_eq!(line.text(), "just this");
    }

    #[test]
    fn backspace_takes_a_placeholder_whole() {
        let mut line = typed("keep ");
        line.paste("a\nb\nc");
        line.backspace();
        assert_eq!(line.text(), "keep ");
        // And the text it stood for goes with it.
        assert_eq!(line.expand(), "keep ");
    }

    #[test]
    fn a_word_goes_at_a_time() {
        let mut line = typed("one two three");
        line.backspace_word();
        assert_eq!(line.text(), "one two ");
        line.backspace_word();
        assert_eq!(line.text(), "one ");

        let mut line = typed("one two three");
        line.home();
        line.delete_word();
        assert_eq!(line.text(), " two three");
    }

    #[test]
    fn the_cursor_walks_by_word() {
        let mut line = typed("alpha beta gamma");
        line.home();
        line.word_right();
        assert_eq!(line.cursor(), 5);
        line.word_right();
        assert_eq!(line.cursor(), 10);
        line.word_left();
        assert_eq!(line.cursor(), 6);
    }

    #[test]
    fn the_arrows_walk_back_through_what_was_sent() {
        let mut line = typed("first");
        line.take();
        let mut line = { line };
        line.insert_str("second");
        line.take();

        // Half a thought, then a look back at the last two.
        line.insert_str("half");
        line.up();
        assert_eq!(line.text(), "second");
        line.up();
        assert_eq!(line.text(), "first");
        line.down();
        assert_eq!(line.text(), "second");
        // Forward past the newest brings the half-written line back.
        line.down();
        assert_eq!(line.text(), "half");
    }

    #[test]
    fn a_line_back_from_the_editor_has_no_pastes_behind_it() {
        let mut line = Line::default();
        line.paste("one\ntwo\nthree");
        assert_eq!(line.text(), "[Pasted #1 +3 lines]");
        let draft = format!("{}\nand [Pasted #1 +3 lines] as words", line.expand());
        line.set_expanded(&draft);
        assert_eq!(line.expand(), draft);
        line.paste("four\nfive");
        assert!(
            line.text().ends_with("[Pasted #1 +2 lines]"),
            "{}",
            line.text()
        );
        // Undone, the line and the paste behind it come back together.
        assert!(line.undo());
        assert!(line.undo());
        assert_eq!(line.expand(), "one\ntwo\nthree");
    }

    #[test]
    fn down_on_a_line_not_recalled_leaves_it_alone() {
        let mut line = typed("first prompt");
        line.take();
        // As a queued line is brought back into the box.
        line.set("and the tests");
        line.down();
        assert_eq!(line.text(), "and the tests");
    }

    #[test]
    fn the_same_line_twice_is_remembered_once() {
        let mut line = typed("again");
        line.take();
        line.insert_str("again");
        line.take();
        line.up();
        assert_eq!(line.text(), "again");
        line.up();
        // Nothing older to reach.
        assert_eq!(line.text(), "again");
    }

    #[test]
    fn an_opened_session_brings_back_only_its_newest_prompts() {
        let mut line = typed("draft");
        let sent = (0..HISTORY + 5).map(|n| n.to_string());
        line.set_history(sent.chain(["again".into(), "again".into()]));
        line.up();
        assert_eq!(line.text(), "again");
        line.up();
        // Said twice in a row, it comes back once.
        assert_eq!(line.text(), (HISTORY + 4).to_string());
        for _ in 0..HISTORY + 5 {
            line.up();
        }
        // The oldest few are past the cap.
        assert_eq!(line.text(), "6");

        // Typed since, a line still joins the end.
        line.clear();
        line.insert_str("new");
        line.take();
        line.up();
        assert_eq!(line.text(), "new");
    }

    #[test]
    fn the_row_keys_keep_to_the_row_the_cursor_is_on() {
        let mut line = typed("one two\nthree four\nfive");
        line.up();
        assert_eq!(line.line_of_cursor(), (1, 4));
        line.row_start();
        assert_eq!(line.line_of_cursor(), (1, 0));
        line.row_end();
        assert_eq!(line.line_of_cursor(), (1, 10));
        for _ in 0.." four".len() {
            line.left();
        }
        line.delete_to_row_end();
        assert_eq!(line.text(), "one two\nthree\nfive");
        line.backspace_to_row_start();
        assert_eq!(line.text(), "one two\n\nfive");
        // At either end of its row, there is nothing to take.
        line.backspace_to_row_start();
        line.delete_to_row_end();
        assert_eq!(line.text(), "one two\n\nfive");
    }

    #[test]
    fn arrows_move_within_a_line_that_has_more_than_one() {
        let mut line = typed("top\nbottom");
        line.home();
        assert_eq!(line.line_of_cursor(), (0, 0));
        line.down();
        assert_eq!(line.line_of_cursor(), (1, 0));
        line.up();
        assert_eq!(line.line_of_cursor(), (0, 0));
    }
}
