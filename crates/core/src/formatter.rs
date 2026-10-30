//! The project's own formatter, run on a file the agent has just written,
//! in a project whose user asked for it.
//!
//! Without it an edit lands in whatever layout the model typed, and the
//! project's formatter rewrites it at the next commit hook or check, a
//! diff the user then reads as noise. Here, once write_file, edit_file or
//! apply_patch has written a file, the formatter the project already uses
//! for that kind of file is run on it alone, and the result says so.
//!
//! Nothing runs unless asked: `format.json` in the data folder lists the
//! projects it is on for, none by default, for the reason `lsp.json` names
//! its projects. A formatter is a program the project picks -- prettier
//! loads the plugins its config names from `node_modules` -- and whoever
//! wrote a repo may not be whoever runs it. Only one the project has set up
//! and this computer already has is used, never one fetched for the
//! occasion, and it gets a few seconds.
//!
//! What it changed counts as what the model has seen of the file: it is
//! the model's own text in the project's layout. Its next edit is matched
//! against what the file holds now, and one whose `old` no longer matches
//! sends it back to read, as for any edit.

use crate::tools::ToolContext;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

/// The projects formatting is on for, in the data folder.
#[cfg_attr(test, allow(dead_code))]
const FILE: &str = "format.json";
/// How long a formatter gets: one file takes rustfmt or prettier well under
/// a second, and a hung one must not hold up the call.
const WAIT: Duration = Duration::from_secs(10);
/// How often a running formatter is looked at.
const POLL: Duration = Duration::from_millis(20);
/// Characters kept of why a formatter left a file alone.
const WHY_CHARS: usize = 200;

/// `format.json`: the project folders formatting is on for.
#[derive(Deserialize, Default)]
struct Config {
    #[serde(default)]
    projects: Vec<PathBuf>,
}

/// A formatter for one file: what to call it, the program, and the
/// arguments that go before the file.
#[derive(Debug, PartialEq)]
pub struct Formatter {
    pub name: &'static str,
    pub program: PathBuf,
    pub args: Vec<String>,
}

/// Where `format.json` is.
#[cfg(not(test))]
fn config_file() -> PathBuf {
    crate::chat::data_dir().join(FILE)
}

#[cfg(test)]
thread_local! {
    /// The config a test gives, so no test reads the real data folder.
    pub(crate) static CONFIG: std::cell::RefCell<PathBuf> =
        const { std::cell::RefCell::new(PathBuf::new()) };
}

#[cfg(test)]
fn config_file() -> PathBuf {
    CONFIG.with(|config| config.borrow().clone())
}

/// Whether formatting after edits is on for the project at `root`.
pub fn on(root: &Path) -> bool {
    let config: Config = crate::state::read(&config_file()).unwrap_or_default();
    config
        .projects
        .iter()
        .any(|project| crate::lsp::same_folder(project, root))
}

/// Turn formatting after edits on or off for the project at `root`.
pub fn switch(root: &Path, on: bool) -> Result<(), String> {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    crate::state::update::<serde_json::Value>(&config_file(), |disk| {
        if !disk.is_object() {
            *disk = serde_json::json!({});
        }
        let Some(file) = disk.as_object_mut() else {
            return;
        };
        let projects = file
            .entry("projects")
            .or_insert_with(|| serde_json::json!([]));
        if !projects.is_array() {
            *projects = serde_json::json!([]);
        }
        let Some(projects) = projects.as_array_mut() else {
            return;
        };
        projects.retain(|project| {
            project
                .as_str()
                .is_none_or(|project| !crate::lsp::same_folder(Path::new(project), &root))
        });
        if on {
            projects.push(root.to_string_lossy().into_owned().into());
        }
    })
}

