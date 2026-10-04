//! Finding the text an `edit_file` call means when the model's copy of it has
//! drifted from the file.
//!
//! Models retype `old` rather than copy it byte for byte: a CRLF file is sent
//! LF, trailing spaces go missing, a block comes back indented differently,
//! and quotes turn curly or straight. Every failed edit costs a round and an
//! approval, so the search tries a short list of ever looser comparisons and
//! maps what it finds back to the file's own bytes. The first comparison that
//! finds anything decides, so a loose one never overrides a closer match, and
//! more than one place is refused unless every place was asked for. The file
//! keeps its byte order mark and its line ending, whatever `new` was sent with.
//!
//! Characters that show as nothing -- zero-width spaces, bidi controls, tag
//! characters -- go missing too: a model cannot copy what it cannot see, and
//! text shown to it without them has to lead back to the file's own bytes.
//!
//! When nothing matches, the miss shows the closest text, numbered as
//! read_file numbers it so it can be copied, and names what differs that
//! those comparisons do not forgive: a misremembered character otherwise
//! costs a second read of the file to find.

use std::cell::Cell;
use std::ops::Range;

/// One replacement: `old` becomes `new`, once or wherever it is found.
pub struct Edit<'a> {
    pub old: &'a str,
    pub new: &'a str,
    pub all: bool,
}

/// A file's text after its edits, with how many places changed and how many
/// edits were in it already.
pub struct Applied {
    pub text: String,
    pub replaced: usize,
    pub already: usize,
}

/// How `old` was compared with the file when it was found.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Strategy {
    Exact,
    LineEndings,
    Invisible,
    TrailingSpace,
    Indent,
    Quotes,
}

/// Make every edit in order, or none of them: the first that cannot be made
/// is the error.
pub fn apply(file: &str, edits: &[Edit]) -> Result<Applied, String> {
    // The mark stays out of the search, where it would keep the first line
    // from ever comparing equal, and goes back on afterwards.
    let (bom, body) = match file.strip_prefix('\u{FEFF}') {
        Some(body) => ("\u{FEFF}", body),
        None => ("", file),
    };
    let ending = line_ending(body);
    let mut text = body.to_owned();
    let (mut replaced, mut already) = (0, 0);
    for (n, edit) in edits.iter().enumerate() {
        // read_file shows the mark, so a first line copied from it brings the
        // mark along, which would keep `old` from matching and give the file
        // a second one from `new`.
        let edit = Edit {
            old: edit.old.strip_prefix(bom).unwrap_or(edit.old),
            new: edit.new.strip_prefix(bom).unwrap_or(edit.new),
            all: edit.all,
        };
        let made = replace(&text, body, &edit, ending).map_err(|e| match edits.len() {
            1 => e,
            total => {
                // A hint on the lines below stays last, so that nothing was
                // made is read with the error, not after a block of the file.
                let (why, hint) = e.split_at(e.find('\n').unwrap_or(e.len()));
                format!(
                    "Edit {} of {total}: {why} None of the edits were made.{hint}",
                    n + 1
                )
            }
        })?;
        match made {
            Some((next, count)) => {
                text = next;
                replaced += count;
            }
            None => already += 1,
        }
    }
    Ok(Applied {
        text: format!("{bom}{text}"),
        replaced,
        already,
    })
}

