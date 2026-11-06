//! Folders added beside a session's project, as the terminal's `--add-dir`
//! and `/add-dir` add them: the session reads and changes files there as it
//! does in the project, held to the same checks. The window keeps them for
//! each session only while it is open, never in the session's file, so a
//! window opened again starts every session with none, as a new run of the
//! terminal does: reach beyond the project is given for now, not for good.

use crate::prelude::*;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

impl Chat {
    /// The folders added to the session `id`, as its next turn takes them.
    pub(crate) fn added_to(&self, id: u64) -> Arc<[PathBuf]> {
        added_to(&self.added, id)
    }

    /// Add the folder `raw` names to the session `id`, from its project
    /// folder or in full, or say why it cannot be. It is in reach from that
    /// session's next turn.
    pub(crate) fn add_folder(&mut self, id: u64, raw: &str) -> Result<PathBuf, String> {
        let project = self
            .store
            .index_of(id)
            .and_then(|ix| self.store.conversations[ix].workspace.clone());
        add(self.added.entry(id).or_default(), raw, project.as_deref())
    }

    /// Take `folder` out of the session `id`'s reach, from its next turn.
    pub(crate) fn remove_added(&mut self, id: u64, folder: &Path, cx: &mut Context<Self>) {
        if let Some(list) = self.added.get_mut(&id) {
            list.retain(|kept| kept != folder);
        }
        cx.notify();
    }

    /// Pick folders to add to the active session with the system's picker.
    pub(crate) fn pick_added_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: None,
        });
        // To the session it was asked for, though another may be on screen
        // by the time the picker closes.
        let id = self.store.conversations[self.store.active].id;
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                for path in paths {
                    let added = this.add_folder(id, &path.to_string_lossy());
                    this.say_added(added, window, cx);
                }
            });
        })
        .detach();
    }

    /// `/add-dir` as typed: add the folder `rest` names, or pick one when it
    /// names none. What was typed stays in the box when it cannot be added.
    pub(crate) fn add_dir_command(
        &mut self,
        rest: &str,
        typed: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composer
            .update(cx, |input, cx| input.set_value("", window, cx));
        if rest.trim().is_empty() {
            return self.pick_added_folder(window, cx);
        }
        let id = self.store.conversations[self.store.active].id;
        let added = self.add_folder(id, rest);
        if added.is_err() {
            self.composer
                .update(cx, |input, cx| input.set_value(typed, window, cx));
        }
        self.say_added(added, window, cx);
    }

    /// Say what adding a folder came to: in reach until the window closes,
    /// or why it is not.
    fn say_added(
        &mut self,
        added: Result<PathBuf, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match added {
            Ok(folder) => window.push_notification(
                gpui_component::notification::Notification::info(format!(
                    "This session can reach {} too, until the window closes.",
                    folder.display()
                )),
                cx,
            ),
            Err(why) => self.error = Some(why),
        }
        cx.notify();
    }
}

/// Add the folder `raw` names to `list`, as `tools::added_folder` allows it
/// beside `project`, and give it; one added already is said so.
fn add(list: &mut Vec<PathBuf>, raw: &str, project: Option<&Path>) -> Result<PathBuf, String> {
    let raw = raw.trim();
    // A short path is read from the project; with none, the window has no
    // folder of its own to read it from.
    let in_full = Path::new(raw).is_absolute() || raw == "~" || raw.starts_with("~/");
    if project.is_none() && !raw.is_empty() && !in_full {
        return Err(format!(
            "`{raw}` would be read from the project folder, and this session has none: \
             give the folder's full path."
        ));
    }
    let folder = letronna_core::tools::added_folder(raw, project)?;
    if list.contains(&folder) {
        return Err(format!("{} is added already.", folder.display()));
    }
    list.push(folder.clone());
    Ok(folder)
}

/// The folders `added` holds for the session `id`, as a turn's connection
/// takes them.
fn added_to(added: &HashMap<u64, Vec<PathBuf>>, id: u64) -> Arc<[PathBuf]> {
    added
        .get(&id)
        .map(|list| Arc::from(list.as_slice()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{add, added_to};
    use std::collections::HashMap;
    use std::path::PathBuf;

    /// A project folder and one beside it, in a scratch folder of their own.
    fn folders(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("letronna-gui-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("app")).unwrap();
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        // Without the `\\?\` Windows puts on a canonical path, as an added
        // folder is given: dropped in front of a drive, kept before a share.
        let real = std::fs::canonicalize(dir.join("lib")).unwrap();
        let text = real.to_string_lossy();
        let lib = match text.strip_prefix(r"\\?\") {
            Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
            _ => real.clone(),
        };
        (dir.join("app"), lib)
    }

    #[test]
    fn a_folder_is_added_once_and_a_second_time_is_said_so() {
        let (project, lib) = folders("added-once");
        let mut list = Vec::new();
        assert_eq!(add(&mut list, "../lib", Some(&project)), Ok(lib.clone()));
        let again = add(&mut list, lib.to_str().unwrap(), Some(&project)).unwrap_err();
        assert!(again.contains("added already"), "{again}");
        assert_eq!(list, vec![lib]);
        let _ = std::fs::remove_dir_all(project.parent().unwrap());
    }

    #[test]
    fn a_folder_that_cannot_be_added_says_why_and_adds_nothing() {
        let (project, _) = folders("refused");
        let mut list = Vec::new();
        let home = add(&mut list, "~", Some(&project)).unwrap_err();
        assert!(home.contains("home folder"), "{home}");
        let missing = add(&mut list, "../missing", Some(&project)).unwrap_err();
        assert!(missing.contains("does not exist"), "{missing}");
        // Nowhere to read a short path from without a project.
        let short = add(&mut list, "../lib", None).unwrap_err();
        assert!(short.contains("full path"), "{short}");
        assert!(list.is_empty());
        let _ = std::fs::remove_dir_all(project.parent().unwrap());
    }

    #[test]
    fn each_session_s_turns_get_only_its_own_folders_and_a_new_window_none() {
        let (project, lib) = folders("per-session");
        let mut added: HashMap<u64, Vec<PathBuf>> = HashMap::new();
        add(added.entry(7).or_default(), "../lib", Some(&project)).unwrap();
        assert_eq!(&*added_to(&added, 7), &[lib][..]);
        assert!(added_to(&added, 8).is_empty());
        // What a window opened again starts with: nothing kept anywhere.
        assert!(added_to(&HashMap::new(), 7).is_empty());
        let _ = std::fs::remove_dir_all(project.parent().unwrap());
    }
}