/// The formatters the project at `root` has set up at its top and this
/// computer has, each with the kinds of file it takes, as a switch shows
/// them before it is turned on.
pub fn found(root: &Path) -> Vec<(Formatter, &'static str)> {
    let mut found: Vec<(Formatter, &'static str)> = Vec::new();
    for (file, kinds) in KINDS {
        let Some(formatter) = for_file(root, &root.join(file)) else {
            continue;
        };
        if !found.iter().any(|(had, _)| had.name == formatter.name) {
            found.push((formatter, kinds));
        }
    }
    found
}

/// A file of each kind a formatter here takes, as `found` asks about them,
/// and how the kinds are said.
const KINDS: [(&str, &str); 5] = [
    ("x.rs", ".rs"),
    ("x.ts", ".js, .ts, .json, .css and more"),
    ("x.py", ".py"),
    ("x.go", ".go"),
    ("x.c", ".c, .cpp and .h"),
];

/// The formatter the project at `root` uses for `path`, when it has one set
/// up and this computer has it.
pub fn for_file(root: &Path, path: &Path) -> Option<Formatter> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "rs" => rustfmt(root, path),
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts" | "json" | "jsonc" | "css"
        | "scss" | "less" | "html" | "vue" | "svelte" | "md" | "mdx" | "yaml" | "yml"
        | "graphql" => prettier(root, path).or_else(|| biome(root, path, &extension)),
        "py" | "pyi" => python(root, path),
        "go" => {
            up(root, path, &["go.mod"])?;
            program("gofmt", "gofmt", &["-w"])
        }
        "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "m" | "mm" => {
            up(root, path, &[".clang-format", "_clang-format"])?;
            program("clang-format", "clang-format", &["-i"])
        }
        _ => None,
    }
}

/// rustfmt, for a file in a Cargo project or under a rustfmt.toml, told the
/// crate's edition, which it cannot work out from one file alone.
fn rustfmt(root: &Path, path: &Path) -> Option<Formatter> {
    let config = up(root, path, &["rustfmt.toml", ".rustfmt.toml"]);
    let manifest = up(root, path, &["Cargo.toml"]);
    if config.is_none() && manifest.is_none() {
        return None;
    }
    let mut formatter = program("rustfmt", "rustfmt", &[])?;
    // A rustfmt.toml that names an edition is read by rustfmt itself.
    if let Some(edition) = manifest.and_then(|manifest| edition(root, &manifest)) {
        formatter.args = vec!["--edition".into(), edition];
    }
    Some(formatter)
}

/// The edition of the crate whose manifest is `manifest`: its own, or its
/// workspace's when it takes that one.
fn edition(root: &Path, manifest: &Path) -> Option<String> {
    let read = |file: &Path| {
        std::fs::read_to_string(file)
            .ok()?
            .parse::<toml::Table>()
            .ok()
    };
    let package = read(manifest)?;
    let edition = package.get("package")?.get("edition")?;
    if let Some(edition) = edition.as_str() {
        return Some(edition.to_owned());
    }
    // `edition.workspace = true`: the nearest workspace above says.
    let mut folder = manifest.parent()?.parent();
    while let Some(here) = folder.filter(|here| here.starts_with(root)) {
        let shared = read(&here.join("Cargo.toml")).and_then(|table| {
            let edition = table.get("workspace")?.get("package")?.get("edition")?;
            edition.as_str().map(str::to_owned)
        });
        if shared.is_some() {
            return shared;
        }
        folder = here.parent();
    }
    None
}

/// prettier from the project's own `node_modules`, for a file under a
/// prettier config.
fn prettier(root: &Path, path: &Path) -> Option<Formatter> {
    let configs = [
        ".prettierrc",
        ".prettierrc.json",
        ".prettierrc.json5",
        ".prettierrc.yaml",
        ".prettierrc.yml",
        ".prettierrc.toml",
        ".prettierrc.js",
        ".prettierrc.cjs",
        ".prettierrc.mjs",
        "prettier.config.js",
        "prettier.config.cjs",
        "prettier.config.mjs",
        "prettier.config.ts",
    ];
    let set_up = up(root, path, &configs).is_some()
        || up(root, path, &["package.json"]).is_some_and(|package| {
            std::fs::read(package)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .is_some_and(|package| package.get("prettier").is_some())
        });
    if !set_up {
        return None;
    }
    let program = up(root, path, &[&own_bin("prettier")])?;
    Some(Formatter {
        name: "prettier",
        program,
        args: vec!["--write".into()],
    })
}

/// Biome from the project's own `node_modules`, for a file under a
/// biome.json, of a kind it formats.
fn biome(root: &Path, path: &Path, extension: &str) -> Option<Formatter> {
    let kinds = [
        "js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts", "json", "jsonc", "css", "graphql",
    ];
    if !kinds.contains(&extension) {
        return None;
    }
    up(root, path, &["biome.json", "biome.jsonc"])?;
    let program = up(root, path, &[&own_bin("biome")])?;
    Some(Formatter {
        name: "biome",
        program,
        args: vec!["format".into(), "--write".into()],
    })
}

