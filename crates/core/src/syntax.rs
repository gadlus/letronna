//! A note in a file tool's result when the code it wrote no longer parses.
//!
//! check_syntax refuses broken JSON and TOML, which stop a whole project.
//! Code is different: an edit that breaks it is often the first of several,
//! so nothing is refused here. But a lost `)`, or a Python block edit_file
//! re-indented the wrong way, is said in the same result, instead of being
//! found a build or a run later, if at all. The parser is tree-sitter, with
//! the grammars the window already links for highlighting, so nothing from
//! the project runs -- a compiler or a language server would -- and a folder
//! cloned a minute ago is as safe to edit as any. Python's grammar there lets
//! a stray indent and an empty block through, so its indentation is checked
//! on the tree as Python itself would.
//!
//! A grammar can be wrong where the compiler is right, so the note is said
//! only where the write is most likely to blame: when the file parsed with no
//! error before, or when it is new and in a language whose grammar rejected
//! almost no real code when measured on about 90,000 files (Python, Rust,
//! JavaScript and YAML, at 0.2% of files or fewer). A new TypeScript file is
//! left alone, as about 5% of real ones meet a gap in the grammar, and so is
//! YAML with a bare `{{` in it, a template that is YAML only once it is
//! filled in; a GitHub workflow's `${{` is YAML as it stands. Shell scripts
//! are not parsed at all: 8% of those bash runs fail the grammar. The note
//! asks for a fix unless the language's own tools accept the file, so a gap
//! is not "fixed" into wrong code. Code that parses costs a parse, about a
//! millisecond; the old text is parsed only when the new text does not, and
//! parsing that runs long is given up on, saying nothing.

use std::ops::ControlFlow;
use std::path::Path;
use std::time::{Duration, Instant};
use tree_sitter::{Language, Node, ParseOptions, ParseState, Parser, Tree};

/// The largest text parsed, a megabyte: source that size is nearly always
/// generated, and parsing it would cost more than the write.
const MOST_BYTES: usize = 1 << 20;
/// How long the parsing behind one note may take before it is given up on:
/// a megabyte of real code parses in about this, but a megabyte of noise
/// takes the grammars seconds, which a write should not wait for.
const MOST_TIME: Duration = Duration::from_millis(200);
/// Characters of the broken line quoted, enough to find it by.
const LINE_CHARS: usize = 120;
/// What is said of a Python statement that does not line up.
const ASKEW: &str = "indented unlike the lines around it";
/// The parts of a Python statement that line up under its first line.
const CLAUSES: [&str; 5] = [
    "elif_clause",
    "else_clause",
    "except_clause",
    "except_group_clause",
    "finally_clause",
];

/// The grammar for one kind of file.
struct Grammar {
    /// The language as the note names it.
    name: &'static str,
    language: Language,
    /// Whether a new file is held to it, not only one that parsed before.
    new_files: bool,
}

/// The first place a text does not parse.
struct Fault {
    /// Its row, from 0.
    row: usize,
    /// What is wrong there, when more can be said than that it is.
    why: Option<String>,
}

