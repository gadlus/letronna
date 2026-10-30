//! Keys the user moves: `keybindings.json` in the data folder, in Claude
//! Code's shape, binds a key to one of the box's actions, or with null
//! unbinds it. It is read at start and again once it changes, so an edit
//! takes at the next key with no restart, and what it gets wrong is said
//! once each time it is read rather than quietly skipped.
//!
//! A bound key is turned into its action's own key before the box sees
//! it, so an action does exactly what its key does and nothing is written
//! twice. `ACTIONS` is the table a new key's author extends.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A key the box answers to that another key can be bound to.
pub struct Action {
    /// Its name in the file, Claude Code's where Claude Code has the same.
    pub name: &'static str,
    /// The key the box knows it by.
    pub key: &'static str,
    pub what: &'static str,
}

pub const ACTIONS: &[Action] = &[
    Action {
        name: "history:search",
        key: "ctrl+r",
        what: "search what you sent",
    },
    Action {
        name: "history:previous",
        key: "up",
        what: "step back through what you sent",
    },
    Action {
        name: "history:next",
        key: "down",
        what: "step forward through what you sent",
    },
    Action {
        name: "chat:newline",
        key: "ctrl+j",
        what: "a new line",
    },
    Action {
        name: "chat:sendNow",
        key: "ctrl+s",
        what: "while it works: send what is queued now",
    },
    Action {
        name: "chat:cycleMode",
        key: "shift+tab",
        what: "switch mode",
    },
    Action {
        name: "chat:undo",
        key: "ctrl+_",
        what: "undo the last change to the line",
    },
    Action {
        name: "chat:yank",
        key: "ctrl+y",
        what: "put back what was cut",
    },
    Action {
        name: "chat:externalEditor",
        key: "ctrl+g",
        what: "write the line in your editor",
    },
    Action {
        name: "chat:imagePaste",
        key: "ctrl+v",
        what: "attach the picture on the clipboard",
    },
    Action {
        name: "chat:clearScreen",
        key: "ctrl+l",
        what: "a clean screen",
    },
    Action {
        name: "chat:lineStart",
        key: "ctrl+a",
        what: "go to the start of the line",
    },
    Action {
        name: "chat:lineEnd",
        key: "ctrl+e",
        what: "go to the end of the line",
    },
    Action {
        name: "chat:deleteToLineStart",
        key: "ctrl+u",
        what: "delete to the start of the line",
    },
    Action {
        name: "chat:deleteToLineEnd",
        key: "ctrl+k",
        what: "delete to the end of the line",
    },
    Action {
        name: "chat:deleteWordBack",
        key: "ctrl+w",
        what: "delete a word back",
    },
    Action {
        name: "chat:deleteWordForward",
        key: "alt+d",
        what: "delete a word ahead",
    },
    Action {
        name: "chat:wordLeft",
        key: "alt+b",
        what: "move a word back",
    },
    Action {
        name: "chat:wordRight",
        key: "alt+f",
        what: "move a word ahead",
    },
    Action {
        name: "app:toggleThinking",
        key: "ctrl+o",
        what: "open or fold the latest thinking",
    },
    Action {
        name: "scroll:pageUp",
        key: "pageup",
        what: "scroll back through the conversation",
    },
    Action {
        name: "scroll:pageDown",
        key: "pagedown",
        what: "scroll forward through the conversation",
    },
];

/// Claude Code's names for actions Letronna names otherwise, so a file
/// copied over keeps working.
const ALIASES: &[(&str, &str)] = &[
    ("chat:clearInput", "chat:clearScreen"),
    ("app:toggleTranscript", "app:toggleThinking"),
];

/// Keys the box keeps to itself: leaving, stopping, sending and the keys a
/// terminal sends for them.
const RESERVED: &[&str] = &[
    "ctrl+c",
    "ctrl+d",
    "enter",
    "ctrl+m",
    "esc",
    "ctrl+[",
    "tab",
    "ctrl+i",
    "backspace",
    "ctrl+h",
];

/// The contexts a block may name; the box is the one place keys go.
const CONTEXTS: [&str; 2] = ["Chat", "Global"];

/// How often a key looks at whether the file changed.
const LOOK: Duration = Duration::from_secs(1);

pub fn file() -> PathBuf {
    letronna_core::chat::data_dir().join("keybindings.json")
}

