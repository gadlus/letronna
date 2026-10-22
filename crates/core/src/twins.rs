//! Classes a run made that share a name with one the project already has.
//!
//! A request that names a class means the one the project has. On DeepSWE a
//! kombu run was asked for `Consumer.__init__` to take an `on_cancel`, wrote
//! a new `class Consumer` in the virtual transport beside the real one in
//! kombu/messaging.py, and every hidden test of it found the real class
//! without the new methods. The run had been told in so many words to extend
//! a class the project already has, and had even seen the real one. So a
//! session's own run nobody watches that added such a class is told once
//! where the other one is.
//!
//! Only classes, structs, enums and traits count, in languages where one name
//! is defined once: a Ruby class may be reopened anywhere, and a function of
//! one name in two modules is common. Tests are left out, both as where a new
//! one was made and as where the other lives, since a test may stand in for
//! a real class.

use std::collections::BTreeSet;
use std::path::{Component, Path};

use regex::Regex;

/// Twins named in one notice; any more go without saying.
const NAMED: usize = 3;

/// A class the run made and one of that name the project already had.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Twin {
    pub name: String,
    /// Where the run made it, in the project.
    pub made: String,
    /// Where the project already has one, in the project, at most `NAMED`.
    pub had: Vec<String>,
}

/// A language's definitions: the extensions its files take, a pattern whose
/// first group is a defined name, and the same as an extended regular
/// expression for git with the name in place of `{}`.
struct Language {
    extensions: &'static [&'static str],
    pattern: &'static str,
    posix: &'static str,
}

const LANGUAGES: &[Language] = &[
    Language {
        extensions: &["py", "pyi"],
        pattern: r"^\s*class\s+([A-Za-z_]\w*)",
        posix: r"^[[:space:]]*class[[:space:]]+{}([^A-Za-z0-9_]|$)",
    },
    Language {
        extensions: &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"],
        pattern: r"^\s*(?:export\s+)?(?:default\s+)?(?:abstract\s+)?class\s+([A-Za-z_$][\w$]*)",
        posix: r"^[[:space:]]*(export[[:space:]]+)?(default[[:space:]]+)?(abstract[[:space:]]+)?class[[:space:]]+{}([^A-Za-z0-9_$]|$)",
    },
    Language {
        extensions: &["rs"],
        pattern: r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum|trait)\s+([A-Za-z_]\w*)",
        posix: r"^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?(struct|enum|trait)[[:space:]]+{}([^A-Za-z0-9_]|$)",
    },
    Language {
        extensions: &["go"],
        pattern: r"^type\s+([A-Za-z_]\w*)\s+(?:struct|interface)\b",
        posix: r"^type[[:space:]]+{}[[:space:]]+(struct|interface)([^A-Za-z0-9_]|$)",
    },
];

/// The classes the run made in `files`, which it wrote or edited as it named
/// them, that the request names and that the project's checked-out commit
/// already defines in another file of the same language.
pub(crate) fn twins(request: &str, workspace: &Path, files: &[String]) -> Vec<Twin> {
    let mut found = Vec::new();
    for file in files {
        let Some(path) = inside(workspace, file) else {
            continue;
        };
        let Some(language) = language(&path) else {
            continue;
        };
        if test(&path) {
            continue;
        }
        let Ok(now) = std::fs::read_to_string(workspace.join(&path)) else {
            continue;
        };
        let before = crate::git::at_head(workspace, &path).unwrap_or_default();
        let had = defined(language, &before);
        for name in defined(language, &now) {
            if had.contains(&name) || !named(request, &name) {
                continue;
            }
            if found.iter().any(|twin: &Twin| twin.name == name) {
                continue;
            }
            let pattern = language.posix.replace("{}", &regex::escape(&name));
            let had: Vec<String> = crate::git::matching_at_head(workspace, &pattern)
                .into_iter()
                .filter(|other| *other != path && same(language, other))
                .filter(|other| !test(other) && !vendored(other))
                .take(NAMED)
                .collect();
            if !had.is_empty() {
                found.push(Twin {
                    name,
                    made: path.clone(),
                    had,
                });
            }
        }
    }
    found
}

