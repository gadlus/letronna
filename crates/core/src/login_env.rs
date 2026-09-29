//! The environment of the user's login shell, for the commands and MCP
//! servers Letronna starts.
//!
//! A window opened from the Dock or a desktop launcher inherits the bare
//! environment of launchd or the desktop session, without what the user's
//! shell profile adds -- Homebrew's, nvm's or cargo's folders on PATH above
//! all -- so `npm`, `cargo` or `npx` went unfound though they work in the
//! user's terminal. So once per run, the first time a command or a server
//! starts, `$SHELL -l -i -c` prints its `env -0` between two markers, and
//! that environment is laid over Letronna's own. The markers keep whatever
//! an rc file prints out of it; stdin is null, so nothing waits for a key;
//! the shell leads a session of its own with no terminal, so an interactive
//! shell cannot stop itself waiting for one, and its whole group is killed
//! when it is done or after `LIMIT`. Whatever fails, the run keeps its own
//! environment, as before, and the log says so in one line.
//!
//! A process with a terminal was started from a shell and has its
//! environment already, one the user may have changed since with a
//! virtualenv or nvm, so it is left as it is; so is Windows, where programs
//! take their PATH from the system rather than from a profile.

use std::ffi::OsString;
use std::sync::OnceLock;
#[cfg(unix)]
use std::{path::Path, time::Duration};

/// How long the login shell may take. A heavy profile (oh-my-zsh, nvm)
/// takes a second or two; past this the first command would seem to hang,
/// so it runs with Letronna's own environment instead.
#[cfg(unix)]
const LIMIT: Duration = Duration::from_secs(5);
/// Output kept from the login shell: an environment is at most a few
/// hundred kilobytes, and an rc file that prints without end must not fill
/// the memory before `LIMIT` stops it.
#[cfg(unix)]
const MAX_OUTPUT: usize = 4 * 1024 * 1024;
/// Set to anything, Letronna keeps its own environment: for a harness that
/// runs Letronna in tests, which must not read the machine's rc files.
const OPT_OUT: &str = "LETRONNA_NO_LOGIN_ENV";
/// Set for the login shell, so an rc file can skip slow work that
/// Letronna has no use for.
#[cfg(unix)]
const RESOLVING: &str = "LETRONNA_RESOLVING_ENV";
/// Variables that describe the login shell itself rather than the user's
/// environment, so they are not carried over.
#[cfg(unix)]
const SHELL_OWN: [&str; 5] = ["PWD", "OLDPWD", "SHLVL", "_", RESOLVING];

/// The login shell's variables to lay over Letronna's own: captured the
/// first time they are asked for, and none where they are not used.
pub(crate) fn vars() -> &'static [(OsString, OsString)] {
    static VARS: OnceLock<Vec<(OsString, OsString)>> = OnceLock::new();
    VARS.get_or_init(load)
}

/// Capture the login shell's variables when this run wants them, logging
/// how it went.
fn load() -> Vec<(OsString, OsString)> {
    if !wanted() {
        return Vec::new();
    }
    #[cfg(unix)]
    {
        from_shell(std::env::var_os("SHELL"), LIMIT)
    }
    #[cfg(not(unix))]
    {
        Vec::new()
    }
}

/// The variables of `shell` as a login shell, or none when it cannot give
/// them within `limit`; either way the log says how it went.
#[cfg(unix)]
fn from_shell(shell: Option<OsString>, limit: Duration) -> Vec<(OsString, OsString)> {
    let started = std::time::Instant::now();
    let result = match shell.filter(|shell| !shell.is_empty()) {
        Some(shell) => capture(Path::new(&shell), limit),
        None => Err("SHELL is not set.".into()),
    };
    crate::diag::event(
        "login_env",
        &[
            ("outcome", crate::diag::outcome(&result).into()),
            ("ms", crate::diag::millis(started)),
            ("vars", result.as_ref().map_or(0, Vec::len).into()),
        ],
    );
    result.unwrap_or_default()
}

/// Whether this run takes the login shell's environment: not on Windows,
/// not in tests, not when opted out, and not when started from a terminal.
fn wanted() -> bool {
    use std::io::IsTerminal;
    let from_terminal = std::io::stdin().is_terminal()
        || std::io::stdout().is_terminal()
        || std::io::stderr().is_terminal();
    cfg!(unix) && !cfg!(test) && std::env::var_os(OPT_OUT).is_none() && !from_terminal
}

