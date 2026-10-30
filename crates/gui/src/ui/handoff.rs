//! Carrying a session between the window and the terminal. Continue in
//! terminal opens the user's own terminal on the session on screen, with
//! `letronna --resume <id>`. The other way, the window opens the session it
//! is started with, `--session <id>`; one already running is asked through
//! `open-session` in the data folder, which a second start writes before it
//! hands over and which the window takes and removes.

use crate::prelude::*;
use std::time::Duration;

actions!(handoff, [ContinueInTerminal]);

/// The file in the data folder a session to open is asked for in.
const ASKED: &str = "open-session";

/// How old an ask may be and still be taken: one a start left behind that
/// no window took is not acted on the next day.
const FRESH: Duration = Duration::from_secs(30);

/// The session named after `--session` in `args`, or with `--session=`.
fn named(mut args: impl Iterator<Item = String>) -> Option<u64> {
    while let Some(arg) = args.next() {
        if let Some(id) = arg.strip_prefix("--session=") {
            return id.parse().ok();
        }
        if arg == "--session" {
            return args.next()?.parse().ok();
        }
    }
    None
}

/// The session this start of the window was asked to open.
pub(crate) fn session_flag() -> Option<u64> {
    named(std::env::args().skip(1))
}

/// Ask the window that runs, or the copy that will, to open session `id`.
pub(crate) fn ask_for(id: u64) {
    let folder = backend::data_dir();
    let _ = std::fs::create_dir_all(&folder);
    let _ = std::fs::write(folder.join(ASKED), id.to_string());
}

/// The session asked for in the file, which is taken, when it was asked
/// lately.
pub(crate) fn take_asked() -> Option<u64> {
    let path = backend::data_dir().join(ASKED);
    let age = std::fs::metadata(&path).ok()?.modified().ok()?.elapsed();
    let text = std::fs::read_to_string(&path).ok();
    let _ = std::fs::remove_file(&path);
    if !age.is_ok_and(|age| age < FRESH) {
        return None;
    }
    text?.trim().parse().ok()
}

impl Chat {
    /// Open the user's terminal on the session on screen, in its folder,
    /// once nothing runs in it here.
    pub(crate) fn continue_in_terminal(&mut self, cx: &mut Context<Self>) {
        let open = &self.store.conversations[self.store.active];
        let (id, folder) = (open.id, open.workspace.clone());
        if (self.busy && self.streaming_id == Some(id)) || self.runs.contains_key(&id) {
            self.error = Some(
                "A reply is under way here. Carry the session on in the terminal once it is done."
                    .into(),
            );
            return cx.notify();
        }
        if open.length() == 0 {
            self.note =
                Some("Nothing has been said here yet: start in the terminal with letronna.".into());
            return cx.notify();
        }
        // On disk before the terminal reads it.
        self.save();
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from);
        let folder = folder.or(home).unwrap_or_default();
        let args = ["--resume".to_owned(), id.to_string()];
        match letronna_core::jobs::open_terminal_running(&folder, "letronna", &args) {
            Ok(()) => {
                self.note = Some(format!(
                    "Opened the terminal on this session: letronna --resume {id}."
                ))
            }
            Err(why) => self.error = Some(why),
        }
        cx.notify();
    }

    /// Show session `id` in the chat, as the terminal asked.
    fn open_asked(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(index) = self.store.index_of(id) else {
            self.error = Some(format!(
                "The terminal asked for session {id}, which is not here."
            ));
            return cx.notify();
        };
        self.leave_for(index);
        self.store.active = index;
        self.page = Page::Chat;
        self.follow();
        self.save();
        cx.notify();
    }

    /// Open the session the terminal or a second start asks for in the file,
    /// whenever it is written, and bring the window forward for it.
    pub(crate) fn watch_asked(cx: &mut Context<Self>) {
        use ::notify::Watcher as _;
        let folder = backend::data_dir();
        let _ = std::fs::create_dir_all(&folder);
        let (tx, rx) = async_channel::bounded(1);
        let watcher =
            ::notify::recommended_watcher(move |event: ::notify::Result<::notify::Event>| {
                let ours = event.is_ok_and(|event| {
                    (event.paths.iter()).any(|path| path.file_name().is_some_and(|n| n == ASKED))
                });
                if ours {
                    let _ = tx.try_send(());
                }
            });
        let Ok(mut watcher) = watcher else {
            return;
        };
        if watcher
            .watch(&folder, ::notify::RecursiveMode::NonRecursive)
            .is_err()
        {
            return;
        }
        cx.spawn(async move |this, cx| {
            let _watcher = watcher;
            while rx.recv().await.is_ok() {
                let Some(id) = take_asked() else {
                    continue;
                };
                let opened = this.update(cx, |this, cx| this.open_asked(id, cx));
                if opened.is_err() {
                    break;
                }
                cx.update(crate::platform::tray::show_main_window);
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::named;

    fn args(words: &[&str]) -> impl Iterator<Item = String> {
        words
            .iter()
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn the_window_opens_the_session_it_is_started_with() {
        assert_eq!(named(args(&["--session", "137"])), Some(137));
        assert_eq!(named(args(&["--relaunch", "--session=12"])), Some(12));
        assert_eq!(named(args(&["--session"])), None);
        assert_eq!(named(args(&["--session", "last"])), None);
        assert_eq!(named(args(&["--relaunch"])), None);
    }
}
