//! A Jupyter notebook as read_file shows it: each cell numbered, with its
//! source and a short form of what it printed, in place of the JSON on
//! disk, where every line of code is a quoted string and a plot is a page
//! of base64.
//!
//! Only text reaches the view. A picture or an HTML table becomes one line
//! naming its type, and a cell's outputs stop after `OUTPUT_LINES`, since a
//! training loop can log thousands. The view drops the ids and metadata a
//! notebook keeps, so it is for reading only; nothing writes one back.

use serde_json::Value;
use std::path::Path;

/// Output lines shown under a cell, all its outputs together: room for a
/// result or an error, where a longer log is mostly more of the same.
const OUTPUT_LINES: usize = 20;

/// Whether `path` is named as a Jupyter notebook.
pub(crate) fn is_notebook(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("ipynb"))
}

/// A notebook's cells as lines of text, or why `bytes` do not read as one.
pub(crate) fn cells(bytes: &[u8]) -> Result<String, String> {
    let notebook: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("it is not JSON ({e})"))?;
    let cells = notebook["cells"]
        .as_array()
        .filter(|cells| !cells.is_empty());
    let Some(cells) = cells else {
        return Err("it has no cells".into());
    };
    // One string, not a string a line: a log of millions of short lines
    // would otherwise take gigabytes to show twenty of them.
    let mut out = String::new();
    for (n, cell) in cells.iter().enumerate() {
        if n > 0 {
            out.push_str("\n\n");
        }
        let kind = cell["cell_type"].as_str().unwrap_or("cell");
        out.push_str(&format!("[{}] {kind}", n + 1));
        for line in text(&cell["source"]).lines() {
            push_line(&mut out, line);
        }
        let (mut shown, mut more) = (0, 0);
        for output in cell["outputs"].as_array().into_iter().flatten() {
            // Loggers and progress bars colour what they print, and a
            // colour code is only noise in the view.
            let text = crate::tools::strip_escapes(&output_text(output));
            for line in lines(&text) {
                if shown == OUTPUT_LINES {
                    more += 1;
                    continue;
                }
                if shown == 0 {
                    push_line(&mut out, "[output]");
                }
                shown += 1;
                push_line(&mut out, line);
            }
        }
        if more > 0 {
            push_line(&mut out, &format!("[{more} more lines of output cut]"));
        }
    }
    Ok(out)
}

/// Adds `line` to `out` on a line of its own.
fn push_line(out: &mut String, line: &str) {
    out.push('\n');
    out.push_str(line);
}

/// What one output shows: what it printed, its error, or its value as text,
/// with a line naming each other form of the value.
fn output_text(output: &Value) -> String {
    match output["output_type"].as_str() {
        Some("stream") => text(&output["text"]),
        Some("error") => format!(
            "{}: {}",
            output["ename"].as_str().unwrap_or("Error"),
            output["evalue"].as_str().unwrap_or_default()
        ),
        _ => {
            let data = &output["data"];
            let mut shown = text(&data["text/plain"]);
            let kinds = data.as_object().into_iter().flat_map(|data| data.keys());
            for kind in kinds.filter(|kind| *kind != "text/plain") {
                if !shown.is_empty() && !shown.ends_with('\n') {
                    shown.push('\n');
                }
                shown.push_str(&format!("[{kind} output]"));
            }
            shown
        }
    }
}

/// Output text as lines, each only as last written over: a progress bar
/// redraws itself after a carriage return, and would otherwise show every
/// step it took on one line.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .map(|line| line.rsplit('\r').find(|part| !part.is_empty()))
        .map(Option::unwrap_or_default)
}