/// Run `shell` as an interactive login shell and read back its variables,
/// then kill it and everything it started, at the latest after `limit`.
#[cfg(unix)]
fn capture(shell: &Path, limit: Duration) -> Result<Vec<(OsString, OsString)>, String> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::sync::mpsc::RecvTimeoutError;
    let (start, end) = markers();
    let mut command = Command::new(shell);
    command
        .args(["-l", "-i", "-c", &script(&start, &end)])
        .env(RESOLVING, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // Where a terminal starts too, so a hook that reads the folder it runs
    // in (an .nvmrc, an .envrc) sees the user's own, not a project's.
    if let Some(home) = crate::tools::home().filter(|home| home.is_dir()) {
        command.current_dir(home);
    }
    // SAFETY: setsid is async-signal-safe and touches no memory of this
    // process, so it may run between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("Cannot start the login shell: {e}"))?;
    let group = child.id() as libc::pid_t;
    let (sender, chunks) = std::sync::mpsc::channel();
    if let Some(mut out) = child.stdout.take() {
        // Read on a thread, so the wait below can give up on a shell that
        // prints nothing; the pipe closes once the group is killed.
        std::thread::spawn(move || {
            let mut buf = [0; 8192];
            while let Ok(n @ 1..) = out.read(&mut buf) {
                if sender.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
    }
    let deadline = std::time::Instant::now() + limit;
    let mut output = Vec::new();
    let read = loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match chunks.recv_timeout(left) {
            Ok(chunk) if !left.is_zero() => {
                let from = output.len().saturating_sub(end.len());
                output.extend(chunk);
                if find(&output[from..], end.as_bytes()).is_some() {
                    break Ok(());
                }
                if output.len() > MAX_OUTPUT {
                    break Err("The login shell printed too much.".to_owned());
                }
            }
            // Output that keeps coming past the limit is no reason to wait.
            Ok(_) | Err(RecvTimeoutError::Timeout) => {
                break Err(format!(
                    "The login shell timed out after {}s.",
                    limit.as_secs()
                ));
            }
            Err(RecvTimeoutError::Disconnected) => {
                break Err("The login shell ended without printing its environment.".to_owned());
            }
        }
    };
    // The environment is in, or never will be: nothing the shell or its rc
    // files left running has anyone to answer to.
    // SAFETY: this only sends a signal, to the group the shell leads; the
    // shell is not reaped yet, so the group's id cannot have been reused.
    unsafe { libc::kill(-group, libc::SIGKILL) };
    let _ = child.wait();
    read?;
    let vars = parse(&output, &start, &end)
        .ok_or("The login shell printed no environment between the markers.")?;
    if vars.is_empty() {
        return Err("The login shell printed an empty environment.".into());
    }
    Ok(vars)
}

/// Two markers no rc file prints by chance, as they hold this process's id
/// and the time.
#[cfg(unix)]
fn markers() -> (String, String) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let token = format!("{}_{now}", std::process::id());
    (
        format!("__LETRONNA_ENV_START_{token}__"),
        format!("__LETRONNA_ENV_END_{token}__"),
    )
}

/// The line the login shell runs: its environment between the markers.
#[cfg(unix)]
fn script(start: &str, end: &str) -> String {
    // Each marker goes out in two halves, so a shell that echoes what it
    // runs (`set -x` with stderr sent to stdout) never prints one early.
    // `command` passes over an alias or a function an rc file made of them.
    let (a, b) = start.split_at(start.len() / 2);
    let (c, d) = end.split_at(end.len() / 2);
    format!("command printf '%s%s' '{a}' '{b}'; command env -0; command printf '%s%s' '{c}' '{d}'")
}

/// The variables `env -0` printed between `start` and `end`, whatever the
/// shell printed around them; none when a marker is missing.
#[cfg(unix)]
fn parse(output: &[u8], start: &str, end: &str) -> Option<Vec<(OsString, OsString)>> {
    use std::os::unix::ffi::OsStringExt;
    let from = find(output, start.as_bytes())? + start.len();
    let to = from + find(&output[from..], end.as_bytes())?;
    let vars = output[from..to]
        .split(|&byte| byte == 0)
        .filter_map(|entry| {
            let at = entry.iter().position(|&byte| byte == b'=')?;
            let name = &entry[..at];
            let own = SHELL_OWN.iter().any(|own| own.as_bytes() == name);
            // A space or a line break in a name means the shell's trace of
            // `env` was sent into the output ahead of the first variable.
            let traced = name
                .iter()
                .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control());
            (at > 0 && !own && !traced).then(|| {
                let value = entry[at + 1..].to_vec();
                (OsString::from_vec(name.to_vec()), OsString::from_vec(value))
            })
        })
        .collect();
    Some(vars)
}

