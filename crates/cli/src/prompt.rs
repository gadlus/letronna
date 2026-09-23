//! The input box.
//!
//! What you type sits in a box under the conversation, with the model it will
//! go to named underneath. Output is printed above the box, so the box stays
//! where your eye already is. Typing `/` opens a list of commands under it
//! that filters as you type, so the commands are discoverable instead of
//! something you have to already know.
//!
//! That needs keystrokes as they happen, which means raw mode -- but only
//! while a line is being edited. Everything else still prints into ordinary
//! scrollback, so the conversation is still there after you quit and mouse
//! selection keeps working.
//!
//! If raw mode is not available, because output is piped or redirected, this
//! falls back to reading a whole line. The box is a convenience, not the only
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

/// Commands the menu will show at once, so a long list cannot push the box
/// off the top of a short window.
const MENU_LIMIT: usize = 8;

fn matches(text: &str) -> Vec<&'static Command> {
    if !text.starts_with('/') {
        return Vec::new();
    }
    COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(text))
        .collect()
}

/// Read one line. `footer` is the standing note under the box, which is where
/// the model belongs: it answers "who am I talking to" without taking a line
/// of the conversation every turn. `None` means the user is done: Ctrl-C,
/// Ctrl-D, or EOF.
pub fn read(prompt: &str, footer: &str) -> Option<String> {
    if !render::interactive() || terminal::enable_raw_mode().is_err() {
        return crate::input::read(prompt);
    }
    let result = edit(prompt, footer);
    let _ = terminal::disable_raw_mode();
    // The line was drawn by hand in raw mode; close it off properly.
    println!();
    result
}

/// Room inside the box for the prompt and what is typed after it.
fn room() -> usize {
    // Clamped at both ends: a hundred columns of empty box looks silly on a
    // wide screen, and a very narrow one still needs somewhere to type.
    render::width().clamp(28, 100) - render::PAD.len() * 2 - 4
}

/// The tail of `text` that fits. A long line keeps its end -- the part being
/// typed -- in view, rather than its beginning.
fn fit(text: &str, room: usize) -> String {
    let count = text.chars().count();
    if count <= room {
        return text.to_owned();
    }
    text.chars().skip(count - room).collect()
}

fn edit(prompt: &str, footer: &str) -> Option<String> {
    let mut text = String::new();
    let mut picked = 0usize;
    let mut drawn = false;
    draw(prompt, footer, &text, picked, &mut drawn);

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
                wipe();
                return None;
            }
            (KeyCode::Char('d'), KeyModifiers::CONTROL) if text.is_empty() => {
                wipe();
                return None;
            }
            (KeyCode::Enter, _) => {
                // A highlighted command wins; otherwise take the text as typed.
                if let Some(choice) = options.get(picked).filter(|_| !options.is_empty()) {
                    text = choice.name.to_owned();
                }
                keep(prompt, &text);
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
        draw(prompt, footer, &text, picked, &mut drawn);
    }
}

/// Redraw the box, the matching commands when the text starts with `/`, and
/// the footer under them.
///
/// The cursor rests on the line inside the box, so every redraw starts by
/// going back up to the top edge and clearing to the bottom of the screen.
/// Erasing wholesale rather than counting the lines the menu took last time
/// is both shorter and harder to get wrong.
fn draw(prompt: &str, footer: &str, text: &str, picked: usize, drawn: &mut bool) {
    let mut out = std::io::stdout().lock();
    let room = room();
    let shown = fit(text, room);
    let typed = visible(prompt) + shown.chars().count();

    to_top(&mut out, *drawn);

    let edge = "─".repeat(room + 2);
    let _ = write!(
        out,
        "{}{}\r\n",
        render::PAD,
        render::dim(&format!("╭{edge}╮"))
    );
    let _ = write!(
        out,
        "{}{} {prompt}{}{} {}\r\n",
        render::PAD,
        render::dim("│"),
        shown,
        " ".repeat(room.saturating_sub(typed)),
        render::dim("│")
    );
    let _ = write!(out, "{}{}", render::PAD, render::dim(&format!("╰{edge}╯")));

    let options = matches(text);
    let listed = options.len().min(MENU_LIMIT);
    for (i, option) in options.iter().take(listed).enumerate() {
        let marker = if i == picked { "›" } else { " " };
        let name = if i == picked {
            render::blue(option.name)
        } else {
            option.name.to_owned()
        };
        let _ = write!(
            out,
            "\r\n{}{marker} {name}  {}",
            render::PAD,
            render::dim(option.blurb)
        );
    }
    let _ = write!(out, "\r\n{}  {}", render::PAD, render::dim(footer));

    // Back up to the line inside the box and sit after what has been typed.
    let up = 2 + listed as u16;
    let column = render::PAD.len() + 2 + typed;
    let _ = write!(out, "\x1b[{up}A\r\x1b[{column}C");
    *drawn = true;
    let _ = out.flush();
}

/// Put the cursor at the start of the box's top edge and clear everything
/// from there down.
fn to_top(out: &mut impl Write, drawn: bool) {
    if drawn {
        let _ = write!(out, "\r\x1b[1A");
    }
    let _ = write!(out, "\r\x1b[0J");
}

/// Take the box away and leave the line as typed, marked as ours so it is
/// easy to pick out when scrolling back.
fn keep(prompt: &str, text: &str) {
    let mut out = std::io::stdout().lock();
    to_top(&mut out, true);
    let _ = write!(out, "{}{prompt}{}", render::PAD, render::bold(text));
    let _ = out.flush();
}

/// Take the box away and leave nothing behind, for a line that was abandoned.
fn wipe() {
    let mut out = std::io::stdout().lock();
    to_top(&mut out, true);
    let _ = out.flush();
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
    use super::{fit, matches, visible};

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

    #[test]
    fn a_long_line_keeps_the_end_in_view() {
        assert_eq!(fit("hello", 10), "hello");
        assert_eq!(fit("hello", 5), "hello");
        // What was typed last is what you are looking at.
        assert_eq!(fit("abcdefghij", 4), "ghij");
        // Counted in characters, so the box does not split one in half.
        assert_eq!(fit("héllo wörld", 5), "wörld");
    }
}
