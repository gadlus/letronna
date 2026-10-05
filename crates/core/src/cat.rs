//! A `cat` of files, counted as a read of them, so `write_file` and
//! `edit_file` do not send the model to read_file for bytes it just printed.
//!
//! Only a command that is nothing but `cat`, or `cat -n`, of paths spelled
//! out counts, after a `cd` into a folder spelled out and `&&` or not. A
//! pipe, a redirect, a glob, a variable, a substitution, a `;` or any other
//! word leaves the line to the shell and the guard as it was.
//!
//! What the model was shown is checked against the files, not taken from the
//! command line: the result has to be exactly what `cat` prints of their
//! bytes as they are now. A result of a command that failed, that was cut,
//! that hid a key or lost an escape sequence, or that has the sandbox's note
//! or a job's header in front is not that, and counts for nothing. As for
//! read_file, the files must not have moved since before the command ran.
//!
//! Not on Windows, where `cat` and `type` are PowerShell's Get-Content. In
//! 5.1 that reads a file with no byte order mark in the ANSI code page, so
//! the model would be shown a text that is not the file, and it ends every
//! line, the last too, with a line break, so the result is no copy of the
//! bytes to check against.

use crate::filestate::{Lines, Stat};
use crate::tools::MAX_OUTPUT;
use std::path::{Component, Path, PathBuf};

/// `cat -n` right-aligns a line's number in this many columns, as GNU and
/// BSD cat both do.
const NUMBER_WIDTH: usize = 6;

/// A plain `cat` and the files it prints, as they stood before it ran.
pub(crate) struct Cat {
    files: Vec<(PathBuf, Stat)>,
    numbered: bool,
}

impl Cat {
    /// The plain `cat` that `command` is when run in `dir`; None for any
    /// other command.
    pub(crate) fn of(command: &str, dir: &Path) -> Option<Cat> {
        if cfg!(windows) {
            return None;
        }
        let plain = plain(command)?;
        let dir = match &plain.folder {
            Some(folder) => dir.join(folder),
            None => dir.to_path_buf(),
        };
        let files = plain
            .files
            .iter()
            .map(|name| {
                let path = dir.join(name);
                let before = crate::filestate::stat(&path);
                (path, before)
            })
            .collect();
        Some(Cat {
            files,
            numbered: plain.numbered,
        })
    }

    /// Records the files as read in `conversation` when `shown`, what the
    /// command returned, is exactly what `cat` prints of them as they are now.
    pub(crate) fn read(self, conversation: u64, shown: &str) {
        // On its way to the model a longer result is cut and the characters
        // nobody sees are taken out; keys were hidden as it was read, so
        // none is in a result that equals the files.
        if shown.len() > MAX_OUTPUT || crate::untrusted::visible(shown) != shown {
            return;
        }
        let Some(texts) = self.texts(shown.len()) else {
            return;
        };
        if self.printed(&texts).as_deref() != Some(shown) {
            return;
        }
        for ((path, before), text) in self.files.iter().zip(&texts) {
            let lines = Lines {
                first: 1,
                last: text.split_inclusive('\n').count(),
                end: true,
            };
            crate::filestate::read(conversation, path, *before, Some(lines));
        }
    }

    /// What each file holds, when all of it fits in a result `room` bytes
    /// long and is text.
    fn texts(&self, room: usize) -> Option<Vec<String>> {
        self.files
            .iter()
            .map(|(path, before)| {
                let (len, _) = (*before)?;
                // Only a regular file is read: opening a pipe would wait for
                // a writer.
                if len > room as u64 || !std::fs::metadata(path).ok()?.is_file() {
                    return None;
                }
                String::from_utf8(std::fs::read(path).ok()?).ok()
            })
            .collect()
    }