/// ruff, or black, whichever the project's config sets up, from its own
/// virtual environment or PATH.
fn python(root: &Path, path: &Path) -> Option<Formatter> {
    let pyproject = up(root, path, &["pyproject.toml"])
        .and_then(|file| std::fs::read_to_string(file).ok())
        .unwrap_or_default();
    let ruff =
        pyproject.contains("[tool.ruff") || up(root, path, &["ruff.toml", ".ruff.toml"]).is_some();
    let (name, args): (&'static str, &[&str]) = if ruff {
        ("ruff", &["format", "--quiet"])
    } else if pyproject.contains("[tool.black") {
        ("black", &["--quiet"])
    } else {
        return None;
    };
    let (folder, binary) = if cfg!(windows) {
        ("Scripts", format!("{name}.exe"))
    } else {
        ("bin", name.to_owned())
    };
    let own = [".venv", "venv"]
        .iter()
        .map(|venv| root.join(venv).join(folder).join(&binary))
        .find(|program| program.is_file());
    match own {
        Some(program) => Some(Formatter {
            name,
            program,
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        }),
        None => program(name, name, args),
    }
}

/// Where npm puts the project's own `name`: a script, and beside it on
/// Windows the .cmd that runs it.
fn own_bin(name: &str) -> String {
    let end = if cfg!(windows) { ".cmd" } else { "" };
    format!("node_modules/.bin/{name}{end}")
}

/// `binary` on PATH as the login shell sets it, called `name`.
fn program(name: &'static str, binary: &str, args: &[&str]) -> Option<Formatter> {
    Some(Formatter {
        name,
        program: crate::files::on_path(binary)?,
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
    })
}

/// The first of `names` in the folder holding `path` or one above it, up to
/// and including `root`.
fn up(root: &Path, path: &Path, names: &[&str]) -> Option<PathBuf> {
    let mut folder = path.parent();
    while let Some(here) = folder.filter(|here| here.starts_with(root)) {
        let found = names
            .iter()
            .map(|name| here.join(name))
            .find(|file| file.exists());
        if found.is_some() {
            return found;
        }
        folder = here.parent();
    }
    None
}

/// Run the project's formatter on `path`, which the conversation has just
/// written as `written`, where formatting is on for its project: what the
/// file holds after and a note naming `raw` for the result, or None when
/// nothing ran or nothing changed.
pub(crate) fn after_write(
    ctx: &ToolContext,
    path: &Path,
    raw: &str,
    written: &str,
) -> Option<(String, String)> {
    let root = ctx.workspace.as_deref()?;
    if !path.starts_with(root) || !on(root) {
        return None;
    }
    let formatter = for_file(root, path)?;
    let name = formatter.name;
    if let Err(why) = run(&formatter, root, path, ctx) {
        return Some((
            written.to_owned(),
            format!(" {name} left `{raw}` as it was: {why}."),
        ));
    }
    let now = std::fs::read_to_string(path).ok()?;
    if now == written {
        return None;
    }
    crate::filestate::reformatted(ctx.conversation, path, written.as_bytes(), now.as_bytes());
    Some((
        now,
        format!(
            " Then {name} put `{raw}` in the project's layout; what it holds now is \
             what an edit matches."
        ),
    ))
}

/// Run `formatter` on `path` in the project at `root`, ending it and
/// everything it started when it takes too long or the call is stopped.
fn run(formatter: &Formatter, root: &Path, path: &Path, ctx: &ToolContext) -> Result<(), String> {
    let login = crate::login_env::vars();
    let mut command = std::process::Command::new(&formatter.program);
    command
        .args(&formatter.args)
        .arg(path)
        .current_dir(root)
        .envs(login.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command
        .spawn()
        .map_err(|error| format!("it would not start ({error})"))?;
    let tree = match crate::jobs::Tree::of(&child) {
        Ok(tree) => tree,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    let (sender, open) = std::sync::mpsc::channel();
    let said = child
        .stderr
        .take()
        .map(|stderr| crate::tools::forward(stderr, sender));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                tree.kill();
                let _ = child.wait();
                return Err(format!("it could not be waited on ({error})"));
            }
        }
        let late = started.elapsed() > WAIT;
        if late || !ctx.cancel.sleep(POLL) {
            tree.kill();
            let _ = child.wait();
            return Err(match late {
                true => format!("it took more than {} s", WAIT.as_secs()),
                false => "it was stopped with the call".into(),
            });
        }
    };
    if status.success() {
        return Ok(());
    }
    crate::tools::pipes_closed(&open);
    let said = said.map(|said| crate::tools::take(&said).into_text().0);
    let why = said
        .as_deref()
        .and_then(|said| said.lines().map(str::trim).find(|line| !line.is_empty()))
        .map_or_else(
            || "it failed".to_owned(),
            |line| line.chars().take(WHY_CHARS).collect(),
        );
    Err(why)
}

