//! Showing a change before it happens.
//!
//! An approval is only meaningful if you can see what you are approving, and
//! "write_file hello.py" tells you nothing about what is going into it. This
//! turns a pending write or edit into the diff you would get from a review:
//! removals on red, additions on green, a few lines of untouched code either
//! side for bearings, and the long stretches between changes folded away.

use letronna_core::tools::ToolContext;
use serde_json::Value;

use crate::render;

/// Lines of unchanged code kept either side of a change.
const CONTEXT: usize = 2;

/// Above this many lines on either side, the line-by-line comparison is not
/// worth its cost and the file is shown as a straight replacement.
const COMPARE_LIMIT: usize = 600;

/// Rows of diff to show before folding the rest away.
const MAX_ROWS: usize = 28;

/// The diff for a call, or `None` for a tool that does not change a file.
pub fn of(name: &str, args: &str, ctx: &ToolContext) -> Option<Vec<String>> {
    let args: Value = serde_json::from_str(args).ok()?;
    let path = args["path"].as_str().unwrap_or_default();
    match name {
        "write_file" => {
            let new = args["content"].as_str().unwrap_or_default();
            // A file that is not there yet is all addition; one that is gets
            // compared, so an overwrite does not read as a fresh file.
            let old = ctx
                .workspace
                .as_ref()
                .map(|root| root.join(path))
                .and_then(|full| std::fs::read_to_string(full).ok())
                .unwrap_or_default();
            Some(render_diff(&old, new))
        }
        "edit_file" => {
            let old = args["old"].as_str().unwrap_or_default();
            let new = args["new"].as_str().unwrap_or_default();
            Some(render_diff(old, new))
        }
        _ => None,
    }
}

/// One line of the comparison.
#[derive(Clone, Debug, PartialEq)]
enum Row {
    Same(String),
    Gone(String),
    New(String),
}

fn render_diff(old: &str, new: &str) -> Vec<String> {
    let before: Vec<&str> = old.lines().collect();
    let after: Vec<&str> = new.lines().collect();
    let rows = if before.len().max(after.len()) > COMPARE_LIMIT {
        before
            .iter()
            .map(|l| Row::Gone((*l).to_owned()))
            .chain(after.iter().map(|l| Row::New((*l).to_owned())))
            .collect()
    } else {
        compare(&before, &after)
    };
    paint(&fold(rows))
}

/// Line-by-line comparison, longest common subsequence. Quadratic, which is
/// why it is only reached for files small enough for it not to matter.
fn compare(before: &[&str], after: &[&str]) -> Vec<Row> {
    let (rows, columns) = (before.len(), after.len());
    // common[i][j] is the length of the longest run shared by the tails.
    let mut common = vec![vec![0usize; columns + 1]; rows + 1];
    for i in (0..rows).rev() {
        for j in (0..columns).rev() {
            common[i][j] = if before[i] == after[j] {
                common[i + 1][j + 1] + 1
            } else {
                common[i + 1][j].max(common[i][j + 1])
            };
        }
    }

    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < rows && j < columns {
        if before[i] == after[j] {
            out.push(Row::Same(before[i].to_owned()));
            i += 1;
            j += 1;
        } else if common[i + 1][j] >= common[i][j + 1] {
            out.push(Row::Gone(before[i].to_owned()));
            i += 1;
        } else {
            out.push(Row::New(after[j].to_owned()));
            j += 1;
        }
    }
    out.extend(before[i..].iter().map(|l| Row::Gone((*l).to_owned())));
    out.extend(after[j..].iter().map(|l| Row::New((*l).to_owned())));
    out
}