/// Where `needle` first appears in `haystack`.
#[cfg(unix)]
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::time::Instant;

    /// A stand-in for `$SHELL`: a script in a scratch folder of its own,
    /// which gets the line to run as its fourth argument.
    fn fake_shell(name: &str, body: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("letronna-login-env-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shell");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn value<'a>(vars: &'a [(OsString, OsString)], name: &str) -> Option<&'a str> {
        vars.iter()
            .find(|(found, _)| found == name)
            .and_then(|(_, value)| value.to_str())
    }

    #[test]
    fn the_environment_is_read_between_the_markers_whatever_is_printed_around_it() {
        let (start, end) = markers();
        let output = format!(
            "Welcome back!\nLast login: today\0HALF=noise\n{start}+ command env -0\nFIRST=traced\0PATH=/opt/tools/bin:/usr/bin\0\
             NOTE=two\nlines\0EQUALS=a=b\0PWD=/somewhere\0SHLVL=2\0{RESOLVING}=1\0\0{end}\
             bye\0LATE=1\0"
        );

        let vars = parse(output.as_bytes(), &start, &end).unwrap();

        assert_eq!(value(&vars, "PATH"), Some("/opt/tools/bin:/usr/bin"));
        assert_eq!(value(&vars, "NOTE"), Some("two\nlines"));
        assert_eq!(value(&vars, "EQUALS"), Some("a=b"));
        assert_eq!(vars.len(), 3, "{vars:?}");
        assert!(parse(format!("{start}PATH=/x\0").as_bytes(), &start, &end).is_none());
        assert!(parse(format!("PATH=/x\0{end}").as_bytes(), &start, &end).is_none());
    }

    #[test]
    fn a_login_shell_gives_its_variables_and_its_noise_is_left_out() {
        let shell = fake_shell(
            "gives",
            "echo 'Welcome back!'\nexport FROM_PROFILE=yes\nexec /bin/sh -x -c \"$4\" 2>&1",
        );

        let vars = capture(&shell, Duration::from_secs(5)).unwrap();

        assert_eq!(value(&vars, "FROM_PROFILE"), Some("yes"));
        assert!(
            vars.iter()
                .all(|(name, _)| !name.to_string_lossy().contains(' ')),
            "{vars:?}"
        );
        for own in SHELL_OWN {
            assert_eq!(value(&vars, own), None, "{own} was carried over");
        }
        std::fs::remove_dir_all(shell.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_login_shell_that_hangs_is_killed_with_what_it_started_at_the_limit() {
        let shell = fake_shell(
            "hangs",
            "echo 'Checking for updates...'\nsleep 60 &\necho $! > \"$(dirname \"$0\")/child\"\nwait",
        );
        let written = shell.with_file_name("child");

        // macOS checks a new executable the first time it runs, which on a
        // loaded machine can take the whole limit before the shell's first
        // line, so the limit doubles until the shell starts its child in time.
        let child = [1, 2, 4, 8, 16]
            .into_iter()
            .find_map(|secs| {
                let limit = Duration::from_secs(secs);
                let _ = std::fs::remove_file(&written);
                let started = Instant::now();
                let error = capture(&shell, limit).unwrap_err();
                assert!(error.contains("timed out"), "{error}");
                // Far short of the 60 s the shell would wait for its child.
                assert!(started.elapsed() < limit + Duration::from_secs(10));
                let child = std::fs::read_to_string(&written).ok()?;
                child.trim().parse::<libc::pid_t>().ok()
            })
            .expect("the shell never started its child");

        let deadline = Instant::now() + Duration::from_secs(10);
        // SAFETY: signal 0 only asks whether the process is there.
        while unsafe { libc::kill(child, 0) } == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_ne!(
            unsafe { libc::kill(child, 0) },
            0,
            "{child} outlived the shell"
        );
        std::fs::remove_dir_all(shell.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_login_shell_that_fails_leaves_the_environment_as_it_was() {
        let shell = fake_shell("fails", "echo 'bad option: -l'\nexit 1");
        let missing = shell.with_file_name("no-such-shell");

        let failed = capture(&shell, Duration::from_secs(5)).unwrap_err();
        let unstarted = capture(&missing, Duration::from_secs(5)).unwrap_err();

        assert!(failed.contains("without printing"), "{failed}");
        assert!(unstarted.contains("Cannot start"), "{unstarted}");
        let taken =
            |shell: Option<&Path>| from_shell(shell.map(Into::into), Duration::from_secs(5));
        assert!(taken(Some(&shell)).is_empty());
        assert!(taken(Some(&missing)).is_empty());
        assert!(taken(None).is_empty());
        std::fs::remove_dir_all(shell.parent().unwrap()).unwrap();
    }
}
