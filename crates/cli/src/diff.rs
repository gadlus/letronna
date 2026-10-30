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
    changed(name, args, ctx).map(|(_, edits)| edits.iter().flat_map(|rows| paint(rows)).collect())
}

/// A change to a file in plain words for another screen to show, as a
/// phone shows a cloud run's: the file, how many lines it adds and takes
/// away, and its diff as text, each row marked "+", "-" or " ".
pub struct Plain {
    pub path: String,
    pub added: usize,
    pub removed: usize,
    pub rows: Vec<String>,
}

/// `of` in plain words, read before the call runs as `of` is, or `None`
/// for a tool that does not change a file.
pub fn plain(name: &str, args: &str, ctx: &ToolContext) -> Option<Plain> {
    let (path, edits) = changed(name, args, ctx)?;
    let rows: Vec<Row> = edits.into_iter().flatten().collect();
    let count = |new: bool| {
        rows.iter()
            .filter(|row| match row {
                Row::New(_) => new,
                Row::Gone(_) => !new,
                Row::Same(_) => false,
            })
            .count()
    };
    Some(Plain {
        path,
        added: count(true),
        removed: count(false),
        rows: rows
            .iter()
            .map(|row| match row {
                Row::Same(text) => format!("  {text}"),
                Row::Gone(text) => format!("- {text}"),
                Row::New(text) => format!("+ {text}"),
            })
            .collect(),
    })
}

/// The file a call changes and the rows of each of its edits, folded, or
/// `None` for a tool that does not change a file.
fn changed(name: &str, args: &str, ctx: &ToolContext) -> Option<(String, Vec<Vec<Row>>)> {
    let mut args: Value = serde_json::from_str(args).ok()?;
    // Read under the names the tool runs with, or an edit sent as
    // `old_string` would be approved as an empty diff.
    letronna_core::tools::rename_aliases(name, &mut args);
    let path = args["path"].as_str().unwrap_or_default();
    let edits = match name {
        "write_file" => {
            let new = args["content"].as_str().unwrap_or_default();
            // A file that is not there yet is all addition; one that is gets
            // compared, so an overwrite does not read as a fresh file.
            let old = ctx
                .workspace
                .as_ref()
                // Read before the user answers, so a path Windows would take
                // to another computer is shown as new rather than read.
                .filter(|root| letronna_core::paths::unsafe_raw(path, Some(root)).is_none())
                .map(|root| root.join(path))
                .and_then(|full| std::fs::read_to_string(full).ok())
                .unwrap_or_default();
            vec![compared(&old, new)]
        }
        "edit_file" => {
            // Edits sent as a list are shown too, or they would be approved
            // unseen.
            let listed = args["edits"].as_array().into_iter().flatten();
            let mut edits = Vec::new();
            for edit in std::iter::once(&args).chain(listed) {
                let Some(old) = edit["old"].as_str() else {
                    continue;
                };
                let new = edit["new"].as_str().unwrap_or_default();
                edits.push(compared(old, new));
            }
            edits
        }
        _ => return None,
    };
    Some((path.to_owned(), edits))
}

/// One line of the comparison.
#[derive(Clone, Debug, PartialEq)]
enum Row {
    Same(String),
    Gone(String),
    New(String),
}

