//! Vim's keys in the box, for fingers that know them: a normal mode where
//! letters move and edit, and an insert mode where they type, as Claude
//! Code's vim mode has them. `/vim` turns them on, and the choice is the
//! terminal's own, kept in `terminal.json`, which the window never reads.
//!
//! The keys work on the line alone, so they are tested without a terminal;
//! what escape and enter mean around them is the bar's to say.

use crate::edit::Line;

/// Where the terminal keeps settings of its own, beside the window's
/// `settings.json` but never written by the window, which would drop them.
fn file() -> std::path::PathBuf {
    letronna_core::chat::data_dir().join("terminal.json")
}

fn read_file() -> serde_json::Value {
    std::fs::read(file())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}))
}

/// Whether vim's keys were left on.
pub fn saved() -> bool {
    read_file()["vim"].as_bool() == Some(true)
}

/// Keep whether vim's keys are on, leaving whatever else the file holds.
pub fn save(on: bool) -> Result<(), String> {
    let path = file();
    let mut json = read_file();
    json["vim"] = on.into();
    let text = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
    let new = path.with_extension("json.new");
    std::fs::write(&new, text)
        .and_then(|()| std::fs::rename(&new, &path))
        .map_err(|e| format!("Could not keep it in {}: {e}", path.display()))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    Insert,
    Normal,
}

/// What a key in normal mode asks of the bar besides the line.
#[derive(Debug, PartialEq)]
pub enum Asked {
    Nothing,
    /// `/`: search what was sent, as ctrl-r does.
    Search,
    /// `?`: the keys card.
    Keys,
    /// `u` with nothing left to undo.
    NothingToUndo,
}

/// A find along the row: f, F, t or T, and the character.
#[derive(Clone, Copy)]
struct Find {
    kind: char,
    ch: char,
}

/// Where a key moves the cursor, as d, c and y take it.
#[derive(Clone, Copy)]
enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordNext,
    WordBack,
    WordEnd,
    RowStart,
    FirstBlank,
    RowEnd,
    Top,
    Bottom,
    Find(Find),
}

/// A command typed in part: its count, the operator waiting for a motion,
/// and a key waiting for the character after it.
#[derive(Default)]
struct Pending {
    count: usize,
    operator: Option<char>,
    /// The count typed before the operator, which multiplies the one after.
    counted: usize,
    /// g, f, F, t, T or r, or i and a after an operator, for a text object.
    then: Option<char>,
}

/// The most a count goes to: more is a slip of the finger, not a plan.
const COUNT: usize = 999;

pub struct Vim {
    mode: Mode,
    pending: Pending,
    /// What d, c, x or y last took, and whether it was whole rows.
    register: (String, bool),
    found: Option<Find>,
}

impl Default for Vim {
    /// Insert mode, as a box is for typing first.
    fn default() -> Self {
        Vim {
            mode: Mode::Insert,
            pending: Pending::default(),
            register: (String::new(), false),
            found: None,
        }
    }
}

impl Vim {
    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn normal(&self) -> bool {
        self.mode == Mode::Normal
    }

    /// Back to typing, as after a message is sent.
    pub fn insert(&mut self) {
        self.mode = Mode::Insert;
        self.pending = Pending::default();
    }

    /// Escape as vim has it: insert mode gives way to normal, the cursor
    /// stepping back onto what was typed, and a command typed in part is
    /// dropped. Says whether it did either, leaving the bar the rest.
    pub fn escape(&mut self, line: &mut Line) -> bool {
        if self.mode == Mode::Insert {
            self.mode = Mode::Normal;
            let chars: Vec<char> = line.text().chars().collect();
            let (start, _) = row(&chars, line.cursor());
            if line.cursor() > start {
                line.go(line.cursor() - 1);
            }
            self.settle(line);
            return true;
        }
        let pending = self.pending.count > 0
            || self.pending.operator.is_some()
            || self.pending.then.is_some();
        self.pending = Pending::default();
        pending
    }

