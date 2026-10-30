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
                            "Letronna's terminal program, which answers the phone, was not \
                             found beside the window or on PATH."
                                .to_owned()
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

/// The terminal program: this one when it is the terminal, as under
/// `letronna serve`; otherwise the one installed beside the window, or on
/// PATH.
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
    if name(&me) == "letronna" {
        return Some(me);
    }
    let home = crate::tools::home().map(|home| home.join(".local/bin/letronna"));
    [dir.join("letronna")]
        .into_iter()
        .chain(home)
        .chain(on_path("letronna"))
        .find(|path| path.is_file())
}

fn on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

#[cfg(all(test, unix))]
mod tests {
    use super::Agent;
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
}