/// `text` with one edit made and the number of places it changed, or `None`
/// when the edit is in the text already; a miss is looked for in `disk`, the
/// file before any edit of the call.
fn replace(
    text: &str,
    disk: &str,
    edit: &Edit,
    ending: Option<&str>,
) -> Result<Option<(String, usize)>, String> {
    if edit.old.is_empty() {
        return Err("`old` is empty. To write a whole file, use write_file.".into());
    }
    // Line numbers copied from read_file come off only when the text as sent
    // is nowhere, because a file can hold `1|` itself. A `new` made from the
    // same copy has them too, and they would be written into the file.
    let stripped = unnumbered(edit.old);
    let stripped_new = unnumbered(edit.new);
    let found = find(text, edit.old)
        .map(|found| (edit.old, edit.new, found))
        .or_else(|| {
            let old = stripped.as_deref()?;
            let new = stripped_new.as_deref().unwrap_or(edit.new);
            Some((old, new, find(text, old)?))
        });
    let Some((old, new, (strategy, spans))) = found else {
        // A repeated call meets its own result: saying so keeps the model
        // from hunting for text it has already replaced. It counts only
        // when `new` is in the file as sent, bar the line endings it was
        // written with: a `new` inside `old`, with or without its line
        // numbers, was there before the edit too, and a loose match could be
        // other code that looks alike, so either would report a change never
        // made as done.
        let made_already = |(how, at): (Strategy, Vec<Range<usize>>)| {
            matches!(how, Strategy::Exact | Strategy::LineEndings) && at.len() == 1
        };
        let in_old = edit.old.contains(edit.new)
            || stripped
                .as_deref()
                .is_some_and(|old| old.contains(edit.new));
        if !in_old && find(text, edit.new).is_some_and(made_already) {
            return Ok(None);
        }
        // The closest text is looked for in the file as it is on disk, so
        // its line numbers are the ones a read shows; text an earlier edit
        // of this call changed is still there as sent.
        let old = stripped.as_deref().unwrap_or(edit.old);
        let hint = match text != disk && find(disk, old).is_some() {
            true => " An earlier edit in this call changed that text; make both changes in one \
                     edit."
                .into(),
            false => closest(disk, 1, old, false),
        };
        return Err(format!("`old` was not found in the file.{hint}"));
    };
    if spans.len() > 1 && !edit.all {
        return Err(format!(
            "`old` matches {} times, at {}; include more context, or set replace_all to \
             change every one.",
            spans.len(),
            lines_of(text, &spans)
        ));
    }
    if spans.len() > 1 && strategy == Strategy::Quotes {
        return Err(
            "`old` matches only once quotes and dashes are made plain, which is too loose \
             to change every match. Copy the text exactly to use replace_all."
                .into(),
        );
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for span in &spans {
        let mut piece = match strategy {
            Strategy::Indent => reindent(new, old, &text[span.clone()])?,
            _ => new.to_owned(),
        };
        if let Some(ending) = ending {
            piece = piece.replace("\r\n", "\n").replace('\n', ending);
        }
        out.push_str(&text[last..span.start]);
        out.push_str(&piece);
        last = span.end;
    }
    out.push_str(&text[last..]);
    Ok(Some((out, spans.len())))
}

/// Lines named for an `old` found in more places, enough to pick the one
/// meant out of a few while a word found all over a file stays one line.
const NAMED_LINES: usize = 10;

/// The lines `spans` start on in `text`, counting from 1, as `line 4` or
/// `lines 4, 9 and 15`, the first `NAMED_LINES` places named and the rest
/// counted.
fn lines_of(text: &str, spans: &[Range<usize>]) -> String {
    let mut lines: Vec<usize> = spans
        .iter()
        .take(NAMED_LINES)
        .map(|span| text[..span.start].matches('\n').count() + 1)
        .collect();
    // Two places on one line are one line to look at.
    lines.dedup();
    let mut named: Vec<String> = lines.iter().map(usize::to_string).collect();
    let more = spans.len().saturating_sub(NAMED_LINES);
    if more > 0 {
        named.push(format!("{more} more places"));
    }
    let Some((last, rest)) = named.split_last() else {
        return String::new();
    };
    match rest {
        [] => format!("line {last}"),
        _ => format!("lines {} and {last}", rest.join(", ")),
    }
}

/// Replace the whole lines `old` with `new` at the one place in `file` past
/// byte `from` where they start a line, and say where the new lines end:
/// a patch's hunks come in file order, each after the one before, and one
/// matched mid-line would change the wrong code; `breaks` is how many line
/// breaks `file` had before any hunk, so a miss names lines as on disk.
pub(crate) fn replace_lines(
    file: &str,
    from: usize,
    breaks: usize,
    old: &str,
    new: &str,
) -> Result<(String, usize), String> {
    let text = &file[from..];
    let starts_line = |span: &Range<usize>| span.start == 0 || text[..span.start].ends_with('\n');
    let Some((strategy, spans)) = find_where(text, old, &starts_line) else {
        // The hunks before only changed lines above `from`, so the rest is
        // as on disk, and counting back from the end gives its numbers.
        let first = (breaks + 1).saturating_sub(text.matches('\n').count());
        return Err(format!(
            "its ` ` and `-` lines are not in the file after the hunks before it.{}",
            closest(text, first, old, true)
        ));
    };
    let [span] = spans.as_slice() else {
        return Err(format!(
            "its ` ` and `-` lines are in {} places. Add context lines, or an `@@` line \
             naming the function or section they are in.",
            spans.len()
        ));
    };
    let mut piece = match strategy {
        Strategy::Indent => reindent(new, old, &text[span.clone()])?,
        _ => new.to_owned(),
    };
    if let Some(ending) = line_ending(file) {
        piece = piece.replace("\r\n", "\n").replace('\n', ending);
    }
    let end = from + span.start + piece.len();
    let out = format!(
        "{}{}{piece}{}",
        &file[..from],
        &text[..span.start],
        &text[span.end..]
    );
    Ok((out, end))
}

/// Where `old` is in `text`, by the first comparison that finds it anywhere.
fn find(text: &str, old: &str) -> Option<(Strategy, Vec<Range<usize>>)> {
    find_where(text, old, &|_| true)
}

/// `find`, counting only the places `keep` takes.
fn find_where(
    text: &str,
    old: &str,
    keep: &dyn Fn(&Range<usize>) -> bool,
) -> Option<(Strategy, Vec<Range<usize>>)> {
    let order = [
        Strategy::Exact,
        Strategy::LineEndings,
        Strategy::Invisible,
        Strategy::TrailingSpace,
        Strategy::Indent,
        Strategy::Quotes,
    ];
    order.into_iter().find_map(|strategy| {
        let spans = match strategy {
            Strategy::Exact => {
                let spans: Vec<_> = text
                    .match_indices(old)
                    .map(|(at, found)| at..at + found.len())
                    .collect();
                // A match that splits a CRLF pair would leave a stray CR once
                // `new` takes the file's line ending; comparing line endings
                // takes the whole pair instead.
                match spans
                    .iter()
                    .any(|span| splits_crlf(text, span.start) || splits_crlf(text, span.end))
                {
                    true => Vec::new(),
                    false => spans,
                }
            }
            Strategy::LineEndings => folded(text, old, strategy),
            Strategy::Invisible => folded(text, old, strategy),
            Strategy::TrailingSpace => whole_lines(text, old, false),
            Strategy::Indent => whole_lines(text, old, true),
            Strategy::Quotes => folded(text, old, strategy),
        };
        let spans: Vec<_> = spans.into_iter().filter(|span| keep(span)).collect();
        (!spans.is_empty()).then_some((strategy, spans))
    })
}

/// Whether a place in `text` falls between the CR and the LF of a line break.
fn splits_crlf(text: &str, at: usize) -> bool {
    text[..at].ends_with('\r') && text[at..].starts_with('\n')
}

/// Where `old` is once CRLF reads as LF in both, and, as `how` says, once
/// typographic punctuation reads as the ASCII typed for it too, or once
/// neither has the characters that show as nothing.
fn folded(text: &str, old: &str, how: Strategy) -> Vec<Range<usize>> {
    // Text copied from what the model is shown never holds a character
    // `untrusted::visible` takes out or turns into a line break, so an `old`
    // that does was typed and means it. Read without it, `\u{200B}\n` would
    // be every line break in the file, and a hidden character alone nothing,
    // found between every two characters.
    if how == Strategy::Invisible && crate::untrusted::visible(old) != old {
        return Vec::new();
    }
    let view = View::of(text, how);
    let old = View::of(old, how).text;
    view.text
        .match_indices(&old)
        .filter_map(|(at, found)| Some(view.original(at)?..view.original(at + found.len())?))
        .collect()
}

/// A copy of some text with a few characters swapped for plainer ones or
/// left out, and where each swap was, so a place in the copy can be taken
/// back to the text.
struct View {
    text: String,
    /// Each swap: the bytes it took in the copy, and the bytes it replaced.
    swaps: Vec<(Range<usize>, Range<usize>)>,
}

impl View {
    fn of(text: &str, how: Strategy) -> Self {
        let mut view = View {
            text: String::with_capacity(text.len()),
            swaps: Vec::new(),
        };
        let hiding = how == Strategy::Invisible;
        // Where the run of hidden characters just passed began. All of them
        // go, even a joiner `untrusted::visible` keeps, so `old` is found
        // with it or without. The run goes with the character after it: a
        // place found never starts inside one, and a patch's line that
        // starts with one still starts its line.
        let mut hidden = None;
        let mut chars = text.char_indices().peekable();
        while let Some((at, c)) = chars.next() {
            let mut own = [0; 4];
            let (with, len) = if c == '\r' && chars.next_if(|&(_, n)| n == '\n').is_some() {
                ("\n", 2)
            } else if let Some(with) = plain(c).filter(|_| how == Strategy::Quotes) {
                (with, c.len_utf8())
            } else if hiding && matches!(c, '\u{2028}' | '\u{2029}') {
                ("\n", c.len_utf8())
            } else if hiding && crate::untrusted::invisible(c) {
                hidden.get_or_insert(at);
                continue;
            } else if hidden.is_some() {
                (&*c.encode_utf8(&mut own), c.len_utf8())
            } else {
                view.text.push(c);
                continue;
            };
            let start = view.text.len();
            view.text.push_str(with);
            let from = hidden.take().unwrap_or(at);
            view.swaps.push((start..view.text.len(), from..at + len));
        }
        view
    }

    /// Where a place in the copy falls in the text, or `None` when it splits a
    /// swap, such as between the dots that stand for one ellipsis.
    fn original(&self, at: usize) -> Option<usize> {
        let before = self.swaps.partition_point(|(copy, _)| copy.start <= at);
        let Some((copy, text)) = before.checked_sub(1).map(|i| &self.swaps[i]) else {
            return Some(at);
        };
        if at == copy.start {
            Some(text.start)
        } else if at < copy.end {
            None
        } else {
            Some(text.end + (at - copy.end))
        }
    }
}

/// The ASCII a model types for a typographic character a file may hold.
fn plain(c: char) -> Option<&'static str> {
    Some(match c {
        '‘' | '’' => "'",
        '“' | '”' => "\"",
        '–' | '—' => "-",
        '…' => "...",
        '\u{a0}' => " ",
        _ => return None,
    })
}

/// Runs of whole lines that equal `old`'s once trailing space is ignored, or
/// with `indent` the space on both sides, as long as the indentation moved
/// the same way on every line.
fn whole_lines(text: &str, old: &str, indent: bool) -> Vec<Range<usize>> {
    // An `old` that ends in a line break takes the file's line break with it.
    let (old, whole) = match old.strip_suffix('\n') {
        Some(old) => (old, true),
        None => (old, false),
    };
    let want: Vec<&str> = old.split('\n').collect();
    // Blank lines alone would match any blank lines in the file.
    if want.iter().all(|line| line.trim().is_empty()) {
        return Vec::new();
    }
    let same = |have: &str, want: &str| match indent {
        true => have.trim() == want.trim(),
        false => have.trim_end() == want.trim_end(),
    };
    let mut lines = Vec::new();
    let mut at = 0;
    for line in text.split('\n') {
        lines.push(at..at + line.len());
        at += line.len() + 1;
    }
    let mut spans = Vec::new();
    let mut i = 0;
    while i + want.len() <= lines.len() {
        let window = &lines[i..i + want.len()];
        let fits = window
            .iter()
            .zip(&want)
            .all(|(line, want)| same(&text[line.clone()], want))
            && (!indent || evenly_shifted(text, window, &want));
        if !fits {
            i += 1;
            continue;
        }
        let last = &window[want.len() - 1];
        let end = match whole {
            true => (last.end + 1).min(text.len()),
            false => last.start + text[last.clone()].trim_end_matches('\r').len(),
        };
        spans.push(window[0].start..end);
        i += want.len();
    }
    spans
}

/// Whether every non-blank line of the window has `want`'s indentation with
/// the same leading space swapped for another, so `new` can be moved to fit.
fn evenly_shifted(text: &str, window: &[Range<usize>], want: &[&str]) -> bool {
    let Some(base) = base_line(want.iter().copied()) else {
        return false;
    };
    let from = indent_of(want[base]);
    let to = indent_of(&text[window[base].clone()]);
    window
        .iter()
        .zip(want)
        .filter(|(_, want)| !want.trim().is_empty())
        .all(|(line, want)| {
            let has = indent_of(&text[line.clone()]);
            indent_of(want)
                .strip_prefix(from)
                .is_some_and(|extra| has.strip_prefix(to) == Some(extra))
        })
}