/// A key as a binding names it: what it is and which of ctrl, alt and
/// shift go with it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Key {
    code: KeyCode,
    modifiers: KeyModifiers,
}

impl Key {
    /// The key `event` is, as a binding would name it: shift goes with a
    /// capital letter and with shift-tab, and not with a symbol, whose
    /// shift is the keyboard's business.
    fn of(event: &KeyEvent) -> Key {
        let mut modifiers =
            event.modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        let code = match event.code {
            KeyCode::BackTab => {
                modifiers |= KeyModifiers::SHIFT;
                KeyCode::Tab
            }
            KeyCode::Char(c) if c.is_uppercase() => {
                modifiers |= KeyModifiers::SHIFT;
                KeyCode::Char(c.to_lowercase().next().unwrap_or(c))
            }
            KeyCode::Char(c) if !c.is_alphabetic() => {
                modifiers -= KeyModifiers::SHIFT;
                KeyCode::Char(c)
            }
            code => code,
        };
        Key { code, modifiers }
    }

    fn event(self) -> KeyEvent {
        KeyEvent::new(self.code, self.modifiers)
    }
}

/// The key `text` names, such as `ctrl+e`, `alt+p` or `shift+tab`.
fn parse(text: &str) -> Result<Key, String> {
    let text = text.trim().to_lowercase();
    if text.contains(' ') {
        return Err(format!(
            "\"{text}\": keys pressed one after another are not taken yet; bind one key"
        ));
    }
    // A key that is itself a plus, as in ctrl++.
    let (mods, name) = match text.strip_suffix("++") {
        Some(rest) => (rest, "+"),
        None => text.rsplit_once('+').unwrap_or(("", text.as_str())),
    };
    let mut modifiers = KeyModifiers::NONE;
    for word in mods.split('+').filter(|word| !word.is_empty()) {
        modifiers |= match word {
            "ctrl" | "control" => KeyModifiers::CONTROL,
            "alt" | "opt" | "option" | "meta" => KeyModifiers::ALT,
            "shift" => KeyModifiers::SHIFT,
            "cmd" | "command" | "super" | "win" => {
                return Err(format!(
                    "\"{text}\": most terminals never send {word}; use ctrl or alt"
                ));
            }
            _ => {
                return Err(format!(
                    "\"{text}\": {word} is no key that goes with others"
                ));
            }
        };
    }
    let code = match name {
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "space" => KeyCode::Char(' '),
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "backspace" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        _ => match (
            name.strip_prefix('f').map(str::parse::<u8>),
            name.chars().count(),
        ) {
            (Some(Ok(n)), _) if (1..=12).contains(&n) => KeyCode::F(n),
            (_, 1) => KeyCode::Char(name.chars().next().unwrap_or(' ')),
            _ => return Err(format!("\"{text}\": {name} is no key Letronna knows")),
        },
    };
    // The same key the terminal sends it as, as `of` makes it.
    Ok(Key::of(&KeyEvent::new(code, modifiers)))
}

/// What a bound key does.
#[derive(Clone, Debug, PartialEq)]
pub enum Bound {
    /// What this key of the box's does.
    Key(KeyEvent),
    /// Run the slash command of this name.
    Command(String),
}

/// One key the file moves: as it was written, and what it does now, if
/// anything.
struct Moved {
    written: String,
    to: Option<Bound>,
    what: String,
}

/// The keys the file moves, and when it was last looked at.
#[derive(Default)]
pub struct Keys {
    moved: HashMap<Key, Moved>,
    stamp: Option<SystemTime>,
    looked: Option<Instant>,
}

impl Keys {
    /// What `event` does under the file: `None` when the file leaves it
    /// alone, `Some(None)` when it unbinds it.
    pub fn bound(&self, event: &KeyEvent) -> Option<Option<&Bound>> {
        self.moved
            .get(&Key::of(event))
            .map(|moved| moved.to.as_ref())
    }

    /// The keys the file moves, by key, with what each does now, for
    /// `/help` and the keys card.
    pub fn listed(&self) -> Vec<(String, String)> {
        let mut rows: Vec<(String, String)> = (self.moved.values())
            .map(|moved| (moved.written.clone(), moved.what.clone()))
            .collect();
        rows.sort();
        rows
    }