    /// Keep the cursor on a character in normal mode, never past the last
    /// of its row, as vim's block cursor sits.
    pub fn settle(&self, line: &mut Line) {
        if self.mode != Mode::Normal {
            return;
        }
        let chars: Vec<char> = line.text().chars().collect();
        let at = line.cursor();
        let (start, end) = row(&chars, at);
        if end > start && at >= end {
            line.put_cursor(end - 1);
        }
    }

    /// A key typed in normal mode.
    pub fn key(&mut self, ch: char, line: &mut Line) -> Asked {
        let asked = self.command(ch, line);
        self.settle(line);
        asked
    }

    fn command(&mut self, ch: char, line: &mut Line) -> Asked {
        if let Some(then) = self.pending.then.take() {
            return self.second(then, ch, line);
        }
        if ch.is_ascii_digit() && (ch != '0' || self.pending.count > 0) {
            let digit = ch as usize - '0' as usize;
            self.pending.count = (self.pending.count * 10 + digit).min(COUNT);
            return Asked::Nothing;
        }
        if let Some(operator) = self.pending.operator {
            match ch {
                // dd, cc and yy take whole rows.
                _ if ch == operator => {
                    let rows = self.count();
                    let first = row_index(line.text(), line.cursor());
                    self.rows(operator, first, first + rows - 1, line);
                }
                'i' | 'a' | 'g' | 'f' | 'F' | 't' | 'T' => self.pending.then = Some(ch),
                _ => match motion(ch, self.found) {
                    Some(motion) => self.operate(operator, motion, line),
                    None => self.pending = Pending::default(),
                },
            }
            return Asked::Nothing;
        }
        let count = self.count();
        let chars: Vec<char> = line.text().chars().collect();
        let at = line.cursor();
        let (start, end) = row(&chars, at);
        match ch {
            'd' | 'c' | 'y' => {
                self.pending.counted = count;
                self.pending.operator = Some(ch);
            }
            'g' | 'f' | 'F' | 't' | 'T' | 'r' => {
                // The count waits with the key for the character after it.
                self.pending.count = count;
                self.pending.then = Some(ch);
            }
            'x' if at < end => {
                self.register = (line.take_range(at, (at + count).min(end)), false);
            }
            'X' if at > start => {
                self.register = (
                    line.take_range(at.saturating_sub(count).max(start), at),
                    false,
                );
            }
            's' => {
                if at < end {
                    self.register = (line.take_range(at, (at + count).min(end)), false);
                }
                self.mode = Mode::Insert;
            }
            'S' => self.rows(
                'c',
                row_index(line.text(), at),
                row_index(line.text(), at),
                line,
            ),
            'D' => self.cut(at, end, false, line),
            'C' => {
                self.cut(at, end, false, line);
                self.mode = Mode::Insert;
            }
            'Y' => {
                let first = row_index(line.text(), at);
                self.rows('y', first, first + count - 1, line);
            }
            'p' | 'P' => self.put(ch == 'p', count, line),
            'J' => join(line, count.max(2) - 1),
            'u' => {
                if !line.undo() {
                    return Asked::NothingToUndo;
                }
            }
            'i' => self.mode = Mode::Insert,
            'a' => {
                if at < end {
                    line.go(at + 1);
                }
                self.mode = Mode::Insert;
            }
            'I' => {
                line.go(first_blank(&chars, at));
                self.mode = Mode::Insert;
            }
            'A' => {
                line.go(end);
                self.mode = Mode::Insert;
            }
            'o' => {
                line.go(end);
                line.insert_str("\n");
                self.mode = Mode::Insert;
            }
            'O' => {
                line.go(start);
                line.insert_str("\n");
                line.go(start);
                self.mode = Mode::Insert;
            }
            ';' | ',' => {
                if let Some(found) = self.found {
                    let find = if ch == ',' { reversed(found) } else { found };
                    if let Some(to) = find_in_row(&chars, at, find, count) {
                        line.go(to);
                    }
                }
            }
            '/' => return Asked::Search,
            '?' => return Asked::Keys,
            _ => {
                if let Some(motion) = motion(ch, self.found) {
                    self.go(motion, count, line);
                }
            }
        }
        Asked::Nothing
    }

