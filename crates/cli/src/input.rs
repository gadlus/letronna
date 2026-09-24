//! Asking things without the chat box: a line, a hidden line, a menu.
//!
//! Setup runs like this, printed straight into the terminal the way an
//! installer is, rather than inside the conversation screen -- installing is
//! not talking to it yet. Anything piped in (a script, a test) gets plain
//! numbered prompts instead, because there is no keyboard to steer with.

use std::io::{BufRead, IsTerminal, Write};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal;

use crate::render;

/// Print a prompt and read one line. `None` at end of input, so Ctrl-D and a
/// closed pipe both end the session rather than spinning.
pub fn read(prompt: &str) -> Option<String> {
    let mut out = std::io::stdout();
    let _ = write!(out, "{prompt}");
    let _ = out.flush();
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim_end_matches(['\n', '\r']).to_owned()),
    }
}

/// Whether there is a keyboard to steer with.
fn keyboard() -> bool {
    std::io::stdin().is_terminal() && render::colour()
}

/// Read a line that is not shown: a dot for each character. For keys, which
/// otherwise sit on the screen and in the scrollback after they are typed.
pub fn secret(prompt: &str) -> Option<String> {
    if !keyboard() || terminal::enable_raw_mode().is_err() {
        return read(prompt);
    }
    let mut out = std::io::stdout();
    let _ = write!(out, "{prompt}");
    let _ = out.flush();
    let mut typed = String::new();
    let answer = loop {
        let Ok(Event::Key(key)) = event::read() else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Enter => break Some(typed),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break None,
            KeyCode::Backspace => {
                if typed.pop().is_some() {
                    let _ = write!(out, "\x08 \x08");
                }
            }
            KeyCode::Char(ch) => {
                typed.push(ch);
                let _ = write!(out, "•");
            }
            _ => {}
        }
        let _ = out.flush();
    };
    let _ = terminal::disable_raw_mode();
    println!();
    answer
}

/// A list steered with the arrow keys, drawn in place under the question.
/// Enter takes the highlighted one, a digit takes that one outright, and
/// escape or Ctrl-C leaves without choosing.
pub fn menu(options: &[&str], start: usize) -> Option<usize> {
    if options.is_empty() {
        return None;
    }
    if !keyboard() || terminal::enable_raw_mode().is_err() {
        for (i, option) in options.iter().enumerate() {
            render::line(&render::dim(&format!("  {}. {option}", i + 1)));
        }
        let typed = read(&format!("{} ", render::blue("›")))?;
        return typed.trim().parse::<usize>().ok()?.checked_sub(1);
    }

    let mut out = std::io::stdout();
    let mut at = start.min(options.len() - 1);
    // The cursor is hidden while the list is up; a blinking block beside the
    // highlight only competes with it.
    let _ = write!(out, "\x1b[?25l");
    draw(&mut out, options, at, false);
    let answer = loop {
        let Ok(Event::Key(key)) = event::read() else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => at = at.checked_sub(1).unwrap_or(options.len() - 1),
            KeyCode::Down | KeyCode::Char('j') => at = (at + 1) % options.len(),
            KeyCode::Enter => break Some(at),
            KeyCode::Esc => break None,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break None,
            KeyCode::Char(digit) if digit.is_ascii_digit() => {
                match digit.to_digit(10).and_then(|d| (d as usize).checked_sub(1)) {
                    Some(index) if index < options.len() => break Some(index),
                    _ => continue,
                }
            }
            _ => continue,
        }
        draw(&mut out, options, at, true);
    };
    let _ = terminal::disable_raw_mode();
    // Leave only the answer behind, so the scrollback reads as a record of
    // what was chosen rather than a menu nobody needs to see again.
    let _ = write!(out, "\x1b[{}A\r\x1b[0J\x1b[?25h", options.len());
    let _ = out.flush();
    if let Some(index) = answer {
        render::line(&format!(
            "{} {}",
            render::blue("›"),
            render::bold(options[index])
        ));
    }
    answer
}

/// Draw the list, over the last drawing of it when there was one.
fn draw(out: &mut impl Write, options: &[&str], at: usize, again: bool) {
    if again {
        let _ = write!(out, "\x1b[{}A\r", options.len());
    }
    for (i, option) in options.iter().enumerate() {
        let row = if i == at {
            format!(
                "{}{} {}",
                render::PAD,
                render::blue("›"),
                render::blue(option)
            )
        } else {
            format!("{}  {}", render::PAD, render::dim(option))
        };
        let _ = write!(out, "\x1b[2K{row}\r\n");
    }
    let _ = out.flush();
}