    /// What `cat` prints of files that hold `texts`; None where cats differ.
    fn printed(&self, texts: &[String]) -> Option<String> {
        let all = texts.concat();
        if !self.numbered {
            return Some(all);
        }
        // GNU cat carries a line a file leaves open on into the first of the
        // next, which BSD cat numbers as a line of its own.
        let (_, earlier) = texts.split_last()?;
        if earlier.iter().any(|t| !t.is_empty() && !t.ends_with('\n')) {
            return None;
        }
        Some(
            all.split_inclusive('\n')
                .enumerate()
                .map(|(at, line)| format!("{:>NUMBER_WIDTH$}\t{line}", at + 1))
                .collect(),
        )
    }
}

/// What a plain `cat` line says.
struct Plain {
    /// The folder it moves to first.
    folder: Option<String>,
    numbered: bool,
    files: Vec<String>,
}

/// One word of a line, or the `&&` between two commands.
enum Token {
    Word(String),
    And,
}

/// What `line` says when it is `cat` or `cat -n` of one or more files, after
/// `cd folder &&` or not, and nothing more.
fn plain(line: &str) -> Option<Plain> {
    let tokens = tokens(line)?;
    let mut rest = tokens.as_slice();
    let mut folder = None;
    if let [Token::Word(cd), Token::Word(to), Token::And, tail @ ..] = rest
        && cd == "cd"
    {
        // `cd -` and its options are not folders, and the shell follows `..`
        // through links as a path joined here would not.
        let up = Path::new(to)
            .components()
            .any(|c| c == Component::ParentDir);
        if to.is_empty() || to.starts_with('-') || up {
            return None;
        }
        folder = Some(to.clone());
        rest = tail;
    }
    let [Token::Word(cat), args @ ..] = rest else {
        return None;
    };
    if cat != "cat" {
        return None;
    }
    let (numbered, args) = match args {
        [Token::Word(flag), files @ ..] if flag == "-n" => (true, files),
        files => (false, files),
    };
    let files: Vec<String> = args
        .iter()
        .map(|token| match token {
            Token::Word(name) if !name.is_empty() && !name.starts_with('-') => Some(name.clone()),
            _ => None,
        })
        .collect::<Option<_>>()?;
    if files.is_empty() {
        return None;
    }
    Some(Plain {
        folder,
        numbered,
        files,
    })
}

/// The words of `line` as the shell hands them on, and its `&&`s; None when
/// the shell would do more with it than that: expand, substitute, redirect,
/// pipe, run in the background or end a command anywhere else.
fn tokens(line: &str) -> Option<Vec<Token>> {
    let mut chars = line.trim().chars().peekable();
    let mut tokens = Vec::new();
    loop {
        while chars.next_if(|c| blank(*c)).is_some() {}
        let Some(&first) = chars.peek() else {
            return Some(tokens);
        };
        let mut word = String::new();
        if matches!(first, '\'' | '"') {
            chars.next();
            loop {
                let c = chars.next()?;
                if c == first {
                    break;
                }
                if !quoted(c, first) {
                    return None;
                }
                word.push(c);
            }
            // Text against the closing quote would make one word of two
            // pieces, which this does not put together.
            if chars.peek().is_some_and(|c| !blank(*c)) {
                return None;
            }
            tokens.push(Token::Word(word));
            continue;
        }
        while let Some(c) = chars.next_if(|c| !blank(*c)) {
            word.push(c);
        }
        if word == "&&" {
            tokens.push(Token::And);
        } else if word.chars().all(bare) {
            tokens.push(Token::Word(word));
        } else {
            return None;
        }
    }
}

fn blank(c: char) -> bool {
    matches!(c, ' ' | '\t')
}

/// Whether the shell hands `c` on as it is outside quotes: not a glob, a
/// brace, a tilde, a variable, a redirect, a comment or the end of a command.
fn bare(c: char) -> bool {
    c.is_alphanumeric() || "_./+,:@%=-".contains(c)
}