/// `new` moved by the indentation `old` needed to fit the text found for it,
/// or an error when a line of `new` starts left of every line of `old`.
fn reindent(new: &str, old: &str, found: &str) -> Result<String, String> {
    let Some(base) = base_line(old.lines()) else {
        return Ok(new.to_owned());
    };
    let (Some(from), Some(to)) = (old.lines().nth(base), found.lines().nth(base)) else {
        return Ok(new.to_owned());
    };
    let (from, to) = (indent_of(from), indent_of(to));
    new.split('\n')
        .map(|line| match line.strip_prefix(from) {
            Some(rest) if !line.trim().is_empty() => Ok(format!("{to}{rest}")),
            // Where such a line goes in the file would be a guess, and in
            // Python or YAML a wrong guess changes what the code means.
            None if !line.trim().is_empty() => Err(
                "`old` was found only with its indentation changed, and a line of `new` \
                 starts left of all of `old`, so where it goes is unclear. Send `old` and \
                 `new` indented as the file is."
                    .to_owned(),
            ),
            _ => Ok(line.to_owned()),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|lines| lines.join("\n"))
}

/// Which line sets a block's indentation: the least indented one not blank.
fn base_line<'a>(lines: impl Iterator<Item = &'a str>) -> Option<usize> {
    lines
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .min_by_key(|(_, line)| indent_of(line).len())
        .map(|(i, _)| i)
}

/// The space a line starts with.
fn indent_of(line: &str) -> &str {
    &line[..line.len() - line.trim_start().len()]
}

/// Text without the `N|` read_file shows before each line, when every line
/// has one and they count up by one.
fn unnumbered(old: &str) -> Option<String> {
    let (body, end) = match old.strip_suffix('\n') {
        Some(body) => (body, "\n"),
        None => (old, ""),
    };
    let mut next = None;
    let mut lines = Vec::new();
    for line in body.split('\n') {
        let (number, rest) = line.split_once('|')?;
        if !number.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let number: usize = number.parse().ok()?;
        if next.is_some_and(|next| next != number) {
            return None;
        }
        next = Some(number + 1);
        lines.push(rest);
    }
    Some(lines.join("\n") + end)
}

/// The line break a file uses, judged by its first; `None` when it has none.
pub(crate) fn line_ending(text: &str) -> Option<&'static str> {
    let at = text.find('\n')?;
    Some(if text[..at].ends_with('\r') {
        "\r\n"
    } else {
        "\n"
    })
}

/// Files larger than this get no closest text: the search runs while the
/// record of files every session reads and writes through is held, and a
/// megabyte of code takes a few milliseconds.
const SEARCHED_BYTES: usize = 1 << 20;
/// Cells of the table `distance` fills that a miss may spend finding where
/// `old`'s lines are, and as many again weighing the places found: some
/// 20 ms each at about 2 ns a cell, where a file of lines that all look
/// alike would otherwise take half a second.
const COMPARED_CELLS: usize = 10_000_000;
/// A line counts as the one `old` meant with up to one character in this
/// many changed: a typo or a renamed word, not another line.
const CHANGED_ONE_IN: usize = 5;
/// How alike a place must be, in percent over its lines, to be shown: below
/// that, the lines that match are as likely chance.
const SHOWN_ALIKE: usize = 60;
/// Points a second place may score below the best and still be shown, so
/// of two lookalike functions the model sees both rather than a guess.
const RUNNER_UP: usize = 5;
/// The longest lines of `old` the search starts from: the longest says the
/// most, and the next find the place when the longest is what changed.
const ANCHORS: usize = 3;
/// Places weighed in full, each anchor's closest first: enough for a line
/// repeated through a file, and a bound on the work.
const PLACES: usize = 20;
/// Lines either side of the anchor that the file may have and `old` lack,
/// or the other way round, before lines are simply set side by side.
const SLACK: usize = 2;
/// Characters of a line compared, so a minified file keeps the search short.
const COMPARED_CHARS: usize = 256;
/// Lines of the file shown across the places: about a screenful, still far
/// short of the page a second read would cost.
const SHOWN_LINES: usize = 12;
/// Differences named for each place: the first few say what went wrong.
const NAMED_DIFFERENCES: usize = 3;
/// Characters of a shown line; a longer one is cut, as read_file cuts.
const SHOWN_CHARS: usize = 200;
/// Characters a difference is widened by, either side, to the words around.
const EXCERPT_SIDE: usize = 12;
/// Characters shown of the text either side has where a line differs.
const EXCERPT_CHARS: usize = 40;

/// A line of `old` as the search sees it.
struct Want<'a> {
    text: &'a str,
    /// Its characters without the space either side, as far as compared.
    chars: Vec<char>,
    /// Whether it may start inside a line of the file, as `old`'s first may.
    free_start: bool,
    /// Whether it may end inside a line of the file, as `old`'s last may.
    free_end: bool,
}

/// One step through a place, `old`'s lines set beside the file's.
#[derive(Clone, Copy)]
enum Step {
    /// A line of `old` beside a line of the file, and how alike in percent,
    /// 0 when too unlike to count.
    Beside {
        want: usize,
        line: usize,
        alike: usize,
    },
    /// A line of `old` the file lacks.
    Lacked(usize),
    /// A line of the file `old` lacks.
    Extra(usize),
}

/// Where `old` may have been meant, with its lines set beside the file's.
struct Place {
    steps: Vec<Step>,
    /// How alike, in percent, over the lines either side has.
    score: usize,
    /// The lines of the file it covers, counted from 0.
    lines: Range<usize>,
}

/// What a miss says after its first sentence: where in `text`, whose first
/// line is line `first` of the file, the text closest to `old` is and how
/// they differ, or that nothing came close; with `whole`, `old` is whole
/// lines, as a patch's are, and otherwise its first and last may be pieces.
fn closest(text: &str, first: usize, old: &str, whole: bool) -> String {
    let again = " Read the file again and copy the text exactly.";
    if text.len() > SEARCHED_BYTES {
        return again.into();
    }
    let nothing = " No similar text was found; grep for a line of it or read the part you \
                   mean.";
    // As read_file shows them, so what is shown and compared is what the
    // model could have copied: a hidden character there is forgiven, and
    // one shown raw would reach the model that read_file kept from it.
    let lines: Vec<String> = lines_in(text)
        .into_iter()
        .map(crate::untrusted::visible)
        .collect();
    let parts = lines_in(old);
    let last = parts.len().saturating_sub(1);
    let wants: Vec<Want> = parts
        .iter()
        .enumerate()
        .map(|(i, &text)| Want {
            text,
            chars: compared(text),
            free_start: !whole && i == 0,
            free_end: !whole && i == last && !old.ends_with('\n'),
        })
        .collect();
    let have: Vec<Vec<char>> = lines.iter().map(|line| compared(line)).collect();
    let (scan, weigh) = (Cell::new(COMPARED_CELLS), Cell::new(COMPARED_CELLS));
    let mut anchors: Vec<usize> = (0..wants.len())
        .filter(|&i| !wants[i].chars.is_empty())
        .collect();
    anchors.sort_by_key(|&i| std::cmp::Reverse(wants[i].chars.len()));
    anchors.truncate(ANCHORS);
    // Anchors that agree on where `old` starts find one place, and the next
    // anchor is tried only while no place is close enough to show.
    let mut starts = Vec::new();
    let mut places: Vec<Place> = Vec::new();
    'search: for &i in &anchors {
        if places.iter().any(|place| place.score >= SHOWN_ALIKE) {
            break;
        }
        let mut hits: Vec<(usize, usize)> = have
            .iter()
            .enumerate()
            .filter_map(|(j, line)| Some((alike(&wants[i], line, &scan)?, j)))
            .collect();
        hits.sort_by_key(|&(alike, _)| std::cmp::Reverse(alike));
        let weighed = places.len();
        for (alike, j) in hits {
            let start = j + wants.len() - i;
            if starts.contains(&start) {
                continue;
            }
            if places.len() - weighed == PLACES {
                break;
            }
            starts.push(start);
            let found = place(&wants, &have, (i, j), alike, &weigh);
            // A place the cells ran out in is only part weighed, and would
            // score lower than it is.
            if weigh.get() == 0 {
                break 'search;
            }
            places.push(found);
        }
    }
    places.sort_by_key(|place| (std::cmp::Reverse(place.score), place.lines.start));
    let Some(best) = places.first().filter(|place| place.score >= SHOWN_ALIKE) else {
        // A search cut short has not looked everywhere.
        return match scan.get() == 0 || weigh.get() == 0 {
            true => again.into(),
            false => nothing.into(),
        };
    };
    let apart =
        |place: &Place| place.lines.end <= best.lines.start || place.lines.start >= best.lines.end;
    let runner = places.iter().skip(1).find(|place| {
        place.score >= SHOWN_ALIKE && place.score + RUNNER_UP >= best.score && apart(place)
    });
    let shown: Vec<&Place> = std::iter::once(best).chain(runner).collect();
    // Hidden before any cut, which could leave part of a key showing.
    let secrets = crate::redact::secrets();
    let seen = |j: usize| {
        let none = crate::redact::Shapes::None;
        crate::redact::hide(&lines[j], &secrets, none, crate::redact::MARKER)
    };
    let mut out = String::new();
    for (n, place) in shown.iter().enumerate() {
        let heading = if n == 0 { "Closest text" } else { "Also close" };
        let room = SHOWN_LINES / shown.len();
        out += &describe(heading, place, &wants, &lines, &seen, first, room);
    }
    out
}