    /// The character after g, f, F, t, T or r, or the object after an
    /// operator's i or a.
    fn second(&mut self, then: char, ch: char, line: &mut Line) -> Asked {
        let motion = match then {
            'g' if ch == 'g' => Some(Motion::Top),
            'f' | 'F' | 't' | 'T' => {
                let find = Find { kind: then, ch };
                self.found = Some(find);
                Some(Motion::Find(find))
            }
            'r' => {
                self.replace(ch, line);
                return Asked::Nothing;
            }
            'i' | 'a' if ch == 'w' || ch == 'W' => {
                if let Some(operator) = self.pending.operator {
                    self.object(operator, then == 'a', ch == 'W', line);
                }
                self.pending = Pending::default();
                return Asked::Nothing;
            }
            _ => None,
        };
        match (motion, self.pending.operator) {
            (Some(motion), Some(operator)) => self.operate(operator, motion, line),
            (Some(motion), None) => {
                let count = self.count();
                self.go(motion, count, line);
            }
            (None, _) => self.pending = Pending::default(),
        }
        Asked::Nothing
    }

    /// The count typed, one when none was, taken so the next command starts
    /// afresh; an operator's own count multiplies it.
    fn count(&mut self) -> usize {
        let after = std::mem::take(&mut self.pending.count).max(1);
        let before = std::mem::take(&mut self.pending.counted).max(1);
        (after * before).min(COUNT)
    }

    /// Move the cursor by `motion`, `count` times.
    fn go(&mut self, motion: Motion, count: usize, line: &mut Line) {
        match motion {
            Motion::Up => (0..count).for_each(|_| line.up()),
            Motion::Down => (0..count).for_each(|_| line.down()),
            Motion::Top | Motion::Bottom => {
                let rows = line.text().split('\n').count();
                // A count names the row, as 3G does.
                let wanted = match motion {
                    _ if count > 1 => count - 1,
                    Motion::Top => 0,
                    _ => rows - 1,
                };
                let chars: Vec<char> = line.text().chars().collect();
                let start = row_start(&chars, wanted.min(rows - 1));
                line.go(first_blank(&chars, start));
            }
            _ => {
                let chars: Vec<char> = line.text().chars().collect();
                if let Some(to) = target(&chars, line.cursor(), motion, count) {
                    line.go(to);
                }
            }
        }
        self.pending = Pending::default();
    }

    /// Apply `operator` over what `motion` moves across.
    fn operate(&mut self, operator: char, motion: Motion, line: &mut Line) {
        let count = self.count();
        self.pending = Pending::default();
        let chars: Vec<char> = line.text().chars().collect();
        let at = line.cursor();
        let here = row_index(line.text(), at);
        let rows = line.text().split('\n').count();
        let (from, to) = match motion {
            // Whole rows: this one and those the motion reaches.
            Motion::Up => return self.rows(operator, here.saturating_sub(count), here, line),
            Motion::Down => {
                return self.rows(operator, here, (here + count).min(rows - 1), line);
            }
            Motion::Top => return self.rows(operator, 0, here, line),
            Motion::Bottom => return self.rows(operator, here, rows - 1, line),
            // cw changes to the end of the word, as vim's does.
            Motion::WordNext
                if operator == 'c' && chars.get(at).is_some_and(|c| !c.is_whitespace()) =>
            {
                match target(&chars, at, Motion::WordEnd, count) {
                    Some(end) if end >= at => (at, end + 1),
                    _ => return,
                }
            }
            Motion::WordNext => {
                let Some(to) = target(&chars, at, motion, count) else {
                    return;
                };
                // The last word of a row goes without the row's end.
                let (_, end) = row(&chars, at);
                (at, to.min(end.max(at + 1)).min(chars.len()))
            }
            Motion::WordEnd => match target(&chars, at, motion, count) {
                Some(to) => (at, (to + 1).min(chars.len())),
                None => return,
            },
            Motion::Find(find) => match find_in_row(&chars, at, find, count) {
                Some(to) if to >= at => (at, to + 1),
                Some(to) => (to, at),
                None => return,
            },
            _ => match target(&chars, at, motion, count) {
                Some(to) if to >= at => (at, to),
                Some(to) => (to, at),
                None => return,
            },
        };
        self.cut(from, to, operator == 'y', line);
        if operator == 'c' {
            self.mode = Mode::Insert;
        }
    }

