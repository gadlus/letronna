//! Letronna's own tools in families, as the window's Capabilities › Tools
//! lists them: what each family is for, whether it can work yet, and a
//! switch for the lot.
//!
//! A family is switched off as permission rules, one deny rule on every call
//! of each of its tools (see `permissions::switch`), rather than in the
//! settings: rules have a file of their own that the window and the
//! terminal both add to without one writing over the other's, and the gate
//! every session and helper goes through already reads it. The tools that
//! keep a turn going, such as asking the user or keeping a to-do list,
//! belong to no family and cannot be switched off here.

use crate::permissions;
use std::collections::BTreeMap;

/// A group of tools that work together and are switched on and off as one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Files,
    Commands,
    Web,
    Browser,
    Media,
    Messaging,
    Plugins,
}

/// Every family, in the order they are listed.
pub const FAMILIES: [Family; 7] = [
    Family::Files,
    Family::Commands,
    Family::Web,
    Family::Browser,
    Family::Media,
    Family::Messaging,
    Family::Plugins,
];

/// Where the user goes to make a family ready.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fix {
    /// Settings › Search, for a search service's key.
    Search,
    /// Settings › Account, to sign in.
    Account,
    /// The Messaging page, to connect a chat app.
    Messaging,
    /// Settings › Plugins, to switch one on.
    Plugins,
}

/// Why a family cannot do all it is for yet, and where that is put right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Readiness {
    pub note: &'static str,
    pub fix: Option<Fix>,
}

impl Family {
    pub fn title(self) -> &'static str {
        match self {
            Family::Files => "Files",
            Family::Commands => "Commands",
            Family::Web => "Web",
            Family::Browser => "Browser",
            Family::Media => "Media",
            Family::Messaging => "Messaging",
            Family::Plugins => "Plugins",
        }
    }

    /// What the family lets the model do, in a line.
    pub fn blurb(self) -> &'static str {
        match self {
            Family::Files => "Read, search, write and edit files in the project.",
            Family::Commands => "Run commands, and follow those left running.",
            Family::Web => "Fetch a page, and search the web with your search service.",
            Family::Browser => "Drive Chrome or Edge: open pages, click, type and read them.",
            Family::Media => "Make pictures, speech and video, and transcribe recordings.",
            Family::Messaging => "Read the chat a message came from, and send files back.",
            Family::Plugins => "Tools the window's plugins add.",
        }
    }

    /// The names of its tools, those not ready yet included, so switching it
    /// off covers them before they appear.
    pub fn tools(self) -> Vec<String> {
        let names: &[&str] = match self {
            Family::Files => &["read_file", "list_dir", "search", "write_file", "edit_file"],
            Family::Commands => &["run_command", "read_job", "stop_job", "send_job"],
            Family::Web => &["web_fetch", "search_web"],
            Family::Browser => &[crate::browser::BROWSE, "use_page"],
            Family::Media => &["make_image", "speak", "transcribe", "make_video"],
            Family::Messaging => &[
                crate::messaging::READ_CHAT,
                crate::messaging::files::SEND_FILE,
            ],
            Family::Plugins => return crate::tools::plugin_names(),
        };
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    /// What it still needs before it can do all it is for, or `None`.
    pub fn readiness(self) -> Option<Readiness> {
        let (ready, note, fix) = match self {
            Family::Files | Family::Commands => return None,
            // It searches, but by an unofficial way that may be limited, so
            // the note says which search runs and leads to a steadier one.
            Family::Web if crate::search::keyless() => (false, "keyless search", Some(Fix::Search)),
            Family::Web => (crate::search::ready(), "no search key", Some(Fix::Search)),
            Family::Browser => (
                crate::browser::remote_address().is_some() || crate::cdp::installed().is_some(),
                "no Chrome or Edge found",
                None,
            ),
            Family::Media => (
                crate::auth::signed_in(),
                "needs sign-in",
                Some(Fix::Account),
            ),
            Family::Messaging => {
                let config = crate::messaging::load();
                let connected =
                    config.telegram.enabled || config.discord.enabled || config.slack.enabled;
                (connected, "no chat app connected", Some(Fix::Messaging))
            }
            Family::Plugins => (
                !self.tools().is_empty(),
                "no plugin adds tools",
                Some(Fix::Plugins),
            ),
        };
        (!ready).then_some(Readiness { note, fix })
    }

    /// How many calls of its tools `counts` holds, as `store::tool_calls`
    /// gives them.
    pub fn calls(self, counts: &BTreeMap<String, usize>) -> usize {
        self.tools()
            .iter()
            .filter_map(|name| counts.get(name))
            .sum()
    }
}

