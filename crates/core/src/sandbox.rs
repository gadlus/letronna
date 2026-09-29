//! Running a command confined by the operating system: no network, writes
//! only in the project and a temp folder of its own, and no reading of keys
//! or of Letronna's own folder.
//!
//! In Accept edits every command asked, and a person who answers many cards
//! stops reading them. Here, in Accept edits and auto mode, a command the
//! gate would otherwise ask about runs confined without a card: what it can
//! break is the project, which an edit could change anyway, and it cannot
//! send anything anywhere. An allow rule does not lift the confinement,
//! since a rule for `cargo test` also covers whatever a build script in the
//! repository does (Codex learnt this); only a yes on a card runs a command
//! outside it.
//!
//! The rules are the kernel's, not ours: on macOS a Seatbelt profile given
//! to `sandbox-exec`. It starts from nothing allowed, so a command also
//! cannot ask another program to act for it (`open`, `osascript`,
//! `launchctl`) or reach the keychain, and it cannot run a setuid program
//! such as `sudo`. On Linux, Landlock and a seccomp filter, in `linux`.
//! Windows reports no sandbox until its own backend comes, and so does a
//! Linux kernel without Landlock; there the gate asks as before.

mod linux;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// A way to confine a command on this system.
pub trait Sandbox: Sync {
    /// Whether commands can be confined here at all.
    fn available(&self) -> bool;
    /// What to spawn in place of `command` so that it runs held to `policy`,
    /// with the same arguments, variables and folder.
    fn wrap(&self, command: &Command, policy: &Policy) -> Result<Command, String>;
}

/// What a confined command may do besides reading: where it may write and
/// what it may not even read. It never has the network.
pub struct Policy {
    /// The folders it may write in, with every link followed, as the kernel
    /// compares paths only after resolving them (`/var` is `/private/var`).
    pub writable: Vec<PathBuf>,
    /// Its own temp folder, one of `writable`, which `TMPDIR` points at.
    pub temp: PathBuf,
    /// Folders it may neither read nor write, even inside `writable`.
    pub hidden: Vec<PathBuf>,
}

/// A temp folder for one confined command, removed with what it holds when
/// dropped.
pub struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Policy {
    /// The policy for a command run in `project`: it writes there and in a
    /// new temp folder, and Letronna's data folder is hidden from it.
    pub fn for_project(project: &Path) -> Result<(Policy, Scratch), String> {
        let project = std::fs::canonicalize(project)
            .map_err(|e| format!("Cannot find the project folder to sandbox: {e}"))?;
        let scratch = scratch()?;
        let data = crate::chat::data_dir();
        let policy = Policy {
            writable: vec![project, scratch.0.clone()],
            temp: scratch.0.clone(),
            hidden: vec![std::fs::canonicalize(&data).unwrap_or(data)],
        };
        Ok((policy, scratch))
    }
}

/// Makes a temp folder no other command shares, under the system's own.
fn scratch() -> Result<Scratch, String> {
    static MADE: AtomicU64 = AtomicU64::new(0);
    let base = std::fs::canonicalize(std::env::temp_dir())
        .map_err(|e| format!("Cannot find the temp folder: {e}"))?;
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    // One left behind by a process that had this id before is skipped.
    for _ in 0..100 {
        let n = MADE.fetch_add(1, Ordering::Relaxed);
        let dir = base.join(format!("letronna-sandbox-{}-{n}", std::process::id()));
        match builder.create(&dir) {
            Ok(()) => return Ok(Scratch(dir)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("Cannot make a temp folder for the sandbox: {e}")),
        }
    }
    Err("Cannot make a temp folder for the sandbox: every name is taken.".into())
}

/// The sandbox for this system.
pub fn here() -> &'static dyn Sandbox {
    if cfg!(target_os = "macos") {
        &Seatbelt
    } else {
        linux::here().unwrap_or(&Unavailable)
    }
}

/// Whether commands can be confined on this system.
pub fn available() -> bool {
    here().available()
}

/// What a confined command's result starts with, so the model knows why it
/// could not reach the network or write elsewhere.
pub const NOTE: &str = "(ran sandboxed: no network, writes only in the project)";

/// What follows `NOTE` when the output looks like the sandbox stopped the
/// command, so the model asks for a run outside it instead of guessing.
const STOPPED: &str = "(this looks like the sandbox stopped it: it has no network, and \
     writes only in the project and its own temp folder. If the command needs \
     more, call run_command again with unsandboxed: true, which needs the \
     user's approval.)";

/// What the kernel and common tools print when the sandbox refuses a file or
/// the network, lowercased: EPERM from any call, EACCES, which Landlock gives
/// for a file, and a host name that cannot be looked up or connected to, as
/// curl, git, cargo, Python and Node word it.
const REFUSALS: &[&str] = &[
    "operation not permitted",
    "eperm",
    "permission denied",
    "eacces",
    "resolve host",
    "nodename nor servname",
    "enotfound",
    "eai_again",
    "couldn't connect",
    "network is unreachable",
    "sandbox-exec",
];