    /// Take `from` to `to` into the register: cut out for d and c, copied
    /// for y, the cursor at its start either way.
    fn cut(&mut self, from: usize, to: usize, copy: bool, line: &mut Line) {
        if from >= to {
            return;
        }
        let taken = if copy {
            line.text().chars().skip(from).take(to - from).collect()
        } else {
            line.take_range(from, to)
        };
        self.register = (taken, false);
        line.go(from);
    }

    /// Apply `operator` to rows `first` to `last`: d takes them out with a
    /// row's end, c empties them into one row to type in, y copies them.
    fn rows(&mut self, operator: char, first: usize, last: usize, line: &mut Line) {
        self.pending = Pending::default();
        let chars: Vec<char> = line.text().chars().collect();
        let total = line.text().split('\n').count();
        let last = last.min(total - 1);
        let start = row_start(&chars, first);
        let end = row(&chars, row_start(&chars, last)).1;
        let taken: String = chars[start..end].iter().collect();
        match operator {
            'y' => {
                self.register = (taken, true);
                line.go(start);
            }
            'c' => {
                line.take_range(start, end);
                self.register = (taken, true);
                self.mode = Mode::Insert;
            }
            _ => {
                // The row's end goes with it: the one after, or for the
                // last row the one before.
                let (from, to) = if end < chars.len() {
                    (start, end + 1)
                } else {
                    (start.saturating_sub(1), end)
                };
                line.take_range(from, to);
                self.register = (taken, true);
                let chars: Vec<char> = line.text().chars().collect();
                let at = from.min(chars.len());
                let at = row_start(&chars, row_index(line.text(), at));
                line.go(first_blank(&chars, at));
            }
        }
    }

    /// p after the cursor and P before it, `count` times: whole rows go on
    /// a row of their own below or above this one.
    fn put(&mut self, after: bool, count: usize, line: &mut Line) {
        let (text, whole) = self.register.clone();
        if text.is_empty() && !whole {
            return;
        }
        let chars: Vec<char> = line.text().chars().collect();
        let at = line.cursor();
        let (start, end) = row(&chars, at);
        let many = vec![text.as_str(); count].join(if whole { "\n" } else { "" });
        if whole {
            if after {
                line.go(end);
                line.insert_str(&format!("\n{many}"));
                line.go(end + 1);
            } else {
                line.go(start);
                line.insert_str(&format!("{many}\n"));
                line.go(start);
            }
            return;
        }
        let at = if after && at < end { at + 1 } else { at };
        line.go(at);
        line.insert_str(&many);
        line.go((at + many.chars().count()).saturating_sub(1));
    }

    /// r: the character under the cursor, or `count` of them, made `ch`.
    fn replace(&mut self, ch: char, line: &mut Line) {
        let count = self.count();
        self.pending = Pending::default();
        let chars: Vec<char> = line.text().chars().collect();
        let at = line.cursor();
        let (_, end) = row(&chars, at);
        if at + count > end {
            return;
        }
        line.take_range(at, at + count);
        line.go(at);
        line.insert_str(&ch.to_string().repeat(count));
        line.go(at + count - 1);
    }

    /// iw and aw, or iW and aW: the word under the cursor, with the space
    /// after it for a.
    fn object(&mut self, operator: char, around: bool, big: bool, line: &mut Line) {
        let chars: Vec<char> = line.text().chars().collect();
        let at = line.cursor();
        let Some(&under) = chars.get(at) else {
            return;
        };
        let kind = |c: char| {
            if big {
                u8::from(!c.is_whitespace())
            } else {
                class(c)
            }
        };
        let own = kind(under);
        let mut from = at;
        while from > 0 && kind(chars[from - 1]) == own && chars[from - 1] != '\n' {
            from -= 1;
        }
        let mut to = at;
        while to < chars.len() && kind(chars[to]) == own && chars[to] != '\n' {
            to += 1;
        }
        if around {
            while to < chars.len() && chars[to] == ' ' {
                to += 1;
            }
        }
        self.cut(from, to, operator == 'y', line);
        if operator == 'c' {
            self.mode = Mode::Insert;
        }
    }
}