/// Whether the shell hands `c` on as it is inside the quotes `quote`: single
/// quotes keep everything, double quotes not `$`, a backtick, `\` or `!`.
fn quoted(c: char, quote: char) -> bool {
    !c.is_control() && (quote == '\'' || !matches!(c, '$' | '`' | '\\' | '!'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(line: &str) -> Option<(Option<String>, bool, Vec<String>)> {
        plain(line).map(|p| (p.folder, p.numbered, p.files))
    }

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn a_cat_of_paths_spelled_out_is_plain() {
        assert_eq!(
            files("cat in/app/config_manager.py"),
            Some((None, false, strings(&["in/app/config_manager.py"])))
        );
        assert_eq!(
            files("  cat\t-n  a.txt   'my notes.txt' \"b c.md\"\n"),
            Some((None, true, strings(&["a.txt", "my notes.txt", "b c.md"])))
        );
        assert_eq!(
            files("cd in/app && cat config_manager.py"),
            Some((
                Some("in/app".into()),
                false,
                strings(&["config_manager.py"])
            ))
        );
        assert_eq!(
            files("cd 'a b' && cat -n x.py y.py"),
            Some((Some("a b".into()), true, strings(&["x.py", "y.py"])))
        );
    }

    #[test]
    fn anything_the_shell_would_do_more_with_is_not() {
        for line in [
            "cat a.txt | head",
            "cat a.txt > b.txt",
            "cat a.txt >> b.txt",
            "cat < a.txt",
            "cat a.txt 2>&1",
            "cat *.py",
            "cat a?.py",
            "cat [ab].py",
            "cat {a,b}.py",
            "cat ~/a.txt",
            "cat $HOME/a.txt",
            "cat \"$HOME/a.txt\"",
            "cat $(pwd)/a.txt",
            "cat `pwd`/a.txt",
            "cat a.txt; rm a.txt",
            "cat a.txt && rm a.txt",
            "cat a.txt || true",
            "cat a.txt &",
            "cat a.txt # why",
            "cat a.txt\nrm a.txt",
            "cat 'a.txt",
            "cat a\\ b.txt",
            "cat \"a\\\"b\"",
            "cat 'a'b.txt",
            "cat -A a.txt",
            "cat -n -n a.txt",
            "cat -- a.txt",
            "cat -",
            "cat",
            "cat -n",
            "cat ''",
            "cd a; cat b.txt",
            "cd a & cat b.txt",
            "cd a || cat b.txt",
            "cd a&&cat b.txt",
            "cd && cat b.txt",
            "cd - && cat b.txt",
            "cd .. && cat b.txt",
            "cd a/../b && cat b.txt",
            "cd $HOME && cat b.txt",
            "cd a && cd b && cat c.txt",
            "cd a && cat b.txt && cat c.txt",
            "cd a",
            "'&&' cat a.txt",
            "cd a '&&' cat b.txt",
            "/bin/cat a.txt",
            "command cat a.txt",
            "FOO=1 cat a.txt",
            "type a.txt",
            "Get-Content a.txt",
            "head a.txt",
            "",
        ] {
            assert!(files(line).is_none(), "{line}");
        }
    }

    #[test]
    fn cat_n_numbers_lines_across_files_as_cat_does() {
        let cat = Cat {
            files: Vec::new(),
            numbered: true,
        };
        let texts = strings(&["a\nb\n", "", "c"]);
        assert_eq!(
            cat.printed(&texts).as_deref(),
            Some("     1\ta\n     2\tb\n     3\tc")
        );
        // GNU and BSD cat number a line a file leaves open differently.
        assert_eq!(cat.printed(&strings(&["a", "b\n"])), None);
        let cat = Cat {
            numbered: false,
            ..cat
        };
        assert_eq!(cat.printed(&texts).as_deref(), Some("a\nb\nc"));
    }

    #[test]
    fn no_cat_counts_on_windows() {
        let cat = Cat::of("cat a.txt", Path::new("."));
        assert_eq!(cat.is_none(), cfg!(windows));
    }
}
