//! Saved prompts the user reaches for often.
//!
//! A skill is a name and a prompt. Loading one drops its text into the
//! composer rather than sending it, so it can be adjusted first: a playbook is
//! a starting point, not a command.

use crate::chat::{self as backend};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize)]
pub struct Skill {
    pub id: u64,
    pub name: String,
    pub prompt: String,
}

pub fn path() -> PathBuf {
    backend::data_dir().join("skills.json")
}

/// The saved skills, the defaults when there is no file yet, or an error
/// naming the copy kept of a file that will not parse.
pub fn load() -> Result<Vec<Skill>, String> {
    load_from(&path())
}

fn load_from(path: &Path) -> Result<Vec<Skill>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(defaults()),
        Err(e) => {
            return Err(format!(
                "Could not read your skills from {}: {e}",
                path.display()
            ));
        }
    };
    let why = match serde_json::from_slice(&bytes) {
        Ok(skills) => return Ok(skills),
        Err(why) => why,
    };
    // Not the defaults, which the window's next save would write over the
    // only copy of the user's skills.
    let kept = keep_aside(path, &bytes)?;
    Err(format!(
        "Could not read your skills from {}: {why}. A copy is kept at {}.",
        path.display(),
        kept.display()
    ))
}

/// Copy a file that will not parse to `<name>.bad.<unix seconds>` beside it,
/// or name the copy of the same bytes already there.
fn keep_aside(path: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let prefix = format!("{name}.bad.");
    // The terminal reads the file on every /skills, and one copy is enough.
    if let Some(Ok(entries)) = path.parent().map(std::fs::read_dir) {
        for kept in entries.flatten().map(|entry| entry.path()) {
            let ours = kept
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(&prefix));
            if ours && std::fs::read(&kept).is_ok_and(|earlier| earlier == bytes) {
                return Ok(kept);
            }
        }
    }
    let kept = path.with_file_name(format!("{prefix}{}", backend::now()));
    std::fs::write(&kept, bytes).map_err(|e| {
        format!(
            "Could not read your skills from {}, nor keep a copy at {}: {e}",
            path.display(),
            kept.display()
        )
    })?;
    Ok(kept)
}

/// Write the skills beside the file and move them over it, so a crash cannot
/// leave half a file.
pub fn save(skills: &[Skill]) -> Result<(), String> {
    save_to(&path(), skills)
}

fn save_to(path: &Path, skills: &[Skill]) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(skills)
        .map_err(|e| format!("Could not save your skills: {e}"))?;
    let temp = path.with_extension("tmp");
    path.parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&temp, bytes))
        .and_then(|()| std::fs::rename(&temp, path))
        .map_err(|e| format!("Could not save your skills to {}: {e}", path.display()))
}

pub fn next_id(skills: &[Skill]) -> u64 {
    skills.iter().map(|s| s.id).max().unwrap_or(0) + 1
}

/// A few to start with, so the page explains itself by example rather than
/// opening empty. They are ordinary skills: editable and deletable.
fn defaults() -> Vec<Skill> {
    [
        (
            "Review these changes",
            "Look at the uncommitted changes in this project and tell me what is wrong with them. \
             Prefer correctness bugs over style. Read the files you need first.",
        ),
        (
            "Explain this project",
            "Work out what this project does and how it is laid out. Read the manifest, the entry \
             point and the main modules, then explain it to me in a few paragraphs.",
        ),
        (
            "Run the tests",
            "Run this project's test suite and tell me what failed. If something fails, read the \
             relevant code and say what is causing it. Do not change anything yet.",
        ),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (name, prompt))| Skill {
        id: i as u64 + 1,
        name: name.to_owned(),
        prompt: prompt.to_owned(),
    })
    .collect()
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{Skill, defaults, load_from, next_id, save_to};
    use std::path::{Path, PathBuf};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("letronna-skills-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn kept_copies(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|p| p.to_string_lossy().contains("skills.json.bad."))
            .collect()
    }

    fn names(skills: &[Skill]) -> Vec<(u64, String, String)> {
        skills
            .iter()
            .map(|s| (s.id, s.name.clone(), s.prompt.clone()))
            .collect()
    }

    #[test]
    fn no_file_yet_gives_the_defaults() {
        let dir = scratch("missing");
        let loaded = load_from(&dir.join("skills.json")).unwrap();
        assert_eq!(names(&loaded), names(&defaults()));
        assert!(kept_copies(&dir).is_empty());
    }

    #[test]
    fn a_file_that_will_not_parse_is_copied_aside_and_reported() {
        let dir = scratch("broken");
        let path = dir.join("skills.json");
        let broken = b"[{\"id\": 1, \"name\": \"Mine\", \"prompt\": \"Keep me\",}]";
        std::fs::write(&path, broken).unwrap();

        let Err(error) = load_from(&path) else {
            panic!("a broken file loaded");
        };
        let kept = kept_copies(&dir);
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert!(error.contains(&kept[0].display().to_string()), "{error}");
        assert_eq!(std::fs::read(&kept[0]).unwrap(), broken);
        // The file itself is left for the user to mend.
        assert_eq!(std::fs::read(&path).unwrap(), broken);
    }

    #[test]
    fn the_same_broken_file_is_copied_only_once() {
        let dir = scratch("once");
        let path = dir.join("skills.json");
        std::fs::write(&path, b"not json").unwrap();
        let earlier = dir.join("skills.json.bad.1");
        std::fs::write(&earlier, b"not json").unwrap();

        let Err(error) = load_from(&path) else {
            panic!("a broken file loaded");
        };
        assert!(error.contains(&earlier.display().to_string()), "{error}");
        assert_eq!(kept_copies(&dir), [earlier]);
    }

    #[test]
    fn a_save_replaces_the_file_whole_and_leaves_no_temp_file() {
        let dir = scratch("save");
        let path = dir.join("nested").join("skills.json");
        save_to(&path, &defaults()).unwrap();
        let mine = [Skill {
            id: 7,
            name: "Mine".into(),
            prompt: "Keep me".into(),
        }];
        save_to(&path, &mine).unwrap();

        assert_eq!(names(&load_from(&path).unwrap()), names(&mine));
        assert!(!path.with_extension("tmp").exists());
    }

    #[test]
    fn a_save_that_fails_midway_leaves_the_old_file_whole() {
        let dir = scratch("midway");
        let path = dir.join("skills.json");
        save_to(&path, &defaults()).unwrap();
        let before = std::fs::read(&path).unwrap();
        // Only the temp file cannot be written, which a save in place would
        // never notice.
        std::fs::create_dir(path.with_extension("tmp")).unwrap();

        let mine = [Skill {
            id: 7,
            name: "Mine".into(),
            prompt: "Keep me".into(),
        }];
        assert!(save_to(&path, &mine).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn a_save_that_cannot_write_says_so() {
        let dir = scratch("unwritable");
        let file = dir.join("a-file");
        std::fs::write(&file, b"").unwrap();
        let error = save_to(&file.join("skills.json"), &defaults()).unwrap_err();
        assert!(error.starts_with("Could not save your skills"), "{error}");
    }

    #[test]
    fn defaults_are_usable_skills_with_distinct_ids() {
        let skills = defaults();
        assert!(!skills.is_empty());
        for skill in &skills {
            assert!(!skill.name.is_empty());
            assert!(!skill.prompt.is_empty());
        }
        let mut ids: Vec<u64> = skills.iter().map(|s| s.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), skills.len(), "ids collide");
        // A new skill must not reuse one of them.
        assert!(!ids.contains(&next_id(&skills)));
    }
}