/// The lines of `text`, without their line breaks, and none after a last
/// break.
fn lines_in(text: &str) -> Vec<&str> {
    text.strip_suffix('\n')
        .unwrap_or(text)
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect()
}

/// The characters of a line that are compared: without the space either
/// side, which edit_file forgives, and no more than `COMPARED_CHARS`.
fn compared(line: &str) -> Vec<char> {
    line.trim().chars().take(COMPARED_CHARS).collect()
}

/// `old`'s lines set beside the file's outward from line `i` of `old` at
/// line `j` of the file, which are `alike` in percent, and scored, with the
/// cells `left` to compare them.
fn place(
    wants: &[Want],
    have: &[Vec<char>],
    (i, j): (usize, usize),
    alike: usize,
    left: &Cell<usize>,
) -> Place {
    let mut steps = walk(wants, have, (i, j), true, left);
    steps.push(Step::Beside {
        want: i,
        line: j,
        alike,
    });
    steps.extend(walk(wants, have, (i, j), false, left));
    // Blank lines on both sides say nothing about the place.
    let counted = |step: &Step| match *step {
        Step::Beside { want, line, alike } => {
            (!wants[want].chars.is_empty() || !have[line].is_empty()).then_some(alike)
        }
        Step::Lacked(want) => (!wants[want].chars.is_empty()).then_some(0),
        Step::Extra(line) => (!have[line].is_empty()).then_some(0),
    };
    let scores: Vec<usize> = steps.iter().filter_map(counted).collect();
    let score = scores.iter().sum::<usize>() / scores.len().max(1);
    let covered = steps.iter().filter_map(|step| match *step {
        Step::Beside { line, .. } | Step::Extra(line) => Some(line),
        Step::Lacked(_) => None,
    });
    let start = covered.clone().min().unwrap_or(j);
    let end = covered.max().unwrap_or(j) + 1;
    Place {
        steps,
        score,
        lines: start..end,
    }
}

/// The steps one way from line `i` of `old` beside line `j` of the file
/// until `old` runs out: each line beside the next, or one skipped on
/// either side when the line after it fits instead, `SLACK` times at most.
fn walk(
    wants: &[Want],
    have: &[Vec<char>],
    (i, j): (usize, usize),
    back: bool,
    left: &Cell<usize>,
) -> Vec<Step> {
    let next = |at: usize| match back {
        true => at.checked_sub(1),
        false => Some(at + 1),
    };
    let fits = |i: Option<usize>, j: Option<usize>| match (i, j) {
        (Some(i), Some(j)) if i < wants.len() && j < have.len() => alike(&wants[i], &have[j], left),
        _ => None,
    };
    let (mut i, mut j, mut slack) = (next(i), next(j), 0);
    let mut steps = Vec::new();
    while let Some(want) = i.filter(|&want| want < wants.len()) {
        i = next(want);
        let Some(line) = j.filter(|&line| line < have.len()) else {
            steps.push(Step::Lacked(want));
            continue;
        };
        if let Some(alike) = fits(Some(want), Some(line)) {
            steps.push(Step::Beside { want, line, alike });
        } else if slack < SLACK && fits(Some(want), next(line)).is_some() {
            steps.push(Step::Extra(line));
            i = Some(want);
            slack += 1;
        } else if slack < SLACK && fits(next(want), Some(line)).is_some() {
            steps.push(Step::Lacked(want));
            slack += 1;
            continue;
        } else {
            steps.push(Step::Beside {
                want,
                line,
                alike: 0,
            });
        }
        j = next(line);
    }
    if back {
        steps.reverse();
    }
    steps
}

/// How alike `want` is to a line of the file, in percent, when it is alike
/// enough to be the line meant and the cells `left` are enough to tell;
/// when they are not, none are left.
fn alike(want: &Want, line: &[char], left: &Cell<usize>) -> Option<usize> {
    if want.chars == line {
        return Some(100);
    }
    let (m, n) = (want.chars.len(), line.len());
    let part = want.free_start || want.free_end;
    // A piece is measured by itself, the rest of the line being free.
    let whole = if part { m } else { m.max(n) };
    if whole == 0 {
        return Some(100);
    }
    let limit = whole / CHANGED_ONE_IN;
    let too_far = match part {
        true => n + limit < m,
        false => m.abs_diff(n) > limit,
    };
    if too_far || lacked(&want.chars, line) > limit {
        return None;
    }
    // Charged in full before the count, which may stop early: a bound on
    // the work, not a measure of it.
    let Some(rest) = left.get().checked_sub(m * n) else {
        left.set(0);
        return None;
    };
    left.set(rest);
    let (changes, _) = distance(&want.chars, line, want.free_start, want.free_end, limit)?;
    Some(100 - (changes * 100).div_ceil(whole))
}

/// How many of `want`'s characters `line` has too few of, ASCII by kind and
/// the rest together: each takes a change, so most lines are turned away
/// before the slower count.
fn lacked(want: &[char], line: &[char]) -> usize {
    let mut have = [0usize; 129];
    let kind = |c: char| (c as usize).min(128);
    for &c in line {
        have[kind(c)] += 1;
    }
    let mut lacked = 0;
    for &c in want {
        match have[kind(c)].checked_sub(1) {
            Some(left) => have[kind(c)] = left,
            None => lacked += 1,
        }
    }
    lacked
}

/// The fewest characters to add, drop or change to make `want` into `line`,
/// or into a run of it where `free_start` lets the run start later and
/// `free_end` end sooner, and that run; `None` past `limit`.
fn distance(
    want: &[char],
    line: &[char],
    free_start: bool,
    free_end: bool,
    limit: usize,
) -> Option<(usize, Range<usize>)> {
    // Each cell: the fewest changes for `want` so far against `line` up to
    // here, and where in `line` that run starts.
    let mut row: Vec<(usize, usize)> = (0..=line.len())
        .map(|j| if free_start { (0, j) } else { (j, 0) })
        .collect();
    for (i, &c) in want.iter().enumerate() {
        let mut corner = row[0];
        row[0] = (i + 1, 0);
        for j in 1..=line.len() {
            let above = row[j];
            let changed = (corner.0 + usize::from(c != line[j - 1]), corner.1);
            let dropped = (above.0 + 1, above.1);
            let added = (row[j - 1].0 + 1, row[j - 1].1);
            corner = above;
            row[j] = [changed, dropped, added]
                .into_iter()
                .min_by_key(|cell| cell.0)
                .unwrap_or(changed);
        }
        if row.iter().all(|cell| cell.0 > limit) {
            return None;
        }
    }
    let end = match free_end {
        true => (0..=line.len())
            .min_by_key(|&j| row[j].0)
            .unwrap_or(line.len()),
        false => line.len(),
    };
    let (changes, start) = row[end];
    (changes <= limit).then_some((changes, start..end))
}

/// One place as the miss shows it: its lines, numbered as read_file numbers
/// them, as many as `room` from where it first differs, then what differs.
fn describe(
    heading: &str,
    place: &Place,
    wants: &[Want],
    lines: &[String],
    seen: &dyn Fn(usize) -> String,
    first: usize,
    room: usize,
) -> String {
    let said = differences(place, wants, lines, seen, first);
    // Whatever is named is why `old` missed, and a place called wholly
    // alike above it would read as a contradiction; lines compared only
    // so far, or alike once trimmed, can score 100 and still differ.
    let score = match said.is_empty() {
        true => place.score,
        false => place.score.min(99),
    };
    let Range { start, end } = place.lines.clone();
    let from = match end - start > room {
        true => said
            .first()
            .map_or(start, |(at, _)| at.saturating_sub(1))
            .clamp(start, end - room),
        false => start,
    };
    let span = match end - start {
        1 => format!("line {}", first + start),
        _ => format!("lines {}-{}", first + start, first + end - 1),
    };
    let mut out = format!("\n{heading}, {span} ({score}% alike):");
    for j in from..end.min(from + room) {
        let text = seen(j);
        out += &match text.char_indices().nth(SHOWN_CHARS) {
            Some((at, _)) => format!("\n{}|{} … [line cut]", first + j, &text[..at]),
            None => format!("\n{}|{text}", first + j),
        };
    }
    for (_, said) in said.iter().take(NAMED_DIFFERENCES) {
        out.push('\n');
        out.push_str(said);
    }
    out
}