/// A confined command's `output` with `NOTE` in front, and the way out when
/// the sandbox seems to be what stopped it. In front, so that cutting long
/// output short cannot drop them.
pub fn noted(output: &str) -> String {
    let lower = output.to_lowercase();
    if REFUSALS.iter().any(|refusal| lower.contains(refusal)) {
        format!("{NOTE}\n{STOPPED}\n{output}")
    } else {
        format!("{NOTE}\n{output}")
    }
}

/// No sandbox: Windows until its own backend comes, and Linux on a
/// processor the network filter does not know.
struct Unavailable;

impl Sandbox for Unavailable {
    fn available(&self) -> bool {
        false
    }
    fn wrap(&self, _: &Command, _: &Policy) -> Result<Command, String> {
        Err("Commands cannot be sandboxed on this system yet.".into())
    }
}

/// macOS's Seatbelt, through `sandbox-exec`. Apple marks the tool
/// deprecated, but its own services still run under the same profiles, and
/// Chrome, Codex and Claude Code confine commands with it.
struct Seatbelt;

/// Where macOS keeps `sandbox-exec`; the full path, so no program named like
/// it earlier on `PATH` is run instead.
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

impl Sandbox for Seatbelt {
    fn available(&self) -> bool {
        // Tried once, as a process that is itself sandboxed cannot start
        // another sandbox, and nothing else says so.
        static WORKS: OnceLock<bool> = OnceLock::new();
        *WORKS.get_or_init(|| {
            Command::new(SANDBOX_EXEC)
                .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        })
    }

    fn wrap(&self, command: &Command, policy: &Policy) -> Result<Command, String> {
        let mut wrapped = Command::new(SANDBOX_EXEC);
        wrapped.arg("-p").arg(profile(policy));
        // Paths go in as parameters, never into the profile's text, so no
        // folder name can end a string there and add a rule of its own.
        let params = (policy
            .writable
            .iter()
            .enumerate()
            .map(|(i, p)| ("WRITABLE", i, p)))
        .chain(
            policy
                .hidden
                .iter()
                .enumerate()
                .map(|(i, p)| ("HIDDEN", i, p)),
        );
        for (name, i, path) in params {
            let mut param = std::ffi::OsString::from(format!("{name}_{i}="));
            param.push(path);
            wrapped.arg("-D").arg(param);
        }
        wrapped
            .arg("--")
            .arg(command.get_program())
            .args(command.get_args());
        for (key, value) in command.get_envs() {
            match value {
                Some(value) => wrapped.env(key, value),
                None => wrapped.env_remove(key),
            };
        }
        wrapped.env("TMPDIR", &policy.temp);
        if let Some(dir) = command.get_current_dir() {
            wrapped.current_dir(dir);
        }
        Ok(wrapped)
    }
}

/// What every confined command may do, whatever the policy: start programs
/// (which stay confined), signal and look at its own processes, read the
/// system's settings, look up users, and read files. Nothing else is named,
/// so the network, other programs' services, the keychain and setuid
/// programs are all refused. `ipc-posix-sem` is for Python's
/// multiprocessing; `libinfo` is how `whoami` and `ls -l` name users.
const BASE: &str = r#"(version 1)
(deny default)
(allow process-exec process-fork)
(allow signal process-info* (target same-sandbox))
(allow sysctl-read user-preference-read ipc-posix-sem)
(allow mach-lookup (global-name "com.apple.system.opendirectoryd.libinfo"))
(allow file-read*)
(allow file-write-data file-ioctl (literal "/dev/null") (literal "/dev/stdout") (literal "/dev/stderr") (subpath "/dev/fd"))
"#;