/// What to add to a write's result when `after` no longer parses as the
/// code `path` holds, where `before` is the old text or `None` for a new
/// file: nothing, or a note on a line of its own, as csv_note's are.
pub(crate) fn note(path: &Path, before: Option<&str>, after: &str) -> String {
    let Some(grammar) = grammar(path) else {
        return String::new();
    };
    // A bare `{{` opens a template, Helm's or Jinja's, that is YAML only
    // once it is filled in; GitHub's `${{` is YAML as it stands.
    let template = grammar.name == "YAML"
        && after
            .match_indices("{{")
            .any(|(at, _)| !after[..at].ends_with('$'));
    if after.len() > MOST_BYTES || template {
        return String::new();
    }
    let after = after.strip_prefix('\u{FEFF}').unwrap_or(after);
    let deadline = Instant::now() + MOST_TIME;
    let Some(fault) =
        tree(&grammar, after, deadline).and_then(|tree| fault(&grammar, &tree, after))
    else {
        return String::new();
    };
    // An empty file is as good as new, so held to what a new one is: it
    // parses cleanly, but proves nothing about the grammar's gaps.
    let parses = match before.filter(|old| !old.trim().is_empty()) {
        None if grammar.new_files => "does not parse",
        Some(old) if old.len() <= MOST_BYTES && clean(&grammar, old, deadline) => {
            "no longer parses"
        }
        _ => return String::new(),
    };
    // As given, like csv_note's, so two files of one name in a patch are
    // told apart.
    let shown = path.display();
    let why = fault.why.map(|why| format!(", {why}")).unwrap_or_default();
    let line = after.lines().nth(fault.row).unwrap_or_default().trim_end();
    let quoted = match line.trim() {
        "" => String::new(),
        _ => format!(": `{}`", crate::page::cut(line, LINE_CHARS)),
    };
    format!(
        "\nNote: `{shown}` line {} {parses} as {}{why}{quoted}. Fix it before moving \
         on, unless the language's own tools accept it.",
        fault.row + 1,
        grammar.name
    )
}

/// The grammar for `path`'s extension, if it has one here.
fn grammar(path: &Path) -> Option<Grammar> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let (name, language, new_files): (_, Language, _) = match extension.as_str() {
        "py" | "pyi" => ("Python", tree_sitter_python::LANGUAGE.into(), true),
        "rs" => ("Rust", tree_sitter_rust::LANGUAGE.into(), true),
        "js" | "mjs" | "cjs" | "jsx" => {
            ("JavaScript", tree_sitter_javascript::LANGUAGE.into(), true)
        }
        "ts" | "mts" | "cts" => (
            "TypeScript",
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            false,
        ),
        "tsx" => ("TSX", tree_sitter_typescript::LANGUAGE_TSX.into(), false),
        "yaml" | "yml" => ("YAML", tree_sitter_yaml::LANGUAGE.into(), true),
        _ => return None,
    };
    Some(Grammar {
        name,
        language,
        new_files,
    })
}

/// Whether `text`, less any byte order mark, parses with no fault at all
/// before `deadline`; one given up on proves nothing, so is not clean.
fn clean(grammar: &Grammar, text: &str, deadline: Instant) -> bool {
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    tree(grammar, text, deadline).is_some_and(|tree| fault(grammar, &tree, text).is_none())
}