/// The motion a key names, `;` repeating the last find.
fn motion(ch: char, found: Option<Find>) -> Option<Motion> {
    Some(match ch {
        'h' => Motion::Left,
        'l' | ' ' => Motion::Right,
        'k' => Motion::Up,
        'j' => Motion::Down,
        'w' | 'W' => Motion::WordNext,
        'b' | 'B' => Motion::WordBack,
        'e' | 'E' => Motion::WordEnd,
        '0' => Motion::RowStart,
        '^' => Motion::FirstBlank,
        '$' => Motion::RowEnd,
        'G' => Motion::Bottom,
        ';' => Motion::Find(found?),
        ',' => Motion::Find(reversed(found?)),
        _ => return None,
    })
}

/// Where `motion` takes the cursor from `at`, `count` times over, or `None`
/// when it goes nowhere.
fn target(chars: &[char], at: usize, motion: Motion, count: usize) -> Option<usize> {
    let (start, end) = row(chars, at);
    let mut to = at;
    for _ in 0..count {
        to = match motion {
            Motion::Left => to.saturating_sub(1).max(start),
            Motion::Right => (to + 1).min(end),
            Motion::WordNext => word_next(chars, to),
            Motion::WordBack => word_back(chars, to),
            Motion::WordEnd => word_end(chars, to),
            Motion::RowStart => start,
            Motion::FirstBlank => first_blank(chars, at),
            Motion::RowEnd => end,
            Motion::Find(find) => return find_in_row(chars, at, find, count),
            Motion::Up | Motion::Down | Motion::Top | Motion::Bottom => return None,
        };
    }
    Some(to)
}

/// Vim's word kinds: blanks, letters and digits, and the rest.
fn class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

/// w: the start of the next word.
fn word_next(chars: &[char], at: usize) -> usize {
    let mut i = at;
    if let Some(&c) = chars.get(i) {
        let own = class(c);
        while own != 0 && i < chars.len() && class(chars[i]) == own {
            i += 1;
        }
    }
    while i < chars.len() && class(chars[i]) == 0 {
        i += 1;
    }
    i
}

/// b: the start of this word, or of the one before when already there.
fn word_back(chars: &[char], at: usize) -> usize {
    let mut i = at;
    while i > 0 && class(chars[i - 1]) == 0 {
        i -= 1;
    }
    let Some(&c) = i.checked_sub(1).and_then(|before| chars.get(before)) else {
        return 0;
    };
    let own = class(c);
    while i > 0 && class(chars[i - 1]) == own {
        i -= 1;
    }
    i
}

/// e: the end of this word, or of the next when already there.
fn word_end(chars: &[char], at: usize) -> usize {
    let mut i = at + 1;
    while i < chars.len() && class(chars[i]) == 0 {
        i += 1;
    }
    let Some(&c) = chars.get(i) else {
        return chars.len().saturating_sub(1).max(at);
    };
    let own = class(c);
    while i + 1 < chars.len() && class(chars[i + 1]) == own {
        i += 1;
    }
    i
}

/// Where the row holding `at` starts and ends, between line endings.
fn row(chars: &[char], at: usize) -> (usize, usize) {
    let at = at.min(chars.len());
    let start = chars[..at]
        .iter()
        .rposition(|c| *c == '\n')
        .map_or(0, |i| i + 1);
    let end = chars[at..]
        .iter()
        .position(|c| *c == '\n')
        .map_or(chars.len(), |i| at + i);
    (start, end)
}

/// Which row `at` is on, counting from naught.
fn row_index(text: &str, at: usize) -> usize {
    text.chars().take(at).filter(|c| *c == '\n').count()
}

/// Where row `index` starts.
fn row_start(chars: &[char], index: usize) -> usize {
    if index == 0 {
        return 0;
    }
    chars
        .iter()
        .enumerate()
        .filter(|(_, c)| **c == '\n')
        .nth(index - 1)
        .map_or(chars.len(), |(i, _)| i + 1)
}

/// ^: the first character of the row that is not a blank.
fn first_blank(chars: &[char], at: usize) -> usize {
    let (start, end) = row(chars, at);
    (start..end)
        .find(|i| !chars[*i].is_whitespace())
        .unwrap_or(start)
}