/// The Seatbelt profile for `policy`, whose paths it names as the
/// `WRITABLE_n` and `HIDDEN_n` parameters. A later rule wins over an
/// earlier one, so each exception follows what it narrows.
fn profile(policy: &Policy) -> String {
    let mut text = String::from(BASE);
    let writable =
        (0..policy.writable.len()).map(|i| format!(r#"(subpath (param "WRITABLE_{i}"))"#));
    text += &rule("allow file-write*", writable);
    // The paths an edit asks about, which Git, CI, editors and other agents
    // run from or which hold keys: the same list, so a command cannot do
    // unasked what a write_file would have to ask for. Seatbelt compares
    // names without case on a volume that does, `.GIT` and `.ſsh` included.
    let guarded = crate::gate::PROTECTED.iter().chain(crate::tools::SECRETS);
    text += &rule("deny file-write*", guarded.map(|entry| names(entry)));
    // Reading keys would hand them to the model's provider, as the floor
    // says for read_file: `.env` files but the example ones, then the list.
    text += &rule(
        "deny file-read*",
        [r#"(regex #"(^|/)\.env(\.[^/]*)?$")"#.to_owned()],
    );
    text += &rule(
        "allow file-read*",
        [r#"(regex #"(^|/)\.env\.(example|sample)$")"#.to_owned()],
    );
    text += &rule(
        "deny file-read*",
        crate::tools::SECRETS.iter().map(|entry| names(entry)),
    );
    let hidden = (0..policy.hidden.len()).map(|i| format!(r#"(subpath (param "HIDDEN_{i}"))"#));
    text += &rule("deny file-read* file-write*", hidden);
    // Nothing above allows it, but said last so no rule added later can.
    text += "(deny network*)\n";
    text
}

/// One rule of `action` over `filters`, any of which it matches; none when
/// there are no filters, as a rule without one would match everything.
fn rule(action: &str, filters: impl IntoIterator<Item = String>) -> String {
    let filters: Vec<String> = filters.into_iter().collect();
    if filters.is_empty() {
        return String::new();
    }
    format!("({action} {})\n", filters.join(" "))
}

/// A filter for `entry`, a run of names such as `.cargo/config*` where a
/// trailing `*` takes any rest of the name, anywhere in a path and
/// everything beneath it.
fn names(entry: &str) -> String {
    let mut pattern = String::from("(^|/)");
    for c in entry.chars() {
        match c {
            '*' => pattern.push_str("[^/]*"),
            '.' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' | '\\' => {
                pattern.push('\\');
                pattern.push(c);
            }
            _ => pattern.push(c),
        }
    }
    format!(r##"(regex #"{pattern}(/|$)")"##)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_is_empty_when_nothing_needs_the_action() {
        assert_eq!(rule("allow file-write*", Vec::<String>::new()), "");
    }

    #[test]
    fn rule_joins_every_filter_after_the_action() {
        let filters = [
            r#"(literal "/a")"#.to_owned(),
            r#"(literal "/b")"#.to_owned(),
        ];
        let text = rule("allow file-write*", filters);
        assert_eq!(
            text,
            "(allow file-write* (literal \"/a\") (literal \"/b\"))\n"
        );
    }

    #[test]
    fn names_escapes_a_dot_so_it_matches_only_itself() {
        assert!(names(".env").contains(r"\.env"), "{}", names(".env"));
    }

    #[test]
    fn names_turns_a_trailing_star_into_any_run_of_names() {
        let pattern = names(".cargo/config*");
        assert!(pattern.contains("config[^/]*"), "{pattern}");
    }

    #[test]
    fn noted_adds_the_way_out_when_output_looks_like_a_refusal() {
        let said = noted("curl: (6) Could not resolve host: example.com");
        assert!(said.starts_with(NOTE));
        assert!(said.contains("run_command again"), "{said}");
    }

    #[test]
    fn noted_adds_the_way_out_when_landlock_refused_a_file() {
        let said = noted("sh: 1: cannot create ../outside.txt: Permission denied");
        assert!(said.contains("run_command again"), "{said}");
    }

    #[test]
    fn noted_leaves_ordinary_output_with_only_the_note_in_front() {
        assert_eq!(noted("2 tests passed"), format!("{NOTE}\n2 tests passed"));
    }

    #[test]
    fn profile_denies_the_network_and_names_every_path_by_parameter() {
        let policy = Policy {
            writable: vec![PathBuf::from("/a"), PathBuf::from("/b")],
            temp: PathBuf::from("/b"),
            hidden: vec![PathBuf::from("/c")],
        };
        let text = profile(&policy);
        assert!(text.contains("(deny network*)"));
        assert!(text.contains("WRITABLE_0"));
        assert!(text.contains("WRITABLE_1"));
        assert!(text.contains("HIDDEN_0"));
    }

    /// A real Seatbelt run: a write inside the project lands, one outside it
    /// and any reach for the network do not. Skipped where `sandbox-exec`
    /// itself cannot run, such as a sandboxed CI container.
    #[test]
    #[cfg(target_os = "macos")]
    fn seatbelt_lets_the_project_through_and_nothing_else() {
        if !Seatbelt.available() {
            return;
        }
        let dir =
            std::env::temp_dir().join(format!("letronna-sandbox-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("project")).unwrap();
        std::fs::write(dir.join("outside.txt"), "original").unwrap();

        let (policy, _scratch) = Policy::for_project(&dir.join("project")).unwrap();
        let run = |line: &str| {
            let mut command = Command::new("/bin/sh");
            command.arg("-c").arg(line).current_dir(dir.join("project"));
            let wrapped = Seatbelt.wrap(&command, &policy).unwrap();
            run_for_test(wrapped)
        };

        assert!(run("echo hi > inside.txt").success());
        assert_eq!(
            std::fs::read_to_string(dir.join("project/inside.txt")).unwrap(),
            "hi\n"
        );

        assert!(!run("echo hi > ../outside.txt").success());
        assert_eq!(
            std::fs::read_to_string(dir.join("outside.txt")).unwrap(),
            "original"
        );

        assert!(!run("curl -s -o /dev/null http://example.com").success());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(target_os = "macos")]
    fn run_for_test(mut command: Command) -> std::process::ExitStatus {
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
    }
}