/// What the run is told of `twins`, at most `NAMED` of them.
pub(crate) fn notice(twins: &[Twin]) -> String {
    let mut named: Vec<String> = (twins.iter().take(NAMED))
        .map(|twin| {
            let had = twin.had.join(" and ");
            format!(
                "`{}` in {} (the project's is in {had})",
                twin.name, twin.made
            )
        })
        .collect();
    if twins.len() > NAMED {
        named.push(format!("and {} more", twins.len() - NAMED));
    }
    format!(
        "[{} check] Before ending: you made a new class that shares its name with one the project \
         already has: {}. Where the request names it, it means the project's own, which callers \
         and tests use: add what the request asks for to that one, and remove yours unless the \
         request asks for a second class of that name. Then give your whole answer again, not \
         only what changed.",
        crate::BRAND,
        named.join(", ")
    )
}

/// `file` as a path in the project folder `workspace`, with `/` between its
/// parts, or `None` when it lies outside.
fn inside(workspace: &Path, file: &str) -> Option<String> {
    let path = Path::new(file);
    let path = match path.strip_prefix(workspace) {
        Ok(rest) => rest,
        Err(_) if path.is_absolute() => return None,
        Err(_) => path,
    };
    let mut parts = Vec::new();
    for part in path.components() {
        match part {
            Component::Normal(name) => parts.push(name.to_str()?),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// The language of the file at `path`, of those whose definitions count.
fn language(path: &str) -> Option<&'static Language> {
    let extension = Path::new(path).extension()?.to_str()?;
    LANGUAGES.iter().find(|language| {
        (language.extensions.iter()).any(|known| known.eq_ignore_ascii_case(extension))
    })
}

/// Whether the file at `path` is of `language`.
fn same(language: &Language, path: &str) -> bool {
    self::language(path).is_some_and(|other| std::ptr::eq(other, language))
}

/// Whether the file at `path` is a test, a fixture or an example by its
/// folder or name.
fn test(path: &str) -> bool {
    const FOLDERS: &[&str] = &[
        "test",
        "tests",
        "__tests__",
        "spec",
        "specs",
        "testing",
        "fixtures",
        "examples",
        "testdata",
    ];
    let mut parts: Vec<&str> = path.split('/').collect();
    let name = parts.pop().unwrap_or_default().to_ascii_lowercase();
    let folders = parts
        .iter()
        .any(|part| FOLDERS.contains(&part.to_ascii_lowercase().as_str()));
    folders
        || name.starts_with("test_")
        || name == "conftest.py"
        || [".test.", ".spec.", "_test.", "_spec."]
            .iter()
            .any(|mark| name.contains(mark))
}

/// Whether the file at `path` is another project's code kept in this one.
fn vendored(path: &str) -> bool {
    const FOLDERS: &[&str] = &[
        "vendor",
        "third_party",
        "third-party",
        "node_modules",
        "external",
    ];
    let mut parts: Vec<&str> = path.split('/').collect();
    parts.pop();
    parts.iter().any(|part| FOLDERS.contains(part))
}

/// The names `language` defines in `text`.
fn defined(language: &Language, text: &str) -> BTreeSet<String> {
    let Ok(pattern) = Regex::new(language.pattern) else {
        return BTreeSet::new();
    };
    text.lines()
        .filter_map(|line| pattern.captures(line))
        .filter_map(|found| found.get(1))
        .map(|name| name.as_str().to_owned())
        .collect()
}

/// Whether `request` uses `name` as a word of its own, as `Consumer` is in
/// "`Consumer.__init__` accepts".
fn named(request: &str, name: &str) -> bool {
    if name.len() < 3 {
        return false;
    }
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    request.match_indices(name).any(|(at, _)| {
        let before = request[..at].chars().next_back();
        let after = request[at + name.len()..].chars().next();
        !before.is_some_and(word) && !after.is_some_and(word)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh git repository under the system's temp folder named for
    /// `name`, holding `files` in its first commit.
    fn repository(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("letronna-twins-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (path, text) in files {
            write(&dir, path, text);
        }
        for args in [
            &["init", "-q"][..],
            &["add", "-A"],
            &[
                "-c",
                "user.name=user_a",
                "-c",
                "user.email=user_a@example.com",
                "commit",
                "-q",
                "-m",
                "first",
            ],
        ] {
            let ok = std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        }
        dir
    }

    fn write(dir: &Path, path: &str, text: &str) {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }

    const REQUEST: &str = "`Consumer.__init__` accepts `on_cancel=None`. \
                           `Channel.basic_consume` supports `x-priority`.";

    #[test]
    fn a_class_the_request_names_made_beside_the_projects_own_is_found() {
        let dir = repository(
            "found",
            &[
                (
                    "kombu/messaging.py",
                    "class Producer:\n    pass\n\nclass Consumer:\n    pass\n",
                ),
                (
                    "kombu/compat.py",
                    "class Consumer(messaging.Consumer):\n    pass\n",
                ),
                ("t/unit/test_compat.py", "class Consumer:\n    pass\n"),
                (
                    "kombu/transport/virtual/base.py",
                    "class Channel:\n    pass\n",
                ),
            ],
        );
        write(
            &dir,
            "kombu/transport/virtual/base.py",
            "class Channel:\n    pass\n\nclass Consumer:\n    def on_cancel_notify(self):\n        pass\n",
        );
        let files = [dir
            .join("kombu/transport/virtual/base.py")
            .to_string_lossy()
            .into_owned()];
        assert_eq!(
            twins(REQUEST, &dir, &files),
            [Twin {
                name: "Consumer".into(),
                made: "kombu/transport/virtual/base.py".into(),
                had: vec!["kombu/compat.py".into(), "kombu/messaging.py".into()],
            }]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_class_the_file_had_one_the_request_does_not_name_or_a_test_double_is_not() {
        let dir = repository(
            "not",
            &[
                (
                    "kombu/messaging.py",
                    "class Consumer:\n    pass\n\nclass Queue:\n    pass\n",
                ),
                (
                    "kombu/transport/virtual/base.py",
                    "class Channel:\n    pass\n",
                ),
                ("kombu/entity.py", "class Queue:\n    pass\n"),
            ],
        );
        // Channel was there before; Helper and Queue are not named; the
        // test's Consumer stands in for the real one.
        write(
            &dir,
            "kombu/transport/virtual/base.py",
            "class Channel:\n    x = 1\n\nclass Helper:\n    pass\n\nclass Queue:\n    pass\n",
        );
        write(&dir, "t/unit/test_sac.py", "class Consumer:\n    pass\n");
        let files = [
            "kombu/transport/virtual/base.py".to_owned(),
            "t/unit/test_sac.py".to_owned(),
            "../elsewhere/base.py".to_owned(),
        ];
        assert!(twins(REQUEST, &dir, &files).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn only_a_class_of_the_same_language_outside_tests_and_vendored_code_is_the_projects_own() {
        let dir = repository(
            "language",
            &[
                ("web/consumer.ts", "export class Consumer {}\n"),
                ("tests/fakes.py", "class Consumer:\n    pass\n"),
                ("vendor/lib/consumer.py", "class Consumer:\n    pass\n"),
            ],
        );
        write(&dir, "kombu/consumer.py", "class Consumer:\n    pass\n");
        assert!(twins(REQUEST, &dir, &["kombu/consumer.py".to_owned()]).is_empty());
        write(&dir, "web/other.ts", "export default class Consumer {}\n");
        assert_eq!(
            twins(REQUEST, &dir, &["web/other.ts".to_owned()]),
            [Twin {
                name: "Consumer".into(),
                made: "web/other.ts".into(),
                had: vec!["web/consumer.ts".into()],
            }]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_name_counts_only_as_a_word_of_its_own() {
        assert!(named("`Consumer.__init__` accepts", "Consumer"));
        assert!(named("the Consumer class", "Consumer"));
        assert!(!named("a ConsumerGroup holds", "Consumer"));
        assert!(!named("the BaseConsumer", "Consumer"));
        assert!(!named("an Id", "Id"));
    }

    #[test]
    fn the_notice_names_at_most_three_and_where_the_projects_own_is() {
        let all: Vec<Twin> = (1..=5)
            .map(|n| Twin {
                name: format!("C{n}"),
                made: format!("new{n}.py"),
                had: vec![format!("old{n}.py"), format!("older{n}.py")],
            })
            .collect();
        let text = notice(&all);
        assert!(text.starts_with("[Letronna check] Before ending: you made a new class"));
        assert!(
            text.contains("`C1` in new1.py (the project's is in old1.py and older1.py)"),
            "{text}"
        );
        assert!(text.contains("and 2 more"), "{text}");
        assert!(!text.contains("C4"));
    }
}
