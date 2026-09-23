//! The input line.
//!
//! Typing `/` opens a list of commands under the prompt that filters as you
//! type, so the commands are discoverable instead of something you have to
//! already know. That needs keystrokes as they happen, which means raw mode --
//! but only while a line is being edited. Everything else still prints into
//! ordinary scrollback.
//!
//! If raw mode is not available, because output is piped or redirected, this
//! falls back to reading a whole line. The menu is a convenience, not the only
//! way in: every command can still be typed out and entered.

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal;
use std::io::Write;

use crate::render;

pub struct Command {
    pub name: &'static str,
    pub blurb: &'static str,
}

pub const COMMANDS: &[Command] = &[
    Command {
        name: "/help",
        blurb: "what Letronna can do here",
    },
    Command {
        name: "/new",
        blurb: "start a fresh session",
    },
    Command {
        name: "/plan",
        blurb: "read and search only, never write or run",
    },
    Command {
        name: "/model",
        blurb: "show the model and gateway in use",
    },
    Command {
        name: "/where",
        blurb: "show the project folder and session file",
    },
    Command {
        name: "/clear",
        blurb: "clear the screen",
    },
    Command {
        name: "/exit",
        blurb: "leave",
    },
];

fn matches(text: &str) -> Vec<&'static Command> {
    if !text.starts_with('/') {
        return Vec::new();
    }
    COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(text))
        .collect()
}

/// Read one line. `None` means the user is done: Ctrl-C, Ctrl-D, or EOF.
pub fn read(prompt: &str) -> Option<String> {
    if !render::interactive() || terminal::enable_raw_mode().is_err() {
        return crate::input::read(prompt);
    }
    let result = edit(prompt);
    let _ = terminal::disable_raw_mode();
    // The line was drawn by hand in raw mode; close it off properly.
    println!();
    result
}

fn edit(prompt: &str) -> Option<String> {
    let mut text = String::new();
    let mut picked = 0usize;
    let mut drawn = 0u16;
    draw(prompt, &text, picked, &mut drawn);

    loop {
        let Ok(Event::Key(KeyEvent {
            code,
            modifiers,
            kind,
            ..
        })) = event::read()
        else {
            continue;
        };
        // Windows reports press and release; act on one of them.
        if kind != KeyEventKind::Press {
            continue;
        }
        let options = matches(&text);
        match (code, modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                clear_menu(drawn);
                return None;
            }
            (KeyCode::Char('d'), KeyModifiers::CONTROL) if text.is_empty() => {
                clear_menu(drawn);
                return None;
            }
            (KeyCode::Enter, _) => {
                // A highlighted command wins; otherwise take the text as typed.
                if let Some(choice) = options.get(picked).filter(|_| !options.is_empty()) {
                    text = choice.name.to_owned();
                }
                // Leave the line standing rather than wiping it: what you
                // asked has to stay above the answer, or the transcript reads
                // as the agent talking to itself.
                keep(prompt, &text, drawn);
                return Some(text);
            }
            (KeyCode::Tab, _) => {
                if let Some(choice) = options.get(picked) {
                    text = choice.name.to_owned();
                    picked = 0;
                }
            }
            (KeyCode::Up, _) if !options.is_empty() => {
                picked = picked.checked_sub(1).unwrap_or(options.len() - 1)
            }
            (KeyCode::Down, _) if !options.is_empty() => picked = (picked + 1) % options.len(),
            (KeyCode::Esc, _) => {
                text.clear();
                picked = 0;
            }
            (KeyCode::Backspace, _) => {
                text.pop();
                picked = 0;
            }
            (KeyCode::Char(ch), _) => {
                text.push(ch);
                picked = 0;
            }
            _ => continue,
        }
        draw(prompt, &text, picked, &mut drawn);
    }
}

/// Redraw the prompt and, when the text starts with `/`, the matching
/// commands under it. `drawn` carries how many lines the menu took last time
/// so they can be wiped before the next draw.
fn draw(prompt: &str, text: &str, picked: usize, drawn: &mut u16) {
    let mut out = std::io::stdout().lock();
    clear_menu_on(&mut out, *drawn);
    let _ = write!(out, "\r\x1b[2K{prompt}{text}");

    let options = matches(text);
    let shown = options.len().min(8) as u16;
    if shown > 0 {
        for (i, option) in options.iter().take(shown as usize).enumerate() {
            let marker = if i == picked { "›" } else { " " };
            let name = if i == picked {
                render::blue(option.name)
            } else {
                option.name.to_owned()
            };
            let _ = write!(
                out,
                "\r\n\x1b[2K  {marker} {name}  {}",
                render::dim(option.blurb)
            );
        }
        // Back up to the prompt line and put the cursor after the text.
        let _ = write!(out, "\x1b[{shown}A\r\x1b[{}C", visible(prompt) + text.len());
    }
    *drawn = shown;
    let _ = out.flush();
}

/// Wipe the menu but keep the line as typed, marked as ours so it is easy to
/// pick out when scrolling back.
fn keep(prompt: &str, text: &str, drawn: u16) {
    let mut out = std::io::stdout().lock();
    clear_menu_on(&mut out, drawn);
    let _ = write!(out, "\r\x1b[2K{prompt}{}", render::bold(text));
    let _ = out.flush();
}

fn clear_menu(drawn: u16) {
    let mut out = std::io::stdout().lock();
    clear_menu_on(&mut out, drawn);
    let _ = out.flush();
}

fn clear_menu_on(out: &mut impl Write, drawn: u16) {
    for _ in 0..drawn {
        let _ = write!(out, "\r\n\x1b[2K");
    }
    if drawn > 0 {
        let _ = write!(out, "\x1b[{drawn}A");
    }
    let _ = write!(out, "\r\x1b[2K");
}

/// Width of the prompt as printed, ignoring the escape codes that colour it.
fn visible(text: &str) -> usize {
    let mut width = 0;
    let mut in_escape = false;
    for ch in text.chars() {
        if in_escape {
            in_escape = ch != 'm';
        } else if ch == '\x1b' {
            in_escape = true;
        } else {
            width += 1;
        }
    }
    width
}

#[cfg(test)]
mod tests {
    use super::{matches, visible};

    #[test]
    fn the_menu_only_opens_for_a_slash() {
        assert!(matches("what is this").is_empty());
        assert!(matches("").is_empty());
        assert!(!matches("/").is_empty());
    }

    #[test]
    fn typing_narrows_the_list() {
        let all = matches("/").len();
        let e = matches("/e");
        assert!(e.len() < all && !e.is_empty());
        assert!(e.iter().all(|c| c.name.starts_with("/e")));
        assert_eq!(matches("/exit").len(), 1);
        // A command that does not exist offers nothing rather than guessing.
        assert!(matches("/nonsense").is_empty());
    }

    #[test]
    fn prompt_width_ignores_colour() {
        assert_eq!(visible("> "), 2);
        assert_eq!(visible("\x1b[38;5;69m>\x1b[0m "), 2);
    }
}
