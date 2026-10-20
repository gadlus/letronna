//! The build a TypeScript or JavaScript change has to pass beyond its tests.
//!
//! A test runner for TypeScript transpiles each file on its own and checks
//! no types, so a change can pass every test and still break the package's
//! own build: a type error, or a rule such as isolated declarations that only
//! the build enforces. On DeepSWE a vitest change passed its own tests and
//! failed the hidden check, which builds the package first. So a session's
//! own run nobody watches that changed such files, and has passed no build
//! since, is told once which script builds them.
//!
//! The script is read off the nearest package.json, from each changed file
//! up to the project folder, that has one: a type check where there is one,
//! which is quicker, else the build. It is run with the package manager the
//! project's lock file names, npm when none does.

use std::path::{Component, Path};

/// Extensions of the files a package's build compiles.
const SOURCES: &[&str] = &[
    "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "vue", "svelte",
];

/// Scripts that check a package, in the order one is chosen: a type check
/// before a whole build.
const SCRIPTS: &[&str] = &["typecheck", "type-check", "check-types", "build"];

/// Programs that build or type check when run themselves.
const BUILDERS: &[&str] = &[
    "tsc",
    "vue-tsc",
    "svelte-check",
    "rollup",
    "tsup",
    "webpack",
];

/// Programs that run a package's script, or a program, by name.
const RUNNERS: &[&str] = &[
    "npm", "pnpm", "yarn", "bun", "npx", "pnpx", "bunx", "turbo", "nx", "lerna",
];

/// Lock files and the package manager each names, the first found winning.
const MANAGERS: &[(&str, &str)] = &[
    ("pnpm-lock.yaml", "pnpm"),
    ("yarn.lock", "yarn"),
    ("bun.lock", "bun"),
    ("bun.lockb", "bun"),
];

/// Commands a notice names: a change seldom spans more packages, and more
/// say nothing new.
const NAMED: usize = 3;

/// Whether `path` is a file a TypeScript or JavaScript build compiles.
pub(crate) fn source(path: &str) -> bool {
    let extension = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    SOURCES.iter().any(|s| extension.eq_ignore_ascii_case(s))
}

/// Whether the command `line` builds or type checks: a builder run itself,
/// or a check script or builder run through a package manager.
pub(crate) fn builds(line: &str) -> bool {
    line.split(['&', ';', '|', '\n']).any(|part| {
        let words: Vec<&str> = part.split_whitespace().collect();
        let Some(first) = words.first() else {
            return false;
        };
        let program = first.rsplit('/').next().unwrap_or(first);
        if BUILDERS.contains(&program) {
            return true;
        }
        RUNNERS.contains(&program)
            && words[1..]
                .iter()
                .any(|word| SCRIPTS.contains(word) || BUILDERS.contains(word))
    })
}

/// The commands that build the packages holding `files`, relative to the
/// project folder `workspace`, each once and in the order first met.
pub(crate) fn commands(workspace: &Path, files: &[String]) -> Vec<String> {
    let manager = manager(workspace);
    let mut found: Vec<String> = Vec::new();
    for file in files.iter().filter(|file| source(file)) {
        let file = Path::new(file);
        let inside = file.strip_prefix(workspace).unwrap_or(file);
        // A path that climbs out with `..`, or names another place, has no
        // package here to build.
        let plain = inside
            .components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir));
        if !plain {
            continue;
        }
        for folder in inside.ancestors().skip(1) {
            let Some(script) = script(&workspace.join(folder)) else {
                continue;
            };
            let folder = folder.to_string_lossy();
            let command = if folder.is_empty() {
                format!("{manager} run {script}")
            } else {
                format!("cd {folder} && {manager} run {script}")
            };
            if !found.contains(&command) {
                found.push(command);
            }
            break;
        }
    }
    found
}

/// What a run whose change `commands` build is told.
pub(crate) fn notice(commands: &[String]) -> String {
    let mut named: Vec<String> = commands
        .iter()
        .take(NAMED)
        .map(|c| format!("`{c}`"))
        .collect();
    if commands.len() > NAMED {
        named.push(format!("and {} more", commands.len() - NAMED));
    }
    format!(
        "[{} check] Before ending: no build has passed since you last changed this TypeScript or \
         JavaScript, and tests alone can pass while the package's build fails on a type error or \
         a rule only the build enforces. Run {}, fix what fails, then give your whole answer \
         again, not only what changed.",
        crate::BRAND,
        named.join(", ")
    )
}