    /// Read the file again, giving what it got wrong; an empty list when
    /// it is fine or not there.
    pub fn load(&mut self) -> Vec<String> {
        let path = file();
        self.looked = Some(Instant::now());
        self.stamp = modified(&path);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let (keys, said) = read(&text);
                self.moved = keys;
                said
            }
            Err(_) => {
                self.moved.clear();
                Vec::new()
            }
        }
    }

    /// Read the file again if it changed since it was read, at most once a
    /// `LOOK`: `None` when it did not.
    pub fn reread(&mut self) -> Option<Vec<String>> {
        // Never read before, as in a test's box, it is not looked for.
        if self.looked.is_none_or(|at| at.elapsed() < LOOK) {
            return None;
        }
        self.looked = Some(Instant::now());
        if modified(&file()) == self.stamp {
            return None;
        }
        Some(self.load())
    }
}

/// The keys `text` moves, as a test's box takes them.
#[cfg(test)]
pub fn from_text(text: &str) -> Keys {
    Keys {
        moved: read(text).0,
        ..Keys::default()
    }
}

fn modified(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

/// The keys the file `text` moves, and a sentence for each thing in it
/// that could not be taken.
fn read(text: &str) -> (HashMap<Key, Moved>, Vec<String>) {
    let mut moved = HashMap::new();
    let mut said = Vec::new();
    let json: serde_json::Value = match serde_json::from_str(text) {
        Ok(json) => json,
        Err(error) => {
            said.push(format!(
                "keybindings.json is not JSON Letronna can read ({error}), so no key is moved."
            ));
            return (moved, said);
        }
    };
    let Some(blocks) = json["bindings"].as_array() else {
        said.push("keybindings.json has no \"bindings\" list, so no key is moved.".to_owned());
        return (moved, said);
    };
    for block in blocks {
        let context = block["context"].as_str().unwrap_or_default();
        if !CONTEXTS.contains(&context) {
            said.push(format!(
                "keybindings.json: the terminal has only the Chat and Global contexts, so the \
                 keys under \"{context}\" are left as they were."
            ));
            continue;
        }
        let Some(pairs) = block["bindings"].as_object() else {
            continue;
        };
        for (written, action) in pairs {
            match take(written, action) {
                Ok((key, entry)) => {
                    if moved.insert(key, entry).is_some() {
                        said.push(format!(
                            "keybindings.json binds {written} twice; the last is kept."
                        ));
                    }
                }
                Err(why) => said.push(format!("keybindings.json: {why}")),
            }
        }
    }
    (moved, said)
}

/// The key `written` and what `action` makes it do, or why it cannot.
fn take(written: &str, action: &serde_json::Value) -> Result<(Key, Moved), String> {
    let key = parse(written)?;
    let reserved = RESERVED
        .iter()
        .any(|name| parse(name).is_ok_and(|own| own == key));
    if reserved {
        return Err(format!("{written} is Letronna's own and stays as it is."));
    }
    let typed = matches!(key.code, KeyCode::Char(_))
        && !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    if typed {
        return Err(format!(
            "{written} types a letter in the box; bind it with ctrl or alt."
        ));
    }
    let written = written.trim().to_lowercase();
    let (to, what) = match action {
        serde_json::Value::Null => (None, "does nothing now".to_owned()),
        serde_json::Value::String(name) => {
            if let Some(command) = name.strip_prefix("command:") {
                let command = command.trim_start_matches('/');
                (
                    Some(Bound::Command(command.to_owned())),
                    format!("runs /{command}"),
                )
            } else {
                let name = ALIASES
                    .iter()
                    .find(|(alias, _)| alias == name)
                    .map_or(name.as_str(), |(_, own)| own);
                let Some(found) = ACTIONS.iter().find(|action| action.name == name) else {
                    return Err(format!(
                        "{name} is no action here; /keybindings lists the ones there are."
                    ));
                };
                let own = parse(found.key)?;
                (Some(Bound::Key(own.event())), found.what.to_owned())
            }
        }
        _ => {
            return Err(format!(
                "{written} needs an action's name, or null to unbind it."
            ));
        }
    };
    Ok((key, Moved { written, to, what }))
}

/// The file `/keybindings` makes when there is none: an empty block to
/// write in, and every action with its key, to copy names from.
pub fn template() -> String {
    let actions: serde_json::Map<String, serde_json::Value> = ACTIONS
        .iter()
        .map(|action| (action.name.to_owned(), action.key.into()))
        .collect();
    let json = serde_json::json!({
        "$actions": actions,
        "$help": "Bind a key to an action, such as \"ctrl+x\": \"chat:sendNow\", or to \
                  \"command:compact\" to run /compact; null unbinds a key. The actions \
                  above are listed with the key they have now.",
        "bindings": [{ "context": "Chat", "bindings": {} }],
    });
    serde_json::to_string_pretty(&json).unwrap_or_default() + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn a_bound_key_becomes_its_actions_key_and_null_unbinds() {
        let (moved, said) = read(
            r#"{"bindings": [{"context": "Chat", "bindings": {
                "ctrl+x": "chat:sendNow", "alt+e": "chat:externalEditor",
                "ctrl+g": null, "ctrl+t": "command:compact",
                "ctrl+n": "app:toggleTranscript"}}]}"#,
        );
        assert!(said.is_empty(), "{said:?}");
        let keys = Keys {
            moved,
            ..Keys::default()
        };
        let ctrl = |c| key(KeyCode::Char(c), KeyModifiers::CONTROL);
        assert_eq!(keys.bound(&ctrl('x')), Some(Some(&Bound::Key(ctrl('s')))));
        assert_eq!(
            keys.bound(&key(KeyCode::Char('e'), KeyModifiers::ALT)),
            Some(Some(&Bound::Key(ctrl('g'))))
        );
        assert_eq!(keys.bound(&ctrl('g')), Some(None));
        assert_eq!(
            keys.bound(&ctrl('t')),
            Some(Some(&Bound::Command("compact".into())))
        );
        // Claude Code's name for the same key.
        assert_eq!(keys.bound(&ctrl('n')), Some(Some(&Bound::Key(ctrl('o')))));
        assert_eq!(keys.bound(&ctrl('r')), None, "a key the file leaves alone");
        assert!(
            keys.listed()
                .contains(&("ctrl+g".into(), "does nothing now".into()))
        );
    }

    #[test]
    fn what_cannot_be_taken_is_said_once_each() {
        let (moved, said) = read(
            r#"{"bindings": [
                {"context": "Chat", "bindings": {
                    "ctrl+c": "chat:undo", "x": "chat:undo", "ctrl+q": "chat:fly",
                    "ctrl+x ctrl+s": "chat:sendNow", "cmd+k": "chat:clearScreen",
                    "ctrl+z": 3}},
                {"context": "Chat", "bindings": {"ctrl+y": "chat:undo"}},
                {"context": "Chat", "bindings": {"ctrl+y": "chat:yank"}},
                {"context": "Transcript", "bindings": {"q": null}}]}"#,
        );
        assert_eq!(moved.len(), 1, "only ctrl+y is taken");
        let said = said.join("\n");
        for word in [
            "ctrl+c is Letronna's own",
            "x types a letter",
            "chat:fly is no action",
            "one after another",
            "never send cmd",
            "ctrl+z needs an action",
            "binds ctrl+y twice",
            "\"Transcript\" are left",
        ] {
            assert!(said.contains(word), "{word} not in:\n{said}");
        }
        let (moved, said) = read("{ not json");
        assert!(moved.is_empty() && said[0].contains("not JSON"));
    }

    #[test]
    fn keys_are_named_as_the_terminal_sends_them() {
        assert_eq!(
            parse("Shift+Tab"),
            Ok(Key::of(&key(KeyCode::BackTab, KeyModifiers::SHIFT)))
        );
        assert_eq!(
            parse("ctrl+shift+k"),
            Ok(Key::of(&key(
                KeyCode::Char('K'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )))
        );
        // A symbol's shift is the keyboard's: ctrl+_ is ctrl and _.
        assert_eq!(
            parse("ctrl+_"),
            Ok(Key::of(&key(
                KeyCode::Char('_'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )))
        );
        assert_eq!(
            parse("meta+p"),
            Ok(Key::of(&key(KeyCode::Char('p'), KeyModifiers::ALT)))
        );
        assert_eq!(parse("f5").map(|key| key.code), Ok(KeyCode::F(5)));
    }

    #[test]
    fn every_action_names_a_key_the_box_knows_and_the_template_lists_them() {
        let template = template();
        for action in ACTIONS {
            assert!(parse(action.key).is_ok(), "{}", action.key);
            assert!(template.contains(action.name), "{}", action.name);
        }
        let (_, said) = read(&template);
        assert!(said.is_empty(), "{said:?}");
    }
}
