//! A paired phone's agent: `letronna acp --phone`, a process of its own for
//! each phone connected, whose stdin and stdout are the phone's messages.
//!
//! The phone speaks the Agent Client Protocol, as an editor does, so it is
//! served by the terminal's own agent, held to what a phone may do: Default
//! or Plan, and a yes for one call at a time (the terminal's `acp.rs`). Its
//! sessions are ordinary ones, in the same listing, so one begun on the
//! phone can be carried on at the desk. Each phone gets a process of its
//! own: one phone's turns are nothing to another's, and a phone that goes
//! takes what it started with it.

use super::switchboard::{Event, Switchboard};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

/// How long an agent whose phone went has to end its turns, once its input
/// is closed, before it is ended for it.
const WIND_DOWN: Duration = Duration::from_secs(5);

/// One phone's agent.
pub struct Agent {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
}

impl Agent {
    /// Start one with the terminal at `program`, each message it writes
    /// going to `out`.
    pub fn start(
        program: &Path,
        out: impl FnMut(Vec<u8>) + Send + 'static,
    ) -> Result<Self, String> {
        let mut command = Command::new(program);
        command.args(["acp", "--phone"]);
        Self::start_with(command, out)
    }

    fn start_with(
        mut command: Command,
        mut out: impl FnMut(Vec<u8>) + Send + 'static,
    ) -> Result<Self, String> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // What it says aside is the editor's log; nobody reads it here.
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("Could not start Letronna's agent for the phone: {e}"))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            return Err("Letronna's agent for the phone has no input or output.".into());
        };
        std::thread::Builder::new()
            .name("phone-agent".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else {
                        break;
                    };
                    if !line.trim().is_empty() {
                        out(line.into_bytes());
                    }
                }
            })
            .map_err(|e| format!("Could not start Letronna's agent for the phone: {e}"))?;
        Ok(Agent {
            child: Some(child),
            stdin: Some(stdin),
        })
    }

    /// Hand it a message from the phone, as the one line the protocol has
    /// each in. One that is not JSON is not passed on.
    pub fn tell(&mut self, message: &[u8]) -> Result<(), String> {
        let value: serde_json::Value = serde_json::from_slice(message)
            .map_err(|_| "The phone sent a message that is not JSON.".to_owned())?;
        let mut line = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
        line.push(b'\n');
        let stdin = self.stdin.as_mut().ok_or("The phone's agent has ended.")?;
        stdin
            .write_all(&line)
            .and_then(|()| stdin.flush())
            .map_err(|_| "The phone's agent has ended.".to_owned())
    }
}