/// Drop the long runs of untouched lines, keeping a little either side of
/// each change so it can be placed.
fn fold(rows: Vec<Row>) -> Vec<Row> {
    let keep: Vec<bool> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            if !matches!(row, Row::Same(_)) {
                return true;
            }
            let from = i.saturating_sub(CONTEXT);
            let to = (i + CONTEXT).min(rows.len() - 1);
            rows[from..=to].iter().any(|r| !matches!(r, Row::Same(_)))
        })
        .collect();

    let mut out = Vec::new();
    let mut skipped = 0usize;
    for (row, keep) in rows.into_iter().zip(keep) {
        if keep {
            if skipped > 0 {
                out.push(Row::Same(format!("… {skipped} unchanged")));
                skipped = 0;
            }
            out.push(row);
        } else {
            skipped += 1;
        }
    }
    if skipped > 0 {
        out.push(Row::Same(format!("… {skipped} unchanged")));
    }
    out
}

fn paint(rows: &[Row]) -> Vec<String> {
    // The block is drawn to a fixed width so the colour behind it is a solid
    // shape rather than a ragged one that stops at each line's last word.
    let width = render::width()
        .saturating_sub(render::PAD.len() + 2)
        .clamp(20, 120);

    let shown = rows.len().min(MAX_ROWS);
    let mut out: Vec<String> = rows
        .iter()
        .take(shown)
        .map(|row| {
            let (mark, text, colour) = match row {
                Row::Same(text) => (" ", text, None),
                Row::Gone(text) => ("-", text, Some((52, 210))),
                Row::New(text) => ("+", text, Some((22, 157))),
            };
            let body = clip(&format!("{mark} {text}"), width);
            match colour {
                Some((back, front)) if render::colour() => {
                    format!(
                        "{}\x1b[48;5;{back}m\x1b[38;5;{front}m{body}\x1b[0m",
                        render::PAD
                    )
                }
                _ => format!("{}{}", render::PAD, render::dim(&body)),
            }
        })
        .collect();

    if rows.len() > shown {
        out.push(format!(
            "{}{}",
            render::PAD,
            render::dim(&format!("… {} more lines", rows.len() - shown))
        ));
    }
    out
}

/// Cut or pad to exactly `width`, so every row of the block is the same
/// length. Tabs are spread out first: a tab inside a coloured run would
/// otherwise take an unpredictable amount of the row.
fn clip(text: &str, width: usize) -> String {
    let text = text.replace('\t', "    ");
    let count = text.chars().count();
    if count > width {
        let mut cut: String = text.chars().take(width.saturating_sub(1)).collect();
        cut.push('…');
        return cut;
    }
    format!("{text}{}", " ".repeat(width - count))
}

#[cfg(test)]
mod tests {
    use super::{Row, compare, fold};

    fn rows(text: &[&str]) -> Vec<String> {
        text.iter().map(|t| (*t).to_owned()).collect()
    }

    #[test]
    fn an_edit_shows_only_what_moved() {
        let before = ["one", "two", "three"];
        let after = ["one", "TWO", "three"];
        let diff = compare(&before, &after);
        assert_eq!(
            diff,
            vec![
                Row::Same("one".into()),
                Row::Gone("two".into()),
                Row::New("TWO".into()),
                Row::Same("three".into()),
            ]
        );
    }

    #[test]
    fn a_new_file_is_all_addition() {
        let after = rows(&["hello", "world"]);
        let after: Vec<&str> = after.iter().map(String::as_str).collect();
        let diff = compare(&[], &after);
        assert!(diff.iter().all(|r| matches!(r, Row::New(_))));
        assert_eq!(diff.len(), 2);
    }

    #[test]
    fn long_runs_of_untouched_lines_are_folded_away() {
        let mut rows = vec![Row::Same("x".into()); 20];
        rows.push(Row::New("here".into()));
        let folded = fold(rows);
        // The change, the lines either side of it, and one line saying how
        // much was left out.
        assert!(folded.len() < 8);
        assert!(folded.iter().any(|r| *r == Row::New("here".into())));
        assert!(
            folded
                .iter()
                .any(|r| matches!(r, Row::Same(text) if text.contains("unchanged")))
        );
    }
}
