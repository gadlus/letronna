//! The line being typed.
//!
//! Text, a cursor in it, the pastes standing behind their placeholders, and
//! the lines sent earlier. Kept apart from the drawing so it can be tested
//! without a terminal, which is most of what there is to get wrong here.

/// A pasted block is not put into the line. A placeholder goes there instead
/// and the text waits behind it, so pasting sixty lines does not bury the
/// sentence you were writing around it. The whole thing goes when you send.
pub struct Paste {
    pub lines: usize,
    pub text: String,
}

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

    pub fn insert(&mut self, ch: char) {
        let at = self.byte_at(self.cursor);
        self.text.insert(at, ch);
        self.cursor += 1;
    }

    pub fn insert_str(&mut self, text: &str) {
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

    /// The line as it should be sent: placeholders swapped back for what they
    /// stand for.
    pub fn expand(&self) -> String {
        let mut out = self.text.clone();
        for (i, paste) in self.pastes.iter().enumerate() {
            out = out.replace(&format!("[Pasted #{} +{} lines]", i + 1, paste.lines), &paste.text);
        }
        out
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.pastes.clear();
        self.recalled = None;
    }

    pub fn set(&mut self, text: &str) {
        self.text = text.to_owned();
        self.cursor = self.text.chars().count();
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.text.chars().count());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.text.chars().count();
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
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
            self.cut(self.cursor, self.cursor + 1);
        }
    }

    /// Delete back to the start of the word the cursor is in.
    pub fn backspace_word(&mut self) {
        if let Some(start) = self.marker_before() {
            self.cut(start, self.cursor);
            return;
        }
        let start = self.word_start();
        self.cut(start, self.cursor);
    }

    /// Delete forward to the end of the word ahead of the cursor.
    pub fn delete_word(&mut self) {
        let end = self.word_end();
        self.cut(self.cursor, end);
    }

    pub fn word_left(&mut self) {
        self.cursor = self.word_start();
    }

    pub fn word_right(&mut self) {
        self.cursor = self.word_end();
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
        let column = before.rsplit('\n').next().unwrap_or_default().chars().count();
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
                self.cursor = before + column.min(rows[wanted].chars().count());
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
            (None, _) => None,
            (Some(0), -1) => Some(0),
            (Some(at), -1) => Some(at - 1),
            (Some(at), _) if at >= newest => None,
            (Some(at), _) => Some(at + 1),
        };
        self.recalled = next;
        match next {
            Some(at) => {
                let line = self.history[at].clone();
                self.set(&line);
            }
            None => {
                let draft = std::mem::take(&mut self.draft);
                self.set(&draft);
            }
        }
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
        sent
    }
}

#[cfg(test)]
mod tests {
    use super::Line;

    fn typed(text: &str) -> Line {
        let mut line = Line::default();
        line.insert_str(text);
        line
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