#[cfg(test)]
mod tests {
    use super::{CONFIG, Formatter, edition, for_file, found, on, switch, up};
    use std::path::{Path, PathBuf};

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("letronna-format-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(dir).unwrap()
    }

    fn put(dir: &Path, file: &str, text: &str) {
        let path = dir.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn formatting_is_off_until_a_project_turns_it_on_and_off_again_after() {
        let dir = scratch("switch");
        CONFIG.with(|config| *config.borrow_mut() = dir.join("format.json"));
        let project = dir.join("project");
        std::fs::create_dir_all(&project).unwrap();
        assert!(!on(&project));
        switch(&project, true).unwrap();
        switch(&project, true).unwrap();
        assert!(on(&project));
        assert!(!on(&dir));
        let file: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("format.json")).unwrap()).unwrap();
        assert_eq!(file["projects"].as_array().map(Vec::len), Some(1));
        switch(&project, false).unwrap();
        assert!(!on(&project));
    }

    #[test]
    fn a_config_is_looked_for_from_the_file_up_to_the_project_and_no_further() {
        let dir = scratch("up");
        let project = dir.join("project");
        put(&dir, ".prettierrc", "{}");
        put(&project, "web/.prettierrc", "{}");
        put(&project, "web/src/a.ts", "");
        put(&project, "api/b.ts", "");
        let near = up(&project, &project.join("web/src/a.ts"), &[".prettierrc"]);
        assert_eq!(near, Some(project.join("web/.prettierrc")));
        assert_eq!(
            up(&project, &project.join("api/b.ts"), &[".prettierrc"]),
            None
        );
    }

    #[test]
    fn prettier_is_used_only_from_the_project_s_own_node_modules() {
        let project = scratch("prettier");
        put(&project, "package.json", r#"{"prettier": {"semi": false}}"#);
        put(&project, "src/a.ts", "");
        assert_eq!(for_file(&project, &project.join("src/a.ts")), None);
        put(&project, &super::own_bin("prettier"), "");
        let formatter = for_file(&project, &project.join("src/a.ts")).unwrap();
        assert_eq!(formatter.name, "prettier");
        assert_eq!(formatter.program, project.join(super::own_bin("prettier")));
        assert_eq!(formatter.args, vec!["--write".to_owned()]);
        // A kind it is not given stays as it is.
        assert_eq!(for_file(&project, &project.join("src/a.rs")), None);
    }

    #[test]
    fn a_project_with_no_formatter_set_up_gets_none_whatever_is_installed() {
        let project = scratch("bare");
        put(&project, "a.ts", "");
        put(&project, "b.py", "");
        put(&project, "c.go", "");
        put(&project, "node_modules/.bin/prettier", "");
        for file in ["a.ts", "b.py", "c.go", "d.c"] {
            assert_eq!(for_file(&project, &project.join(file)), None, "{file}");
        }
        assert!(found(&project).is_empty());
    }

    #[test]
    fn ruff_is_taken_from_the_project_s_virtual_environment_first() {
        let project = scratch("ruff");
        put(
            &project,
            "pyproject.toml",
            "[tool.ruff]\nline-length = 100\n",
        );
        let own = if cfg!(windows) {
            ".venv/Scripts/ruff.exe"
        } else {
            ".venv/bin/ruff"
        };
        put(&project, own, "");
        let formatter = for_file(&project, &project.join("app/main.py")).unwrap();
        assert_eq!(
            formatter,
            Formatter {
                name: "ruff",
                program: project.join(own),
                args: vec!["format".into(), "--quiet".into()],
            }
        );
    }

    #[test]
    fn a_crate_s_edition_is_its_own_or_its_workspace_s() {
        let project = scratch("edition");
        put(
            &project,
            "Cargo.toml",
            "[workspace]\nmembers = [\"a\", \"b\"]\n[workspace.package]\nedition = \"2024\"\n",
        );
        put(
            &project,
            "a/Cargo.toml",
            "[package]\nname = \"a\"\nedition.workspace = true\n",
        );
        put(
            &project,
            "b/Cargo.toml",
            "[package]\nname = \"b\"\nedition = \"2021\"\n",
        );
        assert_eq!(
            edition(&project, &project.join("a/Cargo.toml")).as_deref(),
            Some("2024")
        );
        assert_eq!(
            edition(&project, &project.join("b/Cargo.toml")).as_deref(),
            Some("2021")
        );
    }
}