/// `text` parsed in `grammar`, or `None` when the grammar will not load or
/// the parse is still going at `deadline`.
fn tree(grammar: &Grammar, text: &str, deadline: Instant) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(&grammar.language).ok()?;
    let mut late = |_: &ParseState| {
        if Instant::now() >= deadline {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = ParseOptions::new().progress_callback(&mut late);
    let bytes = text.as_bytes();
    parser.parse_with_options(
        &mut |at, _| bytes.get(at..).unwrap_or_default(),
        None,
        Some(options),
    )
}

/// The first place `text` does not parse in `tree`, its parse in `grammar`;
/// `None` when it parses.
fn fault(grammar: &Grammar, tree: &Tree, text: &str) -> Option<Fault> {
    let mut node = tree.root_node();
    if !node.has_error() {
        return match grammar.name {
            "Python" => indentation(node, text),
            _ => None,
        };
    }
    // Down through the first child holding an error, which reaches the first
    // in the text; in a loop, as code can nest deeper than a stack allows.
    while !node.is_error() && !node.is_missing() {
        let mut cursor = node.walk();
        node = node.children(&mut cursor).find(|child| child.has_error())?;
    }
    // A missing `)` reads well; a missing `identifier` is the grammar's word.
    let why = (node.is_missing() && !node.is_named()).then(|| format!("missing `{}`", node.kind()));
    Some(Fault {
        row: node.start_position().row,
        why,
    })
}

/// The first place Python's indentation is wrong in a tree that parsed, as
/// Python reads it: every statement of a block that starts a line starts at
/// one column, deeper than the block's own first line, a block holds at
/// least one, and an `else` lines up with its `if`. Columns are in bytes, a
/// tab counting one, which Python also requires to agree.
fn indentation(root: Node, text: &str) -> Option<Fault> {
    let rows: Vec<&str> = text.split('\n').collect();
    let indent = |row: usize| {
        let line = rows.get(row).copied().unwrap_or_default();
        line.len() - line.trim_start_matches([' ', '\t']).len()
    };
    // Only what starts its line counts: after a `;` or on the line of an
    // `if x:`, a statement can be anywhere.
    let starts_line =
        |node: &Node| indent(node.start_position().row) == node.start_position().column;
    // The tree is walked in no particular order, so the earliest row wins.
    let mut first: Option<Fault> = None;
    let mut found = |row: usize, why: &str| {
        if first.as_ref().is_none_or(|fault| row < fault.row) {
            first = Some(Fault {
                row,
                why: Some(why.to_owned()),
            });
        }
    };
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        let children: Vec<Node> = node
            .named_children(&mut cursor)
            .filter(|child| !child.is_extra())
            .collect();
        match node.kind() {
            "module" | "block" => {
                let parent = node.parent().map(|parent| parent.start_position().row);
                if let Some(row) = parent.filter(|_| children.is_empty()) {
                    found(row, "with nothing indented under it");
                }
                // The module's statements start at the margin, a block's
                // where its first does.
                let mut column = parent.is_none().then_some(0);
                for child in children.iter().filter(|child| starts_line(child)) {
                    let at = child.start_position().column;
                    let wanted = *column.get_or_insert(at);
                    if at != wanted || parent.is_some_and(|row| at <= indent(row)) {
                        found(child.start_position().row, ASKEW);
                        break;
                    }
                }
            }
            "if_statement" | "try_statement" | "for_statement" | "while_statement" => {
                let column = node.start_position().column;
                for child in &children {
                    if CLAUSES.contains(&child.kind())
                        && starts_line(child)
                        && child.start_position().column != column
                    {
                        found(child.start_position().row, ASKEW);
                    }
                }
            }
            _ => {}
        }
        stack.extend(children);
    }
    first
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TypeScript that tsc takes and the grammar does not: vitest's way of
    /// typing a mocked module.
    const GAP: &str = "const real = await importOriginal<typeof import(\"./mod.js\")>();\n";

    #[test]
    fn an_edit_to_a_ts_file_with_an_existing_grammar_gap_is_silent() {
        let path = Path::new("src/mock.test.ts");
        let (fine, broken) = ("const a: number = 1;\n", "const a: number = (1;\n");
        let said = note(path, Some(fine), broken);
        assert!(
            said.starts_with(
                "\nNote: `src/mock.test.ts` line 1 no longer parses as TypeScript, missing \
                 `)`: `const a: number = (1;`. Fix it before moving on"
            ),
            "{said}"
        );
        let before = format!("{GAP}{fine}");
        assert_eq!(note(path, Some(&before), &format!("{GAP}{broken}")), "");
    }

    #[test]
    fn a_new_ts_file_with_typeof_import_is_silent() {
        let typescript = grammar(Path::new("a.ts")).unwrap();
        let tree = tree(&typescript, GAP, Instant::now() + MOST_TIME).unwrap();
        assert!(fault(&typescript, &tree, GAP).is_some());
        assert_eq!(note(Path::new("a.ts"), None, GAP), "");
        assert_eq!(note(Path::new("a.ts"), Some("\n"), GAP), "");
        assert_eq!(note(Path::new("a.tsx"), None, "const a = (1;\n"), "");
        let said = note(Path::new("a.js"), None, "const a = (1;\n");
        assert!(said.contains("does not parse as JavaScript"), "{said}");
    }

    #[test]
    fn a_new_python_file_with_an_error_is_reported() {
        let said = note(Path::new("app.py"), None, "def f(x):\n    return x +\n");
        assert_eq!(
            said,
            "\nNote: `app.py` line 2 does not parse as Python: `    return x +`. Fix it \
             before moving on, unless the language's own tools accept it."
        );
        let said = note(Path::new("lib.rs"), None, "fn main() {\n    let x = 1\n}\n");
        assert!(said.contains("`lib.rs` line 2 does not parse as Rust, missing `;`"));
    }

    #[test]
    fn a_two_megabyte_file_is_skipped() {
        let line = "x = 1\n";
        let big = line.repeat(2 * MOST_BYTES / line.len()) + "def f(:\n";
        assert_eq!(note(Path::new("big.py"), None, &big), "");
        assert!(!note(Path::new("small.py"), None, "x = 1\ndef f(:\n").is_empty());
    }

    #[test]
    fn python_indentation_the_grammar_lets_through_is_found() {
        let cases = [
            ("x = 1\n    y = 2\n", 2, "indented unlike"),
            (
                "def f():\n    a = 1\nb = 2\n    return a\n",
                4,
                "indented unlike",
            ),
            ("def f(x):\n    return 1\n  y = 2\n", 3, "indented unlike"),
            (
                "def f(x):\n    if x:\n        a = 1\n      else:\n        b = 2\n",
                4,
                "indented unlike",
            ),
            (
                "class A:\n    def f(self):\n        pass\n      def g(self):\n        pass\n",
                4,
                "indented unlike",
            ),
            ("def f():\nreturn 1\n", 1, "with nothing indented under it"),
            (
                "def f(x):\n    if x:\n    # later\n    return 2\n",
                2,
                "with nothing indented",
            ),
        ];
        for (text, line, why) in cases {
            let said = note(Path::new("a.py"), None, text);
            let wanted = format!("\nNote: `a.py` line {line} does not parse as Python, {why}");
            assert!(said.starts_with(&wanted), "{text:?}: {said}");
        }
        // Statements after a `;` or on their header's line, clauses of a
        // comprehension, continued lines and comments go where they like.
        let valid = "if x: a = 1; b = 2\nelse:\n    pass\nmatch x:\n    case 1:\n        pass\n\
                     y = [a\n     for a in b\n     if a]\nif y:\n    z = 1 \\\n  + 2\n  # c\n    z = 3\n";
        assert_eq!(note(Path::new("a.py"), None, valid), "");
    }

    #[test]
    fn a_file_with_a_mark_and_crlf_or_a_template_or_no_grammar_is_silent() {
        let rust = "\u{FEFF}fn a() {\r\n    one();\r\n}\r\n";
        assert_eq!(note(Path::new("a.rs"), Some(rust), rust), "");
        let chart = "{{- if .Values.on }}\nkind: Service\n{{- end }}\n";
        assert_eq!(note(Path::new("templates/service.yaml"), None, chart), "");
        let said = note(Path::new("ci.yml"), None, "a: 1\n b: 2\n");
        assert!(said.contains("does not parse as YAML"), "{said}");
        assert_eq!(note(Path::new("run.sh"), None, "if then fi (\n"), "");
    }

    #[test]
    fn a_workflow_with_github_expressions_is_still_checked() {
        let path = Path::new(".github/workflows/ci.yml");
        let fine = "on: push\njobs:\n  test:\n    runs-on: ${{ matrix.os }}\n    steps:\n      \
                    - run: echo \"${{ github.sha }}\"\n";
        assert_eq!(note(path, None, fine), "");
        // A key indented under a plain value, as `a: 1\n b: 2` is; a lost
        // closing quote would not do, as a plain value may hold a `"`.
        let broken = fine.replace("\n    steps:", "\n     steps:");
        let said = note(path, Some(fine), &broken);
        assert!(
            said.starts_with("\nNote: `.github/workflows/ci.yml` line ")
                && said.contains("no longer parses as YAML"),
            "{said}"
        );
    }

    #[test]
    fn a_parse_still_going_at_its_deadline_proves_nothing() {
        let python = grammar(Path::new("a.py")).unwrap();
        let text = "x = 1\n".repeat(1000);
        assert!(tree(&python, &text, Instant::now() + MOST_TIME).is_some());
        assert!(tree(&python, &text, Instant::now()).is_none());
        // Old text given up on is not taken for clean, so nothing is said.
        assert!(clean(&python, &text, Instant::now() + MOST_TIME));
        assert!(!clean(&python, &text, Instant::now()));
    }
}