impl Drop for Agent {
    /// Close its input, which has it stop its turns and end, and end it if
    /// it has not within `WIND_DOWN`. Waited on aside, so a phone going
    /// never holds up the window.
    fn drop(&mut self) {
        drop(self.stdin.take());
        let Some(mut child) = self.child.take() else {
            return;
        };
        let _ = std::thread::Builder::new()
            .name("phone-agent-end".into())
            .spawn(move || {
                let until = Instant::now() + WIND_DOWN;
                while Instant::now() < until {
                    if !matches!(child.try_wait(), Ok(None)) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                let _ = child.kill();
                let _ = child.wait();
            });
    }
}

/// The agents of the phones connected now, which a host keeps beside its
/// `Switchboard`.
#[derive(Default)]
pub struct Agents {
    running: HashMap<u64, Agent>,
}

impl Agents {
    /// Deal with what happened to a phone: a phone's message goes to its
    /// agent, started with its first, and a phone gone takes its agent with
    /// it. What went wrong is given for the host to show; the phone it
    /// happened to is hung up on.
    pub fn take(&mut self, event: &Event, board: &Switchboard) -> Result<(), String> {
        match event {
            Event::Message { phone, message } => {
                if !self.running.contains_key(phone) {
                    let started = terminal()
                        .ok_or_else(|| {
                            format!(
                                "Letronna's terminal program, which answers the phone, was \
                                 not found {LOOKED}."
                            )
                        })
                        .and_then(|program| Agent::start(&program, board.to(*phone)));
                    match started {
                        Ok(agent) => {
                            self.running.insert(*phone, agent);
                        }
                        Err(why) => {
                            board.hang_up(*phone);
                            return Err(why);
                        }
                    }
                }
                let told = self.running.get_mut(phone).map(|agent| agent.tell(message));
                if let Some(Err(why)) = told {
                    self.running.remove(phone);
                    board.hang_up(*phone);
                    return Err(why);
                }
                Ok(())
            }
            Event::Gone { phone, .. } => {
                self.running.remove(phone);
                Ok(())
            }
            Event::State(_) | Event::Direct(_) | Event::Arrived { .. } => Ok(()),
        }
    }
}

/// Where `terminal` looks on this system, for the message that it found
/// none.
const LOOKED: &str = if cfg!(windows) {
    "beside the window or on PATH"
} else if cfg!(target_os = "macos") {
    "inside the app, beside the window, in ~/.local/share/letronna/bin or on PATH"
} else {
    "beside the window, in ~/.local/share/letronna/bin or on PATH"
};

/// The terminal program for the window to start, as only a window answers
/// phones: the one inside the app on a Mac, otherwise the one beside the
/// window, where the installers put it, or on PATH.
fn terminal() -> Option<PathBuf> {
    let me = std::env::current_exe().ok()?;
    let dir = me.parent()?;
    let name = |path: &Path| {
        (path.file_stem())
            .map(|stem| stem.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    if cfg!(windows) {
        // Beside the window, `letronna.exe` is the window itself; the
        // terminal is `bin\letronnacli.exe`.
        let exe = crate::update::TERMINAL_EXE;
        if name(&me) == "letronnacli" {
            return Some(me);
        }
        return [dir.join("bin").join(exe), dir.join(exe)]
            .into_iter()
            .chain(on_path(exe))
            .find(|path| path.is_file());
    }
    #[cfg(unix)]
    {
        let home = crate::tools::home();
        let path = std::env::var_os("PATH");
        pick(
            &me,
            home.as_deref(),
            path.iter().flat_map(std::env::split_paths),
        )
    }
    #[cfg(not(unix))]
    None
}

/// The first `letronna` the window at `me` can start: inside its app, beside
/// it, where the installers put the terminal, in `~/.local/bin`, then in the
/// folders of `path`.
#[cfg(unix)]
fn pick(me: &Path, home: Option<&Path>, path: impl Iterator<Item = PathBuf>) -> Option<PathBuf> {
    let dir = me.parent()?;
    // Only an app has the folder; beside any other, `Helpers` is not ours.
    let app = (dir.ends_with("Contents/MacOS")).then(|| dir.with_file_name("Helpers"));
    // In this order: `~/.local/bin/letronna` is a link to the installed
    // terminal on a Mac, but on Linux the window, or a copy it made of itself.
    let installed = (home.into_iter())
        .flat_map(|home| [".local/share/letronna/bin", ".local/bin"].map(|at| home.join(at)));
    (app.into_iter())
        .chain([dir.to_path_buf()])
        .chain(installed)
        // One that is not a whole path leads wherever the window was started.
        .chain(path.filter(|folder| folder.is_absolute()))
        .map(|folder| folder.join("letronna"))
        .find(|found| usable(found, me))
}

/// Whether the window at `me` can start `found` as the terminal: a file that
/// may be run, and not the window under another name.
#[cfg(unix)]
fn usable(found: &Path, me: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(file) = std::fs::metadata(found) else {
        return false;
    };
    // A file keeps its number through a link, and through `letronna` for
    // `Letronna` on a disk that ignores case. The window started as the
    // terminal opens a second window and never answers the phone.
    let window = std::fs::metadata(me)
        .is_ok_and(|window| (window.dev(), window.ino()) == (file.dev(), file.ino()));
    file.is_file() && file.mode() & 0o111 != 0 && !window
}

fn on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

#[cfg(all(test, unix))]
mod tests {
    use super::{Agent, pick};
    use crate::store::tests::Scratch;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::mpsc;
    use std::time::Duration;

    /// An agent that is `cat`: it says back each line it is given.
    fn echo() -> (Agent, mpsc::Receiver<Vec<u8>>) {
        let (said, heard) = mpsc::channel();
        let mut cat = Command::new("sh");
        cat.args(["-c", "cat"]);
        let agent = Agent::start_with(cat, move |line| {
            let _ = said.send(line);
        })
        .unwrap();
        (agent, heard)
    }

    #[test]
    fn a_message_goes_in_as_one_line_and_what_comes_out_goes_back() {
        let (mut agent, heard) = echo();
        agent
            .tell(b"{\n  \"jsonrpc\": \"2.0\",\n  \"method\": \"initialize\"\n}")
            .unwrap();
        let line = heard.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(line, br#"{"jsonrpc":"2.0","method":"initialize"}"#);
        // One line apiece, however many come.
        agent.tell(br#"{"a":1}"#).unwrap();
        agent.tell(br#"{"b":2}"#).unwrap();
        assert_eq!(
            heard.recv_timeout(Duration::from_secs(5)).unwrap(),
            br#"{"a":1}"#
        );
        assert_eq!(
            heard.recv_timeout(Duration::from_secs(5)).unwrap(),
            br#"{"b":2}"#
        );
    }

    #[test]
    fn what_is_not_json_is_not_passed_on() {
        let (mut agent, heard) = echo();
        assert!(agent.tell(b"rm -rf ~\n{}").is_err());
        assert!(heard.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn an_agent_let_go_of_ends() {
        let (agent, heard) = echo();
        drop(agent);
        // Its output closes once it has ended.
        assert!(matches!(
            heard.recv_timeout(Duration::from_secs(5)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
    }

    /// An empty program at `path`, its folders made.
    fn program(path: PathBuf) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// What the window at `window` starts, with `home` and `bin` under
    /// `dir` as the home folder and the one folder on PATH.
    fn picked(window: &Path, dir: &Path) -> Option<PathBuf> {
        pick(
            window,
            Some(&dir.join("home")),
            [dir.join("bin")].into_iter(),
        )
    }

    #[test]
    fn the_window_under_another_name_is_not_the_terminal() {
        let dir = Scratch::new("phone-terminal-window");
        let window = program(dir.join("app/Letronna"));
        // A disk that ignores case has it under this name already, and
        // refuses the link; on any other the link puts it there.
        let _ = std::fs::hard_link(&window, dir.join("app/letronna"));
        assert!(dir.join("app/letronna").is_file());
        // And the command on PATH is a link to it.
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::os::unix::fs::symlink(&window, dir.join("bin/letronna")).unwrap();
        assert_eq!(picked(&window, &dir), None);
        // A terminal further on is still found.
        let installed = program(dir.join("home/.local/bin/letronna"));
        assert_eq!(picked(&window, &dir), Some(installed));
    }

    #[test]
    fn the_terminal_inside_the_app_is_picked_first() {
        let dir = Scratch::new("phone-terminal-app");
        let window = program(dir.join("Letronna.app/Contents/MacOS/Letronna"));
        let inside = program(dir.join("Letronna.app/Contents/Helpers/letronna"));
        program(dir.join("home/.local/share/letronna/bin/letronna"));
        program(dir.join("home/.local/bin/letronna"));
        program(dir.join("bin/letronna"));
        assert_eq!(picked(&window, &dir), Some(inside));
    }

    #[test]
    fn with_none_inside_the_app_the_installed_terminal_is_picked() {
        let dir = Scratch::new("phone-terminal-installed");
        let window = program(dir.join("Letronna.app/Contents/MacOS/Letronna"));
        let installed = program(dir.join("home/.local/share/letronna/bin/letronna"));
        let command = program(dir.join("home/.local/bin/letronna"));
        let on_path = program(dir.join("bin/letronna"));
        assert_eq!(picked(&window, &dir), Some(installed.clone()));
        std::fs::remove_file(installed).unwrap();
        assert_eq!(picked(&window, &dir), Some(command.clone()));
        std::fs::remove_file(command).unwrap();
        assert_eq!(picked(&window, &dir), Some(on_path));
    }

    #[test]
    fn a_window_installed_as_the_command_picks_the_installed_terminal() {
        let dir = Scratch::new("phone-terminal-command");
        // Where the window puts itself on Linux, which is on PATH.
        let window = program(dir.join("home/.local/bin/letronna"));
        let installed = program(dir.join("home/.local/share/letronna/bin/letronna"));
        // Beside a folder that is no app's, `Helpers` is not looked in.
        program(dir.join("home/.local/Helpers/letronna"));
        let path = [dir.join("home/.local/bin")].into_iter();
        assert_eq!(
            pick(&window, Some(&dir.join("home")), path),
            Some(installed)
        );
    }

    #[test]
    fn a_copy_of_the_window_comes_after_the_installed_terminal() {
        let dir = Scratch::new("phone-terminal-copy");
        // As `letronna setup` leaves Linux: the window, the copy of itself
        // it then makes in `~/.local/bin`, and the terminal.
        let window = program(dir.join("home/.local/share/letronna/letronna-gui"));
        program(dir.join("home/.local/bin/letronna"));
        let installed = program(dir.join("home/.local/share/letronna/bin/letronna"));
        assert_eq!(picked(&window, &dir), Some(installed));
    }

    #[test]
    fn a_file_that_may_not_be_run_is_passed_over() {
        let dir = Scratch::new("phone-terminal-plain");
        let window = program(dir.join("Letronna.app/Contents/MacOS/Letronna"));
        let plain = dir.join("home/.local/share/letronna/bin/letronna");
        std::fs::create_dir_all(plain.parent().unwrap()).unwrap();
        std::fs::write(&plain, b"").unwrap();
        let on_path = program(dir.join("bin/letronna"));
        assert_eq!(picked(&window, &dir), Some(on_path));
    }

    #[test]
    fn a_folder_on_path_that_is_not_a_whole_path_is_not_looked_in() {
        let dir = Scratch::new("phone-terminal-relative");
        let window = program(dir.join("Letronna.app/Contents/MacOS/Letronna"));
        let terminal = program(dir.join("bin/letronna"));
        // The same folder, as a path from where the tests run.
        let here = std::env::current_dir().unwrap();
        let up: PathBuf = here.components().skip(1).map(|_| "..").collect();
        let relative = up.join(dir.join("bin").strip_prefix("/").unwrap());
        assert!(relative.join("letronna").is_file());
        assert_eq!(pick(&window, None, [relative].into_iter()), None);
        let whole = [dir.join("bin")].into_iter();
        assert_eq!(pick(&window, None, whole), Some(terminal));
    }

    #[test]
    fn with_no_terminal_anywhere_none_is_picked() {
        let dir = Scratch::new("phone-terminal-none");
        let window = program(dir.join("Letronna.app/Contents/MacOS/Letronna"));
        // A folder of that name is not a program.
        std::fs::create_dir_all(dir.join("home/.local/bin/letronna")).unwrap();
        assert_eq!(picked(&window, &dir), None);
    }
}