/// Which of `tools` are switched off, anywhere they can be.
pub fn switched_off(tools: &[String]) -> Vec<String> {
    let off = crate::tools::turned_off();
    tools
        .iter()
        .filter(|name| crate::tools::switched_off(name, &off))
        .cloned()
        .collect()
}

/// Switch `tools` on or off for every session, the terminal's and helpers'
/// too. One the settings' `disabled_tools` lists stays off, and saying so is
/// the error.
pub fn switch(tools: &[String], on: bool) -> Result<(), String> {
    permissions::switch(&permissions::user_file(), tools, on)?;
    let listed = crate::store::disabled_tools();
    let held: Vec<&str> = tools
        .iter()
        .filter(|name| on && crate::tools::switched_off(name, &listed))
        .map(String::as_str)
        .collect();
    match held.as_slice() {
        [] => Ok(()),
        held => Err(format!(
            "{} stays off: `disabled_tools` in settings.json lists it.",
            held.join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_family_switched_off_is_off_for_every_tool_and_on_again_but_for_disabled_tools() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join("letronna-toolset-switch");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let settings =
            serde_json::json!({"endpoint": "", "model": "", "disabled_tools": ["speak"]});
        std::fs::write(dir.join("settings.json"), settings.to_string()).unwrap();

        let web = Family::Web.tools();
        switch(&web, false).unwrap();
        assert_eq!(switched_off(&web), web);
        switch(&web, true).unwrap();
        assert!(switched_off(&web).is_empty());

        let media = Family::Media.tools();
        switch(&media, false).unwrap();
        let held = switch(&media, true).unwrap_err();
        assert!(held.starts_with("speak stays off"), "{held}");
        assert_eq!(switched_off(&media), ["speak"]);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_web_family_says_keyless_search_runs_until_it_is_switched_off() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = crate::store::tests::Scratch::new("toolset-search");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &*dir) };
        let fix = Some(Fix::Search);
        let keyless = Family::Web.readiness();
        let on = (crate::search::offered(), crate::search::switched_off(None));
        std::fs::write(
            dir.join("settings.json"),
            r#"{"endpoint": "", "model": "", "keyless_search": false}"#,
        )
        .unwrap();
        let off = Family::Web.readiness();
        // Still offered, so a search can say how to switch it back on.
        let offered = crate::search::offered();
        let why = crate::search::switched_off(None);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
        assert_eq!(on, (true, None));
        assert!(offered && why.is_some_and(|why| why.contains("/search keyless")));
        assert_eq!(
            keyless,
            Some(Readiness {
                note: "keyless search",
                fix
            })
        );
        assert_eq!(
            off,
            Some(Readiness {
                note: "no search key",
                fix
            })
        );
    }

    #[test]
    fn a_familys_calls_add_up_its_own_tools_only() {
        let counts: BTreeMap<String, usize> = [("read_file", 3), ("edit_file", 2), ("browse", 5)]
            .into_iter()
            .map(|(name, n)| (name.to_owned(), n))
            .collect();
        assert_eq!(Family::Files.calls(&counts), 5);
        assert_eq!(Family::Browser.calls(&counts), 5);
        assert_eq!(Family::Web.calls(&counts), 0);
    }
}