/// f, F, t or T `count` times along the row from `at`.
fn find_in_row(chars: &[char], at: usize, find: Find, count: usize) -> Option<usize> {
    let (start, end) = row(chars, at);
    let forward = matches!(find.kind, 'f' | 't');
    let hits: Vec<usize> = if forward {
        // A t right before its character looks past it, so ; goes on.
        let from = at + 1 + usize::from(find.kind == 't');
        (from.min(end)..end)
            .filter(|i| chars[*i] == find.ch)
            .collect()
    } else {
        let to = at.saturating_sub(usize::from(find.kind == 'T'));
        (start..to.max(start))
            .rev()
            .filter(|i| chars[*i] == find.ch)
            .collect()
    };
    let hit = *hits.get(count - 1)?;
    Some(match find.kind {
        't' => hit - 1,
        'T' => hit + 1,
        _ => hit,
    })
}

fn reversed(find: Find) -> Find {
    let kind = match find.kind {
        'f' => 'F',
        'F' => 'f',
        't' => 'T',
        _ => 't',
    };
    Find { kind, ..find }
}

/// J: the row joined to the next, `times` over, by a space for the line
/// ending and the next row's leading blanks.
fn join(line: &mut Line, times: usize) {
    for _ in 0..times {
        let chars: Vec<char> = line.text().chars().collect();
        let (_, end) = row(&chars, line.cursor());
        if end >= chars.len() {
            return;
        }
        let mut next = end + 1;
        while next < chars.len() && (chars[next] == ' ' || chars[next] == '\t') {
            next += 1;
        }
        let gap = end > 0 && chars[end - 1] != ' ' && next < chars.len() && chars[next] != '\n';
        line.take_range(end, next);
        line.go(end);
        if gap {
            line.insert_str(" ");
            line.go(end);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line holding `text` with the cursor at `at`, in normal mode.
    fn normal(text: &str, at: usize) -> (Vim, Line) {
        let mut line = Line::default();
        line.set(text);
        line.go(at);
        let vim = Vim {
            mode: Mode::Normal,
            ..Vim::default()
        };
        (vim, line)
    }

    /// Type `typed` as the bar would: commands in normal mode, letters in
    /// insert mode.
    fn keys(vim: &mut Vim, line: &mut Line, typed: &str) {
        for ch in typed.chars() {
            if vim.normal() {
                vim.key(ch, line);
            } else {
                line.insert(ch);
            }
        }
    }

    #[test]
    fn escape_leaves_insert_mode_on_the_last_character_typed() {
        let mut line = Line::default();
        line.set("hello");
        let mut vim = Vim::default();
        assert!(vim.escape(&mut line));
        assert!(vim.normal());
        assert_eq!(line.cursor(), 4);
        // Once in normal mode, escape leaves the rest to the bar.
        assert!(!vim.escape(&mut line));
    }

    #[test]
    fn the_motions_walk_words_and_rows() {
        let (mut vim, mut line) = normal("one two.three four\nsecond row", 0);
        keys(&mut vim, &mut line, "w");
        assert_eq!(line.cursor(), 4);
        keys(&mut vim, &mut line, "w");
        assert_eq!(line.cursor(), 7, "punctuation is a word of its own");
        keys(&mut vim, &mut line, "2w");
        assert_eq!(line.cursor(), 14);
        keys(&mut vim, &mut line, "e");
        assert_eq!(line.cursor(), 17);
        keys(&mut vim, &mut line, "b");
        assert_eq!(line.cursor(), 14);
        keys(&mut vim, &mut line, "$");
        assert_eq!(line.cursor(), 17, "on the last character, not past it");
        keys(&mut vim, &mut line, "0");
        assert_eq!(line.cursor(), 0);
        keys(&mut vim, &mut line, "G");
        assert_eq!(line.cursor(), 19);
        keys(&mut vim, &mut line, "gg");
        assert_eq!(line.cursor(), 0);
        keys(&mut vim, &mut line, "fh");
        assert_eq!(line.cursor(), 9);
        keys(&mut vim, &mut line, "0tr");
        assert_eq!(line.cursor(), 9, "just before the r");
    }

    #[test]
    fn the_operators_cut_change_and_copy() {
        let (mut vim, mut line) = normal("alpha beta gamma", 0);
        keys(&mut vim, &mut line, "dw");
        assert_eq!(line.text(), "beta gamma");
        keys(&mut vim, &mut line, "cwdelta");
        assert_eq!(line.text(), "delta gamma");
        assert_eq!(vim.mode(), Mode::Insert);
        vim.escape(&mut line);
        keys(&mut vim, &mut line, "0yiw$p");
        assert_eq!(line.text(), "delta gammadelta");
        keys(&mut vim, &mut line, "0D");
        assert_eq!(line.text(), "");
        assert!(matches!(vim.key('u', &mut line), Asked::Nothing));
        assert_eq!(line.text(), "delta gammadelta");
    }

    #[test]
    fn whole_rows_go_and_come_back_as_rows() {
        let (mut vim, mut line) = normal("first\nsecond\nthird", 7);
        keys(&mut vim, &mut line, "dd");
        assert_eq!(line.text(), "first\nthird");
        assert_eq!(line.cursor(), 6, "on the row that took its place");
        keys(&mut vim, &mut line, "P");
        assert_eq!(line.text(), "first\nsecond\nthird");
        keys(&mut vim, &mut line, "G");
        keys(&mut vim, &mut line, "dd");
        assert_eq!(
            line.text(),
            "first\nsecond",
            "the last row takes the ending before it"
        );
        keys(&mut vim, &mut line, "ggcc");
        assert_eq!(line.text(), "\nsecond");
        assert_eq!(vim.mode(), Mode::Insert);
    }

    #[test]
    fn the_insert_keys_put_the_cursor_where_typing_goes() {
        let (mut vim, mut line) = normal("  indented", 5);
        keys(&mut vim, &mut line, "I");
        assert_eq!(line.cursor(), 2);
        vim.escape(&mut line);
        keys(&mut vim, &mut line, "A");
        assert_eq!(line.cursor(), 10);
        vim.escape(&mut line);
        keys(&mut vim, &mut line, "o");
        assert_eq!(line.text(), "  indented\n");
        assert_eq!(line.cursor(), 11);
        vim.escape(&mut line);
        keys(&mut vim, &mut line, "kO");
        assert_eq!(line.text(), "\n  indented\n");
        assert_eq!(line.cursor(), 0);
    }

    #[test]
    fn counts_x_r_and_join_edit_in_place() {
        let (mut vim, mut line) = normal("abcdef", 1);
        keys(&mut vim, &mut line, "2x");
        assert_eq!(line.text(), "adef");
        keys(&mut vim, &mut line, "rZ");
        assert_eq!(line.text(), "aZef");
        let (mut vim, mut line) = normal("one\n   two", 0);
        keys(&mut vim, &mut line, "J");
        assert_eq!(line.text(), "one two");
    }

    #[test]
    fn slash_and_question_mark_are_the_bars_and_u_says_when_there_is_nothing() {
        let (mut vim, mut line) = normal("text", 0);
        assert_eq!(vim.key('/', &mut line), Asked::Search);
        assert_eq!(vim.key('?', &mut line), Asked::Keys);
        assert_eq!(vim.key('u', &mut line), Asked::Nothing);
        assert_eq!(line.text(), "");
        assert_eq!(vim.key('u', &mut line), Asked::NothingToUndo);
    }

    #[test]
    fn a_cut_into_a_paste_placeholder_takes_it_whole() {
        let (mut vim, mut line) = normal("", 0);
        line.paste("one\ntwo\nthree");
        line.insert_str(" after");
        line.go(3);
        keys(&mut vim, &mut line, "x");
        assert_eq!(line.text(), " after");
    }

    #[test]
    fn escape_drops_a_command_typed_in_part() {
        let (mut vim, mut line) = normal("keep this", 0);
        keys(&mut vim, &mut line, "d");
        assert!(vim.escape(&mut line));
        keys(&mut vim, &mut line, "w");
        assert_eq!(line.text(), "keep this");
        assert_eq!(line.cursor(), 5);
    }
}
