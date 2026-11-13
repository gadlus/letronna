//! `letronna serve`: the chat apps and the schedules, with no window.
//!
//! Core's headless host does the work; this holds it up in a terminal. It
//! does not start while another Letronna hears the chat apps, since only
//! one may, prints a line for each thing that happens, and at a Ctrl-C
//! stops every run and waits for each to end, so what a run had said is in
//! its session and its chat hears "Stopped." A second Ctrl-C leaves at once.
//!
//! The Ctrl-C is read as a key, with the terminal in raw mode, which is the
//! same on every system and needs no signal handler. Started from a script
//! or a service with no terminal, it is ended as any process is, and a run
//! then going is cut off where it was.

use crate::render;
use letronna_core::headless::{self, Headless, Input};
use letronna_core::{browser, messaging, schedule};
use std::io::IsTerminal;
use std::sync::atomic::Ordering;

/// Serve until the runs a Ctrl-C stopped have ended; the exit code.
pub fn run() -> i32 {
    let config = messaging::load();
    let workspace = std::env::current_dir().ok();
    let (tell, sending) = headless::chat_apps(&config);
    let say = Box::new(|line: String| render::line(&line));
    let (host, inputs, taken) = Headless::new(config.clone(), workspace, tell, say);
    let listening = match headless::hear(&config, inputs.clone()) {
        Ok(listening) => listening,
        Err(error) => {
            render::line(&render::red(&error));
            return 1;
        }
    };
    headless::answer_cards(inputs.clone());
    // Unless the window already runs them, as it does while it is open.
    let (_schedules, events) = schedule::Runner::start();
    headless::hear_schedules(events, inputs.clone());
    let keys = std::io::stdin().is_terminal() && std::io::stdout().is_terminal() && foreground();
    let raw = keys && crossterm::terminal::enable_raw_mode().is_ok();
    render::set_raw(raw);
    render::line(&render::bold(&opening(&config)));
    if raw {
        render::line(&render::dim("Ctrl-C stops the runs and leaves."));
        on_ctrl_c(inputs);
    }
    host.serve(taken);
    if let Some(stop) = listening {
        stop.store(true, Ordering::Relaxed);
    }
    // The host is gone with its `Tell`, so this ends once the chats have
    // heard the last of it, "Stopped." included.
    let _ = sending.join();
    // A browser with no window would outlive this unseen.
    browser::close_hidden();
    render::line(&render::dim("Stopped."));
    leave_raw(raw);
    0
}

/// What this Letronna serves, said as it starts.
fn opening(config: &messaging::Config) -> String {
    let apps: Vec<&str> = [
        (config.telegram.ready(), "Telegram"),
        (config.discord.hears(), "Discord"),
    ]
    .into_iter()
    .filter_map(|(ready, app)| ready.then_some(app))
    .collect();
    match apps.as_slice() {
        [] => "No chat app is set up, so only the schedules run here.".into(),
        apps => format!(
            "Answering {} and running the schedules.",
            apps.join(" and ")
        ),
    }
}

/// Quit at the first Ctrl-C, once every run has ended; leave at once at the
/// second.
fn on_ctrl_c(inputs: async_channel::Sender<Input>) {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
    std::thread::spawn(move || {
        let mut quitting = false;
        loop {
            let key = match event::read() {
                Ok(Event::Key(key)) => key,
                Ok(_) => continue,
                // The terminal is gone, and no key will come again.
                Err(_) => return,
            };
            let ctrl_c =
                key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
            if key.kind != KeyEventKind::Press || !ctrl_c {
                continue;
            }
            if quitting {
                leave_raw(true);
                std::process::exit(130);
            }
            quitting = true;
            render::line(&render::dim(
                "Stopping the runs. Ctrl-C again to leave at once.",
            ));
            if inputs.send_blocking(Input::Quit).is_err() {
                return;
            }
        }
    });
}

/// Whether this runs in its terminal's foreground, where the keys come to
/// it. Started with `&` it does not, and raw mode taken from there would
/// stop it, or leave the shell's own terminal raw.
#[cfg(unix)]
fn foreground() -> bool {
    // SAFETY: both only ask about this process and its terminal.
    unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) == libc::getpgrp() }
}

/// A Windows console has no background, so what runs in it has its keys.
#[cfg(not(unix))]
fn foreground() -> bool {
    true
}

/// Give the terminal back as it was.
fn leave_raw(raw: bool) {
    if raw {
        let _ = crossterm::terminal::disable_raw_mode();
        render::set_raw(false);
    }
}