/// The check script of the package.json in `folder`, if it has one.
fn script(folder: &Path) -> Option<&'static str> {
    let text = std::fs::read_to_string(folder.join("package.json")).ok()?;
    let manifest: serde_json::Value = serde_json::from_str(&text).ok()?;
    let scripts = manifest.get("scripts")?.as_object()?;
    SCRIPTS
        .iter()
        .find(|name| {
            scripts
                .get(**name)
                .is_some_and(serde_json::Value::is_string)
        })
        .copied()
}

/// The package manager the project folder's lock file names, npm when none.
fn manager(workspace: &Path) -> &'static str {
    MANAGERS
        .iter()
        .find(|(lock, _)| workspace.join(lock).exists())
        .map_or("npm", |(_, manager)| manager)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder under the system's temp folder named for `name`.
    fn folder(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("letronna-builds-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn manifest(dir: &Path, scripts: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let text = format!("{{\"name\": \"x\", \"scripts\": {{{scripts}}}}}");
        std::fs::write(dir.join("package.json"), text).unwrap();
    }

    #[test]
    fn typescript_and_javascript_files_are_sources_and_others_are_not() {
        for path in ["src/a.ts", "b.tsx", "c.mjs", "d.vue", "E.TS"] {
            assert!(source(path), "{path}");
        }
        for path in ["a.py", "b.rs", "README.md", "package.json", "c.d"] {
            assert!(!source(path), "{path}");
        }
    }

    #[test]
    fn a_build_or_type_check_counts_and_a_folder_named_build_does_not() {
        for line in [
            "pnpm run build",
            "pnpm build",
            "cd packages/vitest && pnpm run build",
            "npm run typecheck",
            "pnpm --filter vitest build",
            "npx tsc --noEmit",
            "tsc -p .",
            "rollup -c",
            "./node_modules/.bin/tsc",
        ] {
            assert!(builds(line), "{line}");
        }
        for line in [
            "mkdir build",
            "ls build",
            "pnpm vitest run test/a.test.ts",
            "npm test",
            "cat package.json",
        ] {
            assert!(!builds(line), "{line}");
        }
    }

    #[test]
    fn the_nearest_package_with_a_check_script_builds_and_a_type_check_comes_first() {
        let dir = folder("nearest");
        std::fs::write(dir.join("pnpm-lock.yaml"), "").unwrap();
        manifest(&dir, "\"build\": \"pnpm -r build\"");
        manifest(
            &dir.join("packages/app"),
            "\"build\": \"rollup -c\", \"typecheck\": \"tsc\"",
        );
        // No check script of its own: the root's builds it.
        manifest(&dir.join("packages/docs"), "\"dev\": \"vite\"");
        let files = [
            "packages/app/src/a.ts".to_owned(),
            dir.join("packages/app/src/b.ts")
                .to_string_lossy()
                .into_owned(),
            "packages/docs/c.ts".to_owned(),
            "notes.md".to_owned(),
        ];
        assert_eq!(
            commands(&dir, &files),
            ["cd packages/app && pnpm run typecheck", "pnpm run build"]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn npm_builds_where_no_lock_file_says_otherwise_and_no_package_means_no_command() {
        let dir = folder("npm");
        assert!(commands(&dir, &["src/a.ts".to_owned()]).is_empty());
        manifest(&dir, "\"build\": \"tsc\"");
        assert_eq!(commands(&dir, &["src/a.ts".to_owned()]), ["npm run build"]);
        assert!(commands(&dir, &["../elsewhere/a.ts".to_owned()]).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_notice_names_at_most_three_commands() {
        let all: Vec<String> = (1..=5)
            .map(|n| format!("cd p{n} && npm run build"))
            .collect();
        let text = notice(&all);
        assert!(text.starts_with("[Letronna check] Before ending: no build has passed"));
        assert!(
            text.contains("`cd p3 && npm run build`, and 2 more"),
            "{text}"
        );
        assert!(!text.contains("p4"));
    }
}
