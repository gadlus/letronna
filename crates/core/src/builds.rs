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
//!
//! A build passed counts only for the files of the package it built: one run
//! in that package's folder or naming it, or one that builds every package at
//! once. On DeepSWE a vitest run type checked the whole repository with its
//! root config, which leaves out the rule the vitest package's own build
//! enforces, and that check stood in for the build the change needed. A file
//! since deleted, such as a script made for a quick check, has nothing left
//! to build.

use std::path::{Component, Path, PathBuf};

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

/// Flags and programs that build every package of a workspace at once.
const EVERY: &[&str] = &[
    "-r",
    "--recursive",
    "--workspaces",
    "-ws",
    "turbo",
    "nx",
    "lerna",
];

/// What parts a command line into the commands it runs one after another.
const SEPARATORS: [char; 4] = ['&', ';', '|', '\n'];

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
    line.split(SEPARATORS).any(building)
}

/// Whether one command of a line, `part`, builds or type checks.
fn building(part: &str) -> bool {
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
}

/// Whether the build `line`, run in the project folder `workspace`, built
/// `file`: it ran in the folder of the package holding the file or named
/// that package, or it built every package at once. A file no package with a
/// check script holds is built by any build.
pub(crate) fn built(line: &str, workspace: &Path, file: &str) -> bool {
    let Some(package) = inside(workspace, file).and_then(|file| package(workspace, file)) else {
        return true;
    };
    let name = manifest(&workspace.join(&package))
        .and_then(|manifest| manifest.get("name")?.as_str().map(str::to_owned));
    // Where the command stands, `None` once it has left the project folder.
    let mut here = Some(PathBuf::new());
    for part in line.split(SEPARATORS) {
        let words: Vec<&str> = part.split_whitespace().collect();
        if words.first() == Some(&"cd") {
            here = here.and_then(|here| resolve(workspace, &here, words.get(1).copied()?));
            continue;
        }
        if !building(part) {
            continue;
        }
        let Some(here) = &here else {
            if every(&words, None) {
                return true;
            }
            continue;
        };
        if here.starts_with(&package) || every(&words, Some(&workspace.join(here))) {
            return true;
        }
        let named = words[1..]
            .iter()
            .flat_map(|word| word.split('='))
            .map(|word| word.trim_matches(['"', '\'']))
            .any(|word| {
                name.as_deref() == Some(word)
                    || resolve(workspace, here, word).is_some_and(|path| path.starts_with(&package))
            });
        if named {
            return true;
        }
    }
    false
}

/// The commands that build the packages holding `files`, relative to the
/// project folder `workspace`, each once and in the order first met.
pub(crate) fn commands(workspace: &Path, files: &[String]) -> Vec<String> {
    let manager = manager(workspace);
    let mut found: Vec<String> = Vec::new();
    for file in files.iter().filter(|file| source(file)) {
        let Some(file) = inside(workspace, file) else {
            continue;
        };
        // Deleted since, as a script made for a quick check is: nothing to build.
        if !workspace.join(file).exists() {
            continue;
        }
        let Some(folder) = package(workspace, file) else {
            continue;
        };
        let Some(script) = script(&workspace.join(&folder)) else {
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
    }
    found
}

/// `file` relative to the project folder `workspace`, or `None` when it
/// climbs out with `..` or names another place, which has no package here.
fn inside<'a>(workspace: &Path, file: &'a str) -> Option<&'a Path> {
    let file = Path::new(file);
    let inside = file.strip_prefix(workspace).unwrap_or(file);
    inside
        .components()
        .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
        .then_some(inside)
}

/// The folder, relative to the project folder `workspace`, of the nearest
/// package from `file` up whose package.json has a check script.
fn package(workspace: &Path, file: &Path) -> Option<PathBuf> {
    file.ancestors()
        .skip(1)
        .find(|folder| script(&workspace.join(folder)).is_some())
        .map(Path::to_path_buf)
}

/// Whether the build command `words`, run in `folder`, builds every package
/// at once: a recursive run, a monorepo tool, or a script of the folder's own
/// package.json that is one.
fn every(words: &[&str], folder: Option<&Path>) -> bool {
    let wide = |word: &str| EVERY.contains(&word.rsplit('/').next().unwrap_or(word));
    if words.iter().any(|word| wide(word)) {
        return true;
    }
    let Some(manifest) = folder.and_then(manifest) else {
        return false;
    };
    let Some(scripts) = manifest
        .get("scripts")
        .and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    words
        .iter()
        .filter_map(|word| scripts.get(*word)?.as_str())
        .any(|script| script.split_whitespace().any(wide))
}

