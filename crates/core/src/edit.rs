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
        let made = replace(&text, &edit, ending).map_err(|e| match edits.len() {
            1 => e,
            total => format!(
                "Edit {} of {total}: {e} None of the edits were made.",
                n + 1
            ),
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
/// when the edit is in the text already.
fn replace(
    text: &str,
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
        return Err(
            "`old` was not found in the file. Read it again and copy the text exactly.".into(),
        );
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
/// matched mid-line would change the wrong code.
pub(crate) fn replace_lines(
    file: &str,
    from: usize,
    old: &str,
    new: &str,
) -> Result<(String, usize), String> {
    let text = &file[from..];
    let starts_line = |span: &Range<usize>| span.start == 0 || text[..span.start].ends_with('\n');
    let Some((strategy, spans)) = find_where(text, old, &starts_line) else {
        return Err(
            "its ` ` and `-` lines are not in the file after the hunks before it. \
             Read the file again and copy them exactly."
                .into(),
        );
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
        let (out, end) = replace_lines("\u{200B}foo\nbar\n", 0, "foo\n", "baz\n").unwrap();
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
}