/// The rows from `old` to `new`, folded.
fn compared(old: &str, new: &str) -> Vec<Row> {
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
    fold(rows)
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
        .flat_map(|row| {
            let theme = crate::theme::now();
            let (mark, text, paint) = match row {
                Row::Same(text) => (" ", text, None),
                Row::Gone(text) => ("-", text, Some(theme.gone)),
                Row::New(text) => ("+", text, Some(theme.new)),
            };
            // A changed line longer than the block goes on under itself
            // rather than being cut, which could hide the change itself.
            pieces(&format!("{mark} {text}"), width)
                .into_iter()
                .map(move |body| match paint {
                    Some(paint) if render::colour() => {
                        format!("{}{}", render::PAD, painted(paint, &body))
                    }
                    _ => format!("{}{}", render::PAD, render::dim(&body)),
                })
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

/// A changed row in its colours. The background and the default ink are set
/// first, and the code colours itself on top. Anything it leaves alone falls
/// back to the ink set here, which is what keeps a removed line readable on
/// red. With no background the ink is all that marks the row, so the code
/// is left in it rather than coloured over.
fn painted(paint: crate::theme::Paint, body: &str) -> String {
    match paint.back {
        Some(back) => format!(
            "\x1b[{back}m\x1b[{}m{}\x1b[0m",
            paint.ink,
            crate::syntax::paint(body)
        ),
        None => format!("\x1b[{}m{body}\x1b[0m", paint.ink),
    }
}

/// A diff row as rows `width` wide, each padded to it: the first with its
/// mark, the rest under the text after the mark.
fn pieces(text: &str, width: usize) -> Vec<String> {
    let text = text.replace('\t', "    ");
    let chars: Vec<char> = text.chars().collect();
    let mut rows = Vec::new();
    let mut at = 0;
    while at < chars.len() || rows.is_empty() {
        let lead = if rows.is_empty() { "" } else { "  " };
        let room = width.saturating_sub(lead.len()).max(1);
        let end = (at + room).min(chars.len());
        let piece: String = chars[at..end].iter().collect();
        rows.push(clip(&format!("{lead}{piece}"), width));
        at = end;
    }
    rows
}

/// One line of a git patch as `/diff` shows it, cut to `width`: what goes
/// in red and what comes in green, as an approval's diff has them, with
/// the file and hunk headers set apart from the code.
pub fn patch_line(line: &str, width: usize) -> String {
    let line = render::clip(&line.replace('\t', "    "), width);
    if line.starts_with("diff ") || line.starts_with("+++ ") || line.starts_with("--- ") {
        return render::bold(&line);
    }
    match line.chars().next() {
        Some('+') => render::green(&line),
        Some('-') => render::red(&line),
        Some('@') => render::blue(&line),
        Some(' ') | None => line,
        // Git's own notes: the index line, modes, "No newline at end".
        Some(_) => render::dim(&line),
    }
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
    use super::{Row, ToolContext, compare, fold, of, painted, patch_line};

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
    fn every_edit_in_a_list_is_shown() {
        let args = r#"{"path": "a.rs", "edits": [
            {"old": "one", "new": "two"},
            {"old": "three", "new": "four"}
        ]}"#;
        let shown = of("edit_file", args, &ToolContext::default())
            .unwrap()
            .join("\n");
        for text in ["- one", "+ two", "- three", "+ four"] {
            assert!(shown.contains(text), "{shown}");
        }
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

    #[test]
    fn an_edit_sent_with_other_names_shows_its_diff() {
        let edit = r#"{"file_path": "a.txt", "old_string": "two", "new_string": "three"}"#;
        let rows = of("edit_file", edit, &ToolContext::default())
            .unwrap()
            .join("\n");
        assert!(rows.contains("- two"), "{rows}");
        assert!(rows.contains("+ three"), "{rows}");
    }

    #[cfg(windows)]
    #[test]
    fn a_device_path_is_not_read_for_the_preview() {
        let dir = std::env::temp_dir().join(format!("letronna-preview-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("old.txt"), "unread_before").unwrap();
        let mut ctx = letronna_core::tools::ToolContext::default();
        ctx.workspace = Some(dir.clone());
        let shown = |path: String| {
            let args = serde_json::json!({"path": path, "content": "new"}).to_string();
            super::of("write_file", &args, &ctx).unwrap().join("\n")
        };
        // Named plainly the file is read, so only the check leaves it unread.
        assert!(shown("old.txt".into()).contains("unread_before"));
        let device = format!(r"\\?\{}", dir.join("old.txt").display());
        assert!(!shown(device).contains("unread_before"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_patch_line_keeps_its_text_and_fits_the_width() {
        let plain = |line: &str| crate::render::plain(&patch_line(line, 8));
        assert_eq!(plain("+added"), "+added");
        assert_eq!(plain("-a longer removed line"), "-a long…");
        assert_eq!(plain(" \tkept"), "     ke…");
    }

    #[test]
    fn a_long_changed_line_goes_on_under_itself() {
        let long = format!("+ {}", "y".repeat(50));
        let rows = super::pieces(&long, 20);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| row.chars().count() == 20), "{rows:?}");
        assert!(rows[1].starts_with("  y"));
        assert_eq!(super::pieces("  short", 20).len(), 1);
    }

    #[test]
    fn a_changed_row_in_the_default_theme_is_drawn_as_it_always_was() {
        let dark = crate::theme::DARK;
        // Byte for byte what the rows were written as before themes.
        assert_eq!(
            painted(dark.gone, "- old"),
            "\x1b[48;5;52m\x1b[38;5;210m- old\x1b[0m"
        );
        assert_eq!(
            painted(dark.new, "+ new"),
            "\x1b[48;5;22m\x1b[38;5;157m+ new\x1b[0m"
        );
        let ink_alone = crate::theme::Paint {
            back: None,
            ink: "31",
        };
        assert_eq!(painted(ink_alone, "- old"), "\x1b[31m- old\x1b[0m");
    }
}