/// The path `to`, named from `here` inside the project folder `workspace`,
/// relative to the project folder, or `None` when it lies outside it.
fn resolve(workspace: &Path, here: &Path, to: &str) -> Option<PathBuf> {
    let to = Path::new(to);
    let joined = match to.strip_prefix(workspace) {
        Ok(inside) => inside.to_path_buf(),
        Err(_) if to.has_root() => return None,
        Err(_) => here.join(to),
    };
    let mut path = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::Normal(name) => path.push(name),
            Component::CurDir => {}
            Component::ParentDir if path.pop() => {}
            _ => return None,
        }
    }
    Some(path)
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
    let manifest = manifest(folder)?;
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

/// The package.json in `folder`, read.
fn manifest(folder: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(folder.join("package.json")).ok()?;
    serde_json::from_str(&text).ok()
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
        named(dir, "x", scripts);
    }

    fn named(dir: &Path, name: &str, scripts: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let text = format!("{{\"name\": \"{name}\", \"scripts\": {{{scripts}}}}}");
        std::fs::write(dir.join("package.json"), text).unwrap();
    }

    /// Empty files at `paths` under `dir`.
    fn touch(dir: &Path, paths: &[&str]) {
        for path in paths {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
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
        touch(
            &dir,
            &[
                "packages/app/src/a.ts",
                "packages/app/src/b.ts",
                "packages/docs/c.ts",
            ],
        );
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
    fn a_file_since_deleted_names_no_build() {
        let dir = folder("deleted");
        manifest(&dir, "\"typecheck\": \"tsc\"");
        manifest(&dir.join("packages/app"), "\"build\": \"rollup -c\"");
        touch(&dir, &["packages/app/src/a.ts"]);
        // A quick check script written at the top and removed again.
        let files = ["check.mts".to_owned(), "packages/app/src/a.ts".to_owned()];
        assert_eq!(commands(&dir, &files), ["cd packages/app && npm run build"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_build_counts_for_the_package_it_ran_in_or_named_or_every_package() {
        let dir = folder("built");
        std::fs::write(dir.join("pnpm-lock.yaml"), "").unwrap();
        // The vitest repository's shape: the root type checks with a config
        // of its own and builds every package; the package builds itself.
        manifest(
            &dir,
            "\"build\": \"pnpm -r --filter='./packages/**' run build\", \
             \"typecheck\": \"tsc -p tsconfig.check.json --noEmit\"",
        );
        named(
            &dir.join("packages/vitest"),
            "vitest",
            "\"build\": \"premove dist && rollup -c\"",
        );
        manifest(&dir.join("packages/utils"), "\"build\": \"rollup -c\"");
        let file = "packages/vitest/src/node/a.ts";
        let root = dir.to_string_lossy();
        for line in [
            "cd packages/vitest && pnpm run build".to_owned(),
            format!("cd {root}/packages/vitest && ../../node_modules/.bin/rollup -c"),
            "cd packages && cd vitest && rollup -c".to_owned(),
            "pnpm --filter vitest build".to_owned(),
            "pnpm --filter=vitest run build".to_owned(),
            "pnpm -C packages/vitest build".to_owned(),
            "node_modules/.bin/tsc -p packages/vitest/tsconfig.json --noEmit".to_owned(),
            // The root's build is a recursive one.
            "pnpm run build 2>&1 | tail -40".to_owned(),
            "pnpm -r build".to_owned(),
            "npx turbo run build".to_owned(),
        ] {
            assert!(built(&line, &dir, file), "{line}");
        }
        for line in [
            "pnpm typecheck".to_owned(),
            format!("cd {root} && node_modules/.bin/tsc -p tsconfig.check.json --noEmit | head"),
            "cd packages/utils && rollup -c".to_owned(),
            "cd packages/vitest/../utils && rollup -c".to_owned(),
            "cd /elsewhere && tsc".to_owned(),
        ] {
            assert!(!built(&line, &dir, file), "{line}");
        }
        // The root's own files are its builds' to check.
        assert!(built("pnpm typecheck", &dir, "scripts/a.ts"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_file_no_package_builds_counts_as_built_by_any_build() {
        let dir = folder("no-package");
        assert!(built("cd somewhere && tsc", &dir, "src/a.ts"));
        assert!(built("tsc", &dir, "../elsewhere/a.ts"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn npm_builds_where_no_lock_file_says_otherwise_and_no_package_means_no_command() {
        let dir = folder("npm");
        touch(&dir, &["src/a.ts"]);
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