/// What sets `old` apart from a place, each with the line of the file it is
/// at; or, when every line is the same bar what edit_file forgives one at a
/// time, why they did not match together.
fn differences(
    place: &Place,
    wants: &[Want],
    lines: &[String],
    seen: &dyn Fn(usize) -> String,
    first: usize,
) -> Vec<(usize, String)> {
    let mut said = Vec::new();
    let mut before = None;
    for step in &place.steps {
        match *step {
            Step::Beside { want, line, .. } => {
                before = Some(line);
                if !same(&wants[want], &lines[line], true, true) {
                    said.push((line, differs(&wants[want], &seen(line), first + line)));
                }
            }
            Step::Extra(line) => {
                before = Some(line);
                let text = named(seen(line).trim());
                said.push((
                    line,
                    format!("Line {} of the file is not in `old`: {text}.", first + line),
                ));
            }
            Step::Lacked(want) => {
                let text = named(wants[want].text.trim());
                let (at, side) = match before {
                    Some(line) => (line, "after"),
                    None => (place.lines.start, "before"),
                };
                let n = first + at;
                said.push((
                    at,
                    format!("`old` has {text} the file does not have, {side} line {n}."),
                ));
            }
        }
    }
    if !said.is_empty() {
        return said;
    }
    let mut beside = place.steps.iter().filter_map(|step| match *step {
        Step::Beside { want, line, .. } => Some((&wants[want], lines[line].as_str())),
        _ => None,
    });
    let quotes = beside
        .clone()
        .any(|(want, line)| !same(want, line, true, false));
    let spacing = beside.any(|(want, line)| !same(want, line, false, true));
    let why = match (quotes, spacing) {
        (true, true) => {
            "The quotes and the spacing at line starts or ends both differ; edit_file forgives \
             one at a time."
        }
        (false, true) => {
            "Only the spacing at line starts or ends differs, which edit_file forgives in whole \
             lines that all moved alike; copy it as the file has it."
        }
        _ => return said,
    };
    said.push((place.lines.start, why.into()));
    said
}

/// Whether `want` is `line`, or the piece of it `want` may be, once the
/// space either side is gone with `trim`, and quotes and dashes are made
/// plain with `fold`.
fn same(want: &Want, line: &str, trim: bool, fold: bool) -> bool {
    let shape = |text: &str| {
        let text = if trim { text.trim() } else { text };
        match fold {
            true => text.chars().map(plain_text).collect(),
            false => text.to_owned(),
        }
    };
    let (want_text, line) = (shape(want.text), shape(line));
    match (want.free_start, want.free_end) {
        (false, false) => line == want_text,
        (true, false) => line.ends_with(&want_text),
        (false, true) => line.starts_with(&want_text),
        (true, true) => line.contains(&want_text),
    }
}

/// A character as the comparison of quotes and dashes reads it.
fn plain_text(c: char) -> String {
    plain(c).map_or_else(|| c.to_string(), str::to_owned)
}

/// Where line `n` of the file, `line`, first differs from `want`, what kind
/// of difference it is when it has a name, and what each has there.
fn differs(want: &Want, line: &str, n: usize) -> String {
    let indent = line.chars().count() - line.trim_start().chars().count();
    let have: Vec<char> = line.trim().chars().collect();
    let wanted: Vec<char> = want.text.trim().chars().collect();
    let run = match want.free_start || want.free_end {
        true => {
            let (a, b) = (wanted.len(), have.len());
            let (a, b) = (a.min(COMPARED_CHARS), b.min(COMPARED_CHARS));
            let found = distance(
                &wanted[..a],
                &have[..b],
                want.free_start,
                want.free_end,
                a + b,
            );
            // Past the cut the rest of `old` is taken to go on as the line
            // does, so a difference there is still found, and a piece that
            // ends with its line ends with it.
            found.map_or(0..have.len(), |(_, run)| {
                let end = match want.free_end {
                    true => run.end + (wanted.len() - a),
                    false => have.len(),
                };
                run.start..end.min(have.len())
            })
        }
        false => 0..have.len(),
    };
    let have = &have[run.clone()];
    let from = have.iter().zip(&wanted).take_while(|(a, b)| a == b).count();
    let room = have.len().min(wanted.len()) - from;
    let alike_end = have.iter().rev().zip(wanted.iter().rev());
    let to = alike_end.take(room).take_while(|(a, b)| a == b).count();
    let (have_to, wanted_to) = (have.len() - to, wanted.len() - to);
    let kind = kind(&have[from..have_to], &wanted[from..wanted_to]);
    format!(
        "Differs at {n}:{}{kind}: the file has `{}`, `old` has `{}`.",
        indent + run.start + from + 1,
        excerpt(have, from, have_to),
        excerpt(&wanted, from, wanted_to)
    )
}

/// The name of a difference edit_file does not forgive, when it has one,
/// to follow where it is: " in letter case", say, or nothing.
fn kind(have: &[char], wanted: &[char]) -> &'static str {
    let plain = |chars: &[char]| -> String { chars.iter().copied().map(plain_text).collect() };
    let without = |text: &str, gone: fn(char) -> bool| -> String {
        text.chars().filter(|&c| !gone(c)).collect()
    };
    let (have_text, wanted_text) = (plain(have), plain(wanted));
    let backslash = |c: char| c == '\\';
    // One the file has is forgiven, so only one typed into `old` is why.
    if wanted.iter().any(|&c| crate::untrusted::invisible(c)) {
        " by an invisible character"
    } else if have_text == wanted_text {
        ""
    } else if have_text.to_lowercase() == wanted_text.to_lowercase() {
        " in letter case"
    } else if without(&have_text, backslash) == without(&wanted_text, backslash) {
        " in backslashes"
    } else if without(&have_text, char::is_whitespace) == without(&wanted_text, char::is_whitespace)
    {
        " in spacing"
    } else {
        ""
    }
}

/// A whole line named in a difference: its text, or that it is blank.
fn named(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    match chars.is_empty() {
        true => "a blank line".into(),
        false => format!("`{}`", excerpt(&chars, 0, chars.len())),
    }
}