/// A multiline field, which nbformat stores as one string or as a list of
/// lines that each keep their line break.
fn text(value: &Value) -> String {
    match value {
        Value::Array(parts) => parts.iter().filter_map(Value::as_str).collect(),
        _ => value.as_str().unwrap_or_default().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn render(notebook: Value) -> String {
        cells(notebook.to_string().as_bytes()).unwrap()
    }

    #[test]
    fn a_notebook_reads_as_numbered_cells() {
        let notebook = json!({"nbformat": 4, "metadata": {}, "cells": [
            {"cell_type": "code", "execution_count": 1, "metadata": {},
             "source": ["import math\n", "print('pi')\n", "math.pi"],
             "outputs": [
                {"output_type": "stream", "name": "stdout", "text": ["pi\n"]},
                {"output_type": "execute_result", "execution_count": 1,
                 "data": {"text/plain": "3.141592653589793"}, "metadata": {}}
             ]},
            {"cell_type": "markdown", "metadata": {}, "source": "# Results\nAll *good*."},
            {"cell_type": "code", "execution_count": 2, "metadata": {},
             "source": "plt.plot([1, 2])",
             "outputs": [{"output_type": "display_data", "metadata": {}, "data": {
                "text/plain": ["<Figure size 640x480 with 1 Axes>"],
                "image/png": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB"
             }}]},
            {"cell_type": "code", "execution_count": 3, "metadata": {},
             "source": "df.style",
             "outputs": [{"output_type": "execute_result", "execution_count": 3,
                "metadata": {}, "data": {"text/html": "<table></table>"}}]},
            {"cell_type": "code", "execution_count": 4, "metadata": {},
             "source": "1 / 0",
             "outputs": [{"output_type": "error", "ename": "ZeroDivisionError",
                "evalue": "division by zero",
                "traceback": ["\u{1b}[0;31m---------\u{1b}[0m", "long and coloured"]}]},
            {"cell_type": "code", "execution_count": null, "metadata": {},
             "source": [], "outputs": []}
        ]});
        assert_eq!(
            render(notebook),
            "[1] code\nimport math\nprint('pi')\nmath.pi\n[output]\npi\n3.141592653589793\n\
             \n[2] markdown\n# Results\nAll *good*.\n\
             \n[3] code\nplt.plot([1, 2])\n[output]\n<Figure size 640x480 with 1 Axes>\n\
             [image/png output]\n\
             \n[4] code\ndf.style\n[output]\n[text/html output]\n\
             \n[5] code\n1 / 0\n[output]\nZeroDivisionError: division by zero\n\
             \n[6] code"
        );
    }

    #[test]
    fn a_cell_shows_only_the_start_of_a_long_output() {
        let log: String = (1..=50).map(|n| format!("step {n}\n")).collect();
        let notebook = json!({"cells": [{"cell_type": "code", "source": "train()", "outputs": [
            {"output_type": "stream", "name": "stdout", "text": log},
            {"output_type": "stream", "name": "stderr", "text": "done\n"}
        ]}]});
        let steps: Vec<String> = (1..=20).map(|n| format!("step {n}")).collect();
        assert_eq!(
            render(notebook),
            format!(
                "[1] code\ntrain()\n[output]\n{}\n[31 more lines of output cut]",
                steps.join("\n")
            )
        );
    }

    #[test]
    fn a_log_of_a_million_lines_is_counted_not_kept() {
        let log = "ok\n".repeat(1_000_000);
        let notebook = json!({"cells": [{"cell_type": "code", "source": "x", "outputs": [
            {"output_type": "stream", "name": "stdout", "text": log}
        ]}]});
        let shown = render(notebook);
        let head = format!("[1] code\nx\n[output]\n{}", ["ok"; 20].join("\n"));
        assert_eq!(shown, format!("{head}\n[999980 more lines of output cut]"));
    }

    #[test]
    fn a_progress_bar_shows_where_it_ended() {
        let bar = "\r 10%|#         |\r 50%|#####     |\r\u{1b}[32m100%|##########|\u{1b}[0m\r\n";
        let notebook = json!({"cells": [{"cell_type": "code", "source": "fit()", "outputs": [
            {"output_type": "stream", "name": "stderr", "text": bar}
        ]}]});
        assert_eq!(
            render(notebook),
            "[1] code\nfit()\n[output]\n100%|##########|"
        );
    }

    #[test]
    fn what_is_not_a_notebook_says_why() {
        let broken = cells(b"{\"cells\": [").unwrap_err();
        assert!(broken.starts_with("it is not JSON ("), "{broken}");
        for empty in [json!({"cells": []}), json!({"worksheets": []}), json!([1])] {
            let refused = cells(empty.to_string().as_bytes());
            assert_eq!(refused, Err("it has no cells".into()), "{empty}");
        }
        assert!(is_notebook(Path::new("a/Analysis.IPYNB")));
        assert!(!is_notebook(Path::new("ipynb")));
    }
}
