//! Saved prompts the user reaches for often.
//!
//! A skill is a name and a prompt. Loading one drops its text into the
//! composer rather than sending it, so it can be adjusted first: a playbook is
//! a starting point, not a command.

use crate::prelude::*;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Skill {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) prompt: String,
}

pub(crate) fn path() -> PathBuf {
    backend::data_dir().join("skills.json")
}

pub(crate) fn load() -> Vec<Skill> {
    std::fs::read(path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(defaults)
}

pub(crate) fn save(skills: &[Skill]) {
    let _ = std::fs::create_dir_all(backend::data_dir());
    if let Ok(bytes) = serde_json::to_vec_pretty(skills) {
        let _ = std::fs::write(path(), bytes);
    }
}

pub(crate) fn next_id(skills: &[Skill]) -> u64 {
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
    use super::{defaults, next_id};

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