/// `text[start..end]` widened to the words it is in, by `EXCERPT_SIDE`
/// characters at most either side, cut to `EXCERPT_CHARS`, with invisible
/// characters named so they can be seen.
fn excerpt(text: &[char], start: usize, end: usize) -> String {
    let mut from = start;
    while from > 0 && start - from < EXCERPT_SIDE && !text[from - 1].is_whitespace() {
        from -= 1;
    }
    let mut to = end;
    while to < text.len() && to - end < EXCERPT_SIDE && !text[to].is_whitespace() {
        to += 1;
    }
    text[from..to.min(from + EXCERPT_CHARS)]
        .iter()
        .map(|&c| match crate::untrusted::invisible(c) {
            true => format!("<U+{:04X}>", u32::from(c)),
            false => c.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(file: &str, old: &str, new: &str) -> Result<String, String> {
        apply(
            file,
            &[Edit {
                old,
                new,
                all: false,
            }],
        )
        .map(|done| done.text)
    }

    #[test]
    fn an_exact_match_is_replaced_and_nothing_else() {
        assert_eq!(one("one two three", "two", "2").unwrap(), "one 2 three");
        assert_eq!(find("a b", "b").map(|(how, _)| how), Some(Strategy::Exact));
    }

    #[test]
    fn a_crlf_file_takes_an_lf_old_and_stays_crlf() {
        let file = "fn a() {\r\n    x();\r\n}\r\n";
        let out = one(file, "    x();\n}\n", "    y();\n    z();\n}\n").unwrap();
        assert_eq!(out, "fn a() {\r\n    y();\r\n    z();\r\n}\r\n");
        assert_eq!(
            find(file, "x();\n}").map(|(how, _)| how),
            Some(Strategy::LineEndings)
        );
    }

    #[test]
    fn an_old_that_starts_inside_a_crlf_takes_the_whole_line_break() {
        let file = "a();\r\nfoo();\r\n";
        assert_eq!(
            one(file, "\nfoo();", "\nbar();").unwrap(),
            "a();\r\nbar();\r\n"
        );
        assert_eq!(
            find(file, "\nfoo();").map(|(how, _)| how),
            Some(Strategy::LineEndings)
        );
    }

    #[test]
    fn new_takes_the_line_ending_of_the_file_either_way() {
        assert_eq!(one("a\r\nb\r\n", "b", "b\nc").unwrap(), "a\r\nb\r\nc\r\n");
        assert_eq!(one("a\nb\n", "b", "b\r\nc").unwrap(), "a\nb\nc\n");
    }

    #[test]
    fn trailing_spaces_on_either_side_are_ignored() {
        let file = "let a = 1;   \nlet b = 2;\nlet c = 3;\n";
        let old = "let a = 1;\nlet b = 2;  ";
        assert_eq!(
            find(file, old).map(|(how, _)| how),
            Some(Strategy::TrailingSpace)
        );
        assert_eq!(
            one(file, old, "let a = 10;\nlet b = 20;").unwrap(),
            "let a = 10;\nlet b = 20;\nlet c = 3;\n"
        );
    }

    #[test]
    fn a_block_indented_differently_is_found_and_new_is_moved_to_fit() {
        let file = "impl A {\n        fn f() {\n            go();\n        }\n}\n";
        let old = "fn f() {\n    go();\n}";
        assert_eq!(find(file, old).map(|(how, _)| how), Some(Strategy::Indent));
        assert_eq!(
            one(file, old, "fn f() {\n    stop();\n\n    go();\n}").unwrap(),
            "impl A {\n        fn f() {\n            stop();\n\n            go();\n        }\n}\n"
        );
    }

    #[test]
    fn a_closing_line_less_indented_than_the_first_still_moves_evenly() {
        let file = "mod m {\n    fn f() {\n        go();\n    }\n}\n";
        let out = one(file, "    go();\n}", "    stop();\n}").unwrap();
        assert_eq!(out, "mod m {\n    fn f() {\n        stop();\n    }\n}\n");
    }

    #[test]
    fn a_line_of_new_left_of_all_of_old_is_refused_rather_than_guessed() {
        let file = "fn f() {\n    if x {\n        go();\n    }\n}\n";
        let old = "            go();\n        }";
        assert_eq!(
            one(file, old, "            stop();\n        }").unwrap(),
            "fn f() {\n    if x {\n        stop();\n    }\n}\n"
        );
        // Lines left of all of `old` could go at any depth the file has.
        let new = "            go();\n        }\n    }\n    fn g() {";
        let error = one(file, old, new).unwrap_err();
        assert!(error.contains("indented as the file is"), "{error}");
    }

    #[test]
    fn indentation_that_moved_unevenly_is_not_a_match() {
        let file = "    a();\n    b();\n";
        // `old` indents the second line under the first; the file does not.
        assert!(one(file, "a();\n  b();", "c();").is_err());
    }

    #[test]
    fn curly_quotes_and_dashes_match_their_plain_forms() {
        let file = "say(“it’s here — now…”);\r\nnext();\r\n";
        let old = "say(\"it's here - now...\");\nnext();";
        assert_eq!(find(file, old).map(|(how, _)| how), Some(Strategy::Quotes));
        assert_eq!(
            one(file, old, "say(\"gone\");\nnext();").unwrap(),
            "say(\"gone\");\r\nnext();\r\n"
        );
    }

    #[test]
    fn characters_nobody_sees_need_not_be_in_old() {
        // A zero-width space, a right-to-left override and its pop, and tag
        // characters, in a CRLF file; those after the match stay put.
        let file = "let a\u{200B} = \u{202E}1\u{202C};\r\nlet b = 2;\u{E0072}\u{E006D}\r\n";
        let old = "let a = 1;\nlet b = 2;";
        assert_eq!(
            find(file, old).map(|(how, _)| how),
            Some(Strategy::Invisible)
        );
        assert_eq!(
            one(file, old, "let a = 3;\nlet b = 4;").unwrap(),
            "let a = 3;\r\nlet b = 4;\u{E0072}\u{E006D}\r\n"
        );
        // Separators read as the line breaks they show as, and a joiner is
        // found whether or not `old` kept it.
        assert_eq!(one("a\u{2028}b", "a\nb", "c").unwrap(), "c");
        let file = "\u{1F469}\u{200D}\u{1F4BB} x\u{200B}y";
        for old in ["\u{1F469}\u{200D}\u{1F4BB} xy", "\u{1F469}\u{1F4BB} xy"] {
            assert_eq!(one(file, old, "z").unwrap(), "z", "{old:?}");
        }
    }

    #[test]
    fn hidden_characters_go_with_the_character_after_them() {
        // Those before the first character found go with it, those after the
        // last stay, and a place never starts between two of them.
        let file = "x\u{200B}\u{200B}b\u{200C}c\u{200B}d";
        assert_eq!(one(file, "bc", "BC").unwrap(), "xBC\u{200B}d");
        // So a patch's line still starts its line when the file's starts
        // with one.
        let (out, end) = replace_lines("\u{200B}foo\nbar\n", 0, 2, "foo\n", "baz\n").unwrap();
        assert_eq!((out.as_str(), end), ("baz\nbar\n", 4));
    }

    #[test]
    fn a_copy_missing_only_hidden_characters_beats_one_missing_spaces() {
        // `old` is the second line exactly as `untrusted::visible` shows it.
        assert_eq!(
            one("ab  \na\u{200B}b\n", "ab\n", "c\n").unwrap(),
            "ab  \nc\n"
        );
        // A copy found as it is wins over both.
        assert_eq!(one("a\u{200B}b ab", "ab", "c").unwrap(), "a\u{200B}b c");
    }

    #[test]
    fn replace_all_takes_every_place_hidden_characters_hid() {
        let every = |file, old| {
            apply(
                file,
                &[Edit {
                    old,
                    new: "z",
                    all: true,
                }],
            )
            .map(|done| done.text)
        };
        assert_eq!(every("x\u{200B}y and x\u{200D}y", "xy").unwrap(), "z and z");
        // Hidden characters alone are nothing to find, not everywhere.
        assert!(every("abc", "\u{200B}").is_err());
    }

    #[test]
    fn hidden_characters_typed_into_old_are_looked_for_as_they_are() {
        // A cleanup made twice: the first call takes the zero-width spaces,
        // and the second must not take every line break left instead.
        let cleanup = [Edit {
            old: "\u{200B}\n",
            new: "",
            all: true,
        }];
        let once = apply("one\u{200B}\ntwo\nthree\u{200B}\nfour\n", &cleanup).unwrap();
        assert_eq!(once.text, "onetwo\nthreefour\n");
        assert!(apply(&once.text, &cleanup).is_err());
        // Nor every space, nor every line break for a paragraph separator.
        for old in ["\u{2060} ", "\u{2029}"] {
            let edit = [Edit {
                old,
                new: "",
                all: true,
            }];
            assert!(apply("let a = b + c;\nlet d;\n", &edit).is_err(), "{old:?}");
        }
    }

    #[test]
    fn a_match_that_splits_an_ellipsis_is_not_taken() {
        assert!(one("wait…", "wait..", "x").is_err());
    }

    #[test]
    fn the_byte_order_mark_is_kept_and_never_in_the_way() {
        let file = "\u{FEFF}first  \r\nsecond\r\n";
        let out = one(file, "first\nsecond", "one\ntwo").unwrap();
        assert_eq!(out, "\u{FEFF}one\r\ntwo\r\n");
        // read_file shows the mark, so a copied first line may carry it too.
        let out = one(file, "\u{FEFF}first", "\u{FEFF}one").unwrap();
        assert_eq!(out, "\u{FEFF}one  \r\nsecond\r\n");
    }

    #[test]
    fn line_numbers_copied_from_read_file_are_dropped_from_old() {
        let file = "a\nb\nc\n";
        assert_eq!(one(file, "2|b\n3|c", "B\nC").unwrap(), "a\nB\nC\n");
        // Numbers that do not count up are part of the text, and not found.
        assert!(one(file, "2|b\n5|c", "x").is_err());
        // A file that holds the numbers itself is edited as it stands.
        assert_eq!(one("1|b\nb\n", "1|b", "x").unwrap(), "x\nb\n");
    }

    #[test]
    fn line_numbers_copied_into_new_as_well_stay_out_of_the_file() {
        let file = "fn a() {\n    x();\n}\n";
        assert_eq!(
            one(file, "2|    x();", "2|    y();").unwrap(),
            "fn a() {\n    y();\n}\n"
        );
        assert_eq!(
            one(file, "2|    x();\n3|}", "2|    y();\n3|    z();\n4|}").unwrap(),
            "fn a() {\n    y();\n    z();\n}\n"
        );
        // An `old` found as sent means the numbers are the file's own.
        assert_eq!(one("1|b\n", "1|b", "2|c").unwrap(), "2|c\n");
    }

    #[test]
    fn an_edit_already_made_changes_nothing() {
        let done = apply(
            "let x = 2;\n",
            &[Edit {
                old: "let x = 1;",
                new: "let x = 2;",
                all: false,
            }],
        )
        .unwrap();
        assert_eq!((done.replaced, done.already), (0, 1));
        assert_eq!(done.text, "let x = 2;\n");
        // `new` found twice could be anything, so it is still a miss.
        assert!(one("b b", "a", "b").is_err());
    }

    #[test]
    fn a_mistyped_deletion_is_not_reported_as_already_applied() {
        let file = "def f(x):\n    print(x)\n    return x\n";
        // `new` is the tail of `old`, so the file holds it whether or not
        // the line above was ever deleted.
        let (old, new) = ("    print(y)\n    return x\n", "    return x\n");
        let error = one(file, old, new).unwrap_err();
        assert!(error.contains("was not found"), "{error}");
        // Copied with read_file's numbers, `old` holds `new` only once they
        // are gone, since they sit between its lines.
        let numbered = one(
            "def f(x):\n    print(x)\n    a = 1\n    return a\n",
            "2|    print(y)\n3|    a = 1\n4|    return a",
            "    a = 1\n    return a",
        );
        assert!(numbered.unwrap_err().contains("was not found"));
        // Beside an edit that can be made, it fails the call rather than
        // hiding behind the one that worked.
        let edits = [
            Edit {
                old: "def f(x)",
                new: "def g(x)",
                all: false,
            },
            Edit {
                old,
                new,
                all: false,
            },
        ];
        let error = apply(file, &edits).err().unwrap();
        assert!(error.starts_with("Edit 2 of 2:"), "{error}");
    }

    #[test]
    fn an_edit_repeated_exactly_is_still_reported_as_already_applied() {
        let edit = [Edit {
            old: "    one();\n}",
            new: "    two();\n    three();\n}",
            all: false,
        }];
        let once = apply("fn a() {\r\n    one();\r\n}\r\n", &edit).unwrap();
        assert_eq!(once.text, "fn a() {\r\n    two();\r\n    three();\r\n}\r\n");
        // `new` went in with the file's CRLF, so only its line endings differ.
        let twice = apply(&once.text, &edit).unwrap();
        assert_eq!((twice.replaced, twice.already), (0, 1));
        assert_eq!(twice.text, once.text);
    }

    #[test]
    fn a_loose_match_of_new_is_not_taken_as_already_applied() {
        let file = "say(“hi”);\n";
        let new = "say(\"hi\");";
        assert_eq!(find(file, new).map(|(how, _)| how), Some(Strategy::Quotes));
        let error = one(file, "say(\"bye\");", new).unwrap_err();
        assert!(error.contains("was not found"), "{error}");

        let file = "fn f() {\n        a();\n        b();\n}\n";
        let new = "a();\nb();";
        assert_eq!(find(file, new).map(|(how, _)| how), Some(Strategy::Indent));
        let error = one(file, "c();\nd();", new).unwrap_err();
        assert!(error.contains("was not found"), "{error}");

        let file = "a\u{200B}b\n";
        assert_eq!(
            find(file, "ab").map(|(how, _)| how),
            Some(Strategy::Invisible)
        );
        let error = one(file, "zz", "ab").unwrap_err();
        assert!(error.contains("was not found"), "{error}");
    }

    #[test]
    fn more_than_one_match_is_refused() {
        let error = one("x = 1;\nx = 1;\n", "x = 1;", "x = 2;").unwrap_err();
        assert!(error.contains("matches 2 times"), "{error}");
        // A closer match wins outright, never counted with looser ones.
        assert_eq!(one("a  \na\n", "a\n", "b\n").unwrap(), "a  \nb\n");
    }

    #[test]
    fn an_old_text_matching_twice_says_where() {
        let error = one("a\nx = 1;\nb\nx = 1;\n", "x = 1;", "x = 2;").unwrap_err();
        assert_eq!(
            error,
            "`old` matches 2 times, at lines 2 and 4; include more context, or set \
             replace_all to change every one."
        );
        let error = one("x x x", "x", "y").unwrap_err();
        assert!(error.contains("3 times, at line 1;"), "{error}");
        let many = "x\n".repeat(NAMED_LINES + 2);
        let error = one(&many, "x", "y").unwrap_err();
        assert!(
            error.contains("at lines 1, 2, 3, 4, 5, 6, 7, 8, 9, 10 and 2 more places;"),
            "{error}"
        );
    }

    #[test]
    fn replace_all_changes_every_match() {
        let done = apply(
            "x = 1;\r\ny = x;\r\nx = 1;\r\n",
            &[Edit {
                old: "x = 1;\n",
                new: "x = 2;\n",
                all: true,
            }],
        )
        .unwrap();
        assert_eq!(done.text, "x = 2;\r\ny = x;\r\nx = 2;\r\n");
        assert_eq!(done.replaced, 2);
        // Quotes made plain are too loose to change more than one place.
        let loose = apply(
            "“a” “a”",
            &[Edit {
                old: "\"a\"",
                new: "b",
                all: true,
            }],
        );
        assert!(loose.is_err());
    }

    #[test]
    fn edits_apply_in_order_and_all_or_none() {
        let edits = [
            Edit {
                old: "one",
                new: "two",
                all: false,
            },
            Edit {
                old: "two three",
                new: "done",
                all: false,
            },
        ];
        let done = apply("one three", &edits).unwrap();
        assert_eq!((done.text.as_str(), done.replaced), ("done", 2));

        let edits = [
            Edit {
                old: "one",
                new: "two",
                all: false,
            },
            Edit {
                old: "missing",
                new: "x",
                all: false,
            },
        ];
        let error = apply("one three", &edits).err().unwrap();
        assert!(error.starts_with("Edit 2 of 2:"), "{error}");
    }

    #[test]
    fn an_empty_old_is_refused() {
        assert!(one("text", "", "x").is_err());
    }

    /// A short file with a line the tests below mistype.
    const TOTAL: &str = "fn main() {\n    let total = foo_bar(1);\n    println!(\"{total}\");\n}\n";

    #[test]
    fn a_one_character_typo_names_the_line_and_the_difference() {
        let error = one(TOTAL, "let total = foo-bar(1);", "x").unwrap_err();
        assert_eq!(
            error,
            "`old` was not found in the file.\nClosest text, line 2 (95% alike):\n\
             2|    let total = foo_bar(1);\n\
             Differs at 2:20: the file has `foo_bar(1);`, `old` has `foo-bar(1);`."
        );
    }

    #[test]
    fn a_missing_line_is_named() {
        let file = "fn total(items: &[u32]) -> u32 {\n    let mut sum = 0;\n    \
                    for item in items {\n        sum += item;\n    }\n    sum\n}\n";
        let old = "    let mut sum = 0;\n    for item in items {\n        sum += *item * 2;\n        \
                   sum += item;\n    }";
        assert_eq!(
            one(file, old, "x").unwrap_err(),
            "`old` was not found in the file.\nClosest text, lines 2-5 (80% alike):\n\
             2|    let mut sum = 0;\n3|    for item in items {\n4|        sum += item;\n5|    }\n\
             `old` has `sum += *item * 2;` the file does not have, after line 3."
        );
        // And a line the file has that `old` left out.
        let old = "    for item in items {\n    }\n    sum";
        assert_eq!(
            one(file, old, "x").unwrap_err(),
            "`old` was not found in the file.\nClosest text, lines 3-6 (75% alike):\n\
             3|    for item in items {\n4|        sum += item;\n5|    }\n6|    sum\n\
             Line 4 of the file is not in `old`: `sum += item;`."
        );
    }

    #[test]
    fn case_backslash_and_invisible_differences_are_named() {
        let file = concat!(
            r#"let path = "C:\\Users\\data";"#,
            "\n",
            r#"println!("Hello World");"#
        );
        let error = one(file, r#"let path = "C:\Users\data";"#, "x").unwrap_err();
        assert!(
            error.ends_with(
                r#"Differs at 1:16 in backslashes: the file has `"C:\\Users\\data";`, `old` has `"C:\Users\data";`."#
            ),
            "{error}"
        );
        let error = one(file, r#"println!("hello world");"#, "x").unwrap_err();
        assert!(
            error.ends_with(
                r#"Differs at 2:11 in letter case: the file has `println!("Hello World");`, `old` has `println!("hello world");`."#
            ),
            "{error}"
        );
        // edit_file forgives one the file has, and never one `old` has.
        let error = one("let ab = 1;\n", "let a\u{200B}b = 1;", "x").unwrap_err();
        assert!(
            error.ends_with(
                "Differs at 1:6 by an invisible character: the file has `ab`, `old` has \
                 `a<U+200B>b`."
            ),
            "{error}"
        );
    }

    #[test]
    fn quotes_and_indentation_differing_together_are_named() {
        let file = "    say(\u{201C}hi\u{201D});\n    next();\n";
        let error = one(file, "say(\"hi\");\nnext();", "x").unwrap_err();
        assert!(
            error.ends_with(
                "\nThe quotes and the spacing at line starts or ends both differ; edit_file \
                 forgives one at a time."
            ),
            "{error}"
        );
        // Either alone is forgiven.
        assert!(one(file, "say(\"hi\");\n    next();", "x").is_ok());
        assert!(one(file, "say(\u{201C}hi\u{201D});\nnext();", "x").is_ok());
    }

    #[test]
    fn two_lookalike_places_are_both_shown() {
        let file = "fn first(items: &[u32]) -> u32 {\n    items.iter().map(|item| item * 2).sum()\n}\n\n\
                    fn second(items: &[u32]) -> u32 {\n    items.iter().map(|item| item * 3).sum()\n}\n";
        let error = one(file, "items.iter().map(|item| item * 4).sum()", "x").unwrap_err();
        assert_eq!(
            error,
            "`old` was not found in the file.\nClosest text, line 2 (97% alike):\n\
             2|    items.iter().map(|item| item * 2).sum()\n\
             Differs at 2:36: the file has `2).sum()`, `old` has `4).sum()`.\n\
             Also close, line 6 (97% alike):\n\
             6|    items.iter().map(|item| item * 3).sum()\n\
             Differs at 6:36: the file has `3).sum()`, `old` has `4).sum()`."
        );
    }

    #[test]
    fn old_sent_with_read_file_numbers_still_finds_its_place() {
        let old = "2|    let total = foo-bar(1);\n3|    println!(\"{total}\");";
        assert_eq!(
            one(TOTAL, old, "x").unwrap_err(),
            "`old` was not found in the file.\nClosest text, lines 2-3 (97% alike):\n\
             2|    let total = foo_bar(1);\n3|    println!(\"{total}\");\n\
             Differs at 2:20: the file has `foo_bar(1);`, `old` has `foo-bar(1);`."
        );
    }

    #[test]
    fn the_second_edit_reports_lines_on_disk() {
        let edits = [
            Edit {
                old: "fn main() {\n",
                new: "// One.\n// Two.\nfn main() {\n",
                all: false,
            },
            Edit {
                old: "let total = foo-bar(1);",
                new: "let total = 2;",
                all: false,
            },
        ];
        assert_eq!(
            apply(TOTAL, &edits).err().unwrap(),
            "Edit 2 of 2: `old` was not found in the file. None of the edits were made.\n\
             Closest text, line 2 (95% alike):\n2|    let total = foo_bar(1);\n\
             Differs at 2:20: the file has `foo_bar(1);`, `old` has `foo-bar(1);`."
        );
        // Text the first edit changed is still on disk as sent.
        let edits = [
            Edit {
                old: "foo_bar(1)",
                new: "baz(1)",
                all: false,
            },
            Edit {
                old: "let total = foo_bar(1);",
                new: "let total = 2;",
                all: false,
            },
        ];
        assert_eq!(
            apply(TOTAL, &edits).err().unwrap(),
            "Edit 2 of 2: `old` was not found in the file. An earlier edit in this call changed \
             that text; make both changes in one edit. None of the edits were made."
        );
    }

    #[test]
    fn a_blank_line_one_side_lacks_is_named_and_not_called_wholly_alike() {
        let error = one("fn a() {\n\n    x();\n}\n", "fn a() {\n    x();\n}", "x");
        assert_eq!(
            error.unwrap_err(),
            "`old` was not found in the file.\nClosest text, lines 1-4 (99% alike):\n\
             1|fn a() {\n2|\n3|    x();\n4|}\nLine 2 of the file is not in `old`: a blank line."
        );
    }

    #[test]
    fn uneven_indentation_alone_is_not_called_wholly_alike() {
        let file = "    if x {\n        y();\n    }\n";
        let error = one(file, "if x {\n      y();\n    }", "x").unwrap_err();
        assert!(error.contains("lines 1-3 (99% alike)"), "{error}");
        assert!(error.contains("Only the spacing at line starts"), "{error}");
    }

    #[test]
    fn a_difference_past_the_compared_characters_is_found() {
        let line = |end: char| format!("let s = \"{}{end}\";", "x".repeat(300));
        let error = one(&line('A'), &line('B'), "x").unwrap_err();
        assert!(error.contains("line 1 (99% alike)"), "{error}");
        assert!(
            error.ends_with(
                "Differs at 1:310: the file has `xxxxxxxxxxxxA\";`, `old` has `xxxxxxxxxxxxB\";`."
            ),
            "{error}"
        );
    }

    #[test]
    fn hidden_characters_in_the_file_never_reach_the_hint() {
        // Tag characters spell text only a model reads; edit_file forgives
        // them, so the typo is what is named.
        let file = "let x\u{E0041}\u{E0042} = foo_bar(1);\n";
        let error = one(file, "let x = foo-bar(1);", "x").unwrap_err();
        assert!(!error.chars().any(crate::untrusted::invisible), "{error:?}");
        assert!(error.contains("\n1|let x = foo_bar(1);\n"), "{error}");
        assert!(
            error.ends_with("the file has `foo_bar(1);`, `old` has `foo-bar(1);`."),
            "{error}"
        );
    }

    #[test]
    fn a_line_too_costly_to_compare_is_not_compared() {
        let text = "let total = foo_bar(1);";
        let want = Want {
            text,
            chars: compared(text),
            free_start: false,
            free_end: false,
        };
        let line = compared("let total = foo-bar(1);");
        let cells = want.chars.len() * line.len();
        let left = Cell::new(cells);
        assert_eq!(alike(&want, &line, &left), Some(95));
        assert_eq!(left.get(), 0);
        let left = Cell::new(cells - 1);
        assert_eq!(alike(&want, &line, &left), None);
        assert_eq!(left.get(), 0);
    }

    #[test]
    fn a_search_cut_short_says_to_read_again_not_that_nothing_is_alike() {
        // Each line has every character `old` has, so each is counted out
        // in full, and the cells run out long before the last.
        let old = "abcdefghij".repeat(25);
        let lines: String = (0..200)
            .map(|_| old.chars().rev().collect::<String>() + "\n")
            .collect();
        let error = one(&lines, &old, "x").unwrap_err();
        assert_eq!(
            error,
            "`old` was not found in the file. Read the file again and copy the text exactly."
        );
        // The same lines short of the cells are searched, and none is alike.
        let error = one(&lines[..lines.len() / 10], &old, "x").unwrap_err();
        assert!(error.contains("No similar text was found"), "{error}");
    }

    #[test]
    fn nothing_is_shown_below_the_threshold() {
        let nothing = "`old` was not found in the file. No similar text was found; grep for a \
                       line of it or read the part you mean.";
        let error = one("alpha\nbeta\ngamma\n", "something else entirely", "x");
        assert_eq!(error.unwrap_err(), nothing);
        // One line of three alike is a third of the place.
        let file = "fn a() {\n    one();\n    two();\n}\n";
        let error = one(file, "fn a() {\n    three_more();\n    four_more();", "x");
        assert_eq!(error.unwrap_err(), nothing);
    }

    #[test]
    fn a_key_in_the_window_is_redacted() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let data = crate::store::tests::Scratch::new("edit-near-miss");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &*data) };
        let key = "a-saved-key-of-no-known-shape";
        std::fs::write(data.join("keys.json"), format!(r#"{{"OpenAI":"{key}"}}"#)).unwrap();
        // The cut at `SHOWN_CHARS` falls inside the key.
        let line = format!("let token = \"{}{key}\";", "x".repeat(SHOWN_CHARS - 20));
        let error = one(&format!("{line}\n"), "let tokn = \"xxxxxxxx", "x");
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
        let error = error.unwrap_err();
        assert!(error.contains("\n1|let token = \"xxx"), "{error}");
        assert!(error.contains("[secret"), "{error}");
        assert!(!error.contains("a-saved"), "{error}");
    }
}
