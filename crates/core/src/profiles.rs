//! Profiles: separate data folders for separate identities.
//!
//! Everything a profile owns already lives directly under `data_dir`: chats,
//! `SOUL.md`, skills, keys, settings, the default model. A profile is
//! nothing more than one of these folders, picked at runtime, so switching
//! or cloning one is a matter of pointing `chat::data_dir` at a different
//! folder, or copying it -- no format of its own to keep in step with.
//!
//! The default profile is `chat::base_dir` itself, so an install with a
//! single profile looks exactly as it always has. Every other profile is a
//! folder under `base_dir/profiles`, named after it.

use std::path::{Path, PathBuf};

/// A profile: its name, and where its files live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub dir: PathBuf,
}

/// The default profile's name. Not a name another profile can also take, so
/// it is always unambiguous.
pub const DEFAULT: &str = "Default";

fn root() -> PathBuf {
    crate::chat::base_dir().join("profiles")
}

/// Every profile, the default first and the rest alphabetically.
pub fn list() -> Vec<Profile> {
    let mut rest: Vec<Profile> = std::fs::read_dir(root())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| Profile {
            name: entry.file_name().to_string_lossy().into_owned(),
            dir: entry.path(),
        })
        .collect();
    rest.sort_by_key(|a| a.name.to_lowercase());
    let mut all = vec![Profile {
        name: DEFAULT.to_owned(),
        dir: crate::chat::base_dir(),
    }];
    all.append(&mut rest);
    all
}

/// The active profile's name: whichever of `list` is `chat::data_dir` right
/// now, `Default` if none was switched to or the folder it was switched to
/// is gone.
pub fn active() -> String {
    let dir = crate::chat::data_dir();
    list()
        .into_iter()
        .find(|p| p.dir == dir)
        .map(|p| p.name)
        .unwrap_or_else(|| DEFAULT.to_owned())
}

/// Point `chat::data_dir` at `name`'s folder from now on. Whatever was
/// reading or writing the old profile should be finished first: the switch
/// takes hold for the very next call.
pub fn switch(name: &str) -> Result<(), String> {
    let profile = list()
        .into_iter()
        .find(|p| p.name == name)
        .ok_or_else(|| format!("No profile named \"{name}\"."))?;
    crate::chat::set_profile_dir((profile.name != DEFAULT).then_some(profile.dir));
    Ok(())
}

/// A name turned into a folder name: letters, digits, spaces, `-` and `_`
/// kept, everything else replaced so a profile's name cannot walk it out of
/// `profiles`.
fn folder_name(name: &str) -> String {
    name.trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Whether Windows would treat `name` as a device rather than a folder.
fn is_reserved(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    matches!(name.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|p| {
            name.strip_prefix(p)
                .is_some_and(|n| n.len() == 1 && n != "0" && n.chars().all(|c| c.is_ascii_digit()))
        })
}

/// A new profile named `name`: empty, or a copy of `clone_from`'s files.
/// The profile is named as its folder is, since that is all `list` can give
/// back, so `Work!` becomes `Work-`.
pub fn create(name: &str, clone_from: Option<&str>) -> Result<Profile, String> {
    let name = folder_name(name);
    if name.trim_matches('-').trim().is_empty() {
        return Err("Name the profile.".into());
    }
    if is_reserved(&name) {
        return Err(format!("\"{name}\" is not a name a folder can have."));
    }
    let existing = list();
    if name.eq_ignore_ascii_case(DEFAULT)
        || existing
            .iter()
            .any(|p| p.name.to_lowercase() == name.to_lowercase())
    {
        return Err(format!("There is already a profile named \"{name}\"."));
    }
    let dir = root().join(&name);
    if dir.exists() {
        return Err(format!("There is already a profile named \"{name}\"."));
    }
    let made = match clone_from {
        Some(from) => {
            let source = existing
                .into_iter()
                .find(|p| p.name == from)
                .ok_or_else(|| format!("No profile named \"{from}\" to clone."))?;
            // The default profile's dir is `base_dir`, which also holds
            // `profiles` itself -- every other profile's files, none of
            // which belong inside the one being cloned.
            copy_dir(&source.dir, &dir, &root())
        }
        None => std::fs::create_dir_all(&dir),
    };
    if let Err(err) = made {
        // Half a copy would show up in `list` as a profile of its own.
        let _ = std::fs::remove_dir_all(&dir);
        return Err(err.to_string());
    }
    Ok(Profile { name, dir })
}

/// Every file and folder under `src`, made again under `dst`, but not
/// `exclude` itself. Links are left out: copying one would read whatever it
/// points at, wherever that is, into the new profile.
fn copy_dir(src: &Path, dst: &Path, exclude: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        if path == exclude {
            continue;
        }
        let to = dst.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_dir(&path, &to, exclude)?;
        } else if kind.is_file() {
            std::fs::copy(&path, to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch `base_dir`, `AGENT_DATA_DIR` pointed at it, held under the
    /// lock every test touching that variable shares.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("letronna-profiles-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_fresh_install_has_only_the_default_profile() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch("fresh");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let profiles = list();
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
        assert_eq!(
            profiles,
            vec![Profile {
                name: DEFAULT.to_owned(),
                dir,
            }]
        );
    }

    #[test]
    fn a_new_profile_starts_empty_and_can_be_switched_to_and_from() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch("switch");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let profile = create("Work", None).unwrap();
        assert!(profile.dir.starts_with(dir.join("profiles")));
        assert_eq!(active(), DEFAULT);

        switch("Work").unwrap();
        assert_eq!(active(), "Work");
        assert_eq!(crate::chat::data_dir(), profile.dir);

        switch(DEFAULT).unwrap();
        assert_eq!(active(), DEFAULT);
        assert_eq!(crate::chat::data_dir(), dir);

        crate::chat::set_profile_dir(None);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn cloning_a_profile_copies_its_files_but_not_its_further_changes() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch("clone");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        std::fs::write(dir.join("SOUL.md"), "Be terse.").unwrap();

        let source = list().remove(0);
        let clone = create("Personal", Some(&source.name)).unwrap();
        assert_eq!(
            std::fs::read_to_string(clone.dir.join("SOUL.md")).unwrap(),
            "Be terse."
        );

        std::fs::write(dir.join("SOUL.md"), "Be verbose.").unwrap();
        assert_eq!(
            std::fs::read_to_string(clone.dir.join("SOUL.md")).unwrap(),
            "Be terse."
        );

        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_profile_cannot_be_named_default_or_reuse_a_name() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch("names");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        assert!(create("default", None).is_err());
        assert!(create("  ", None).is_err());
        create("Work", None).unwrap();
        assert!(create("Work", None).is_err());

        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_profile_is_named_as_its_folder_so_it_can_be_switched_to() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch("folder-names");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let profile = create("Work!", None).unwrap();
        assert_eq!(profile.name, "Work-");
        assert!(switch(&profile.name).is_ok());
        crate::chat::set_profile_dir(None);
        assert!(create("work-", None).is_err());
        assert!(create("../..", None).is_err());
        assert!(create("nul", None).is_err());
        assert!(create("COM3", None).is_err());

        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[cfg(unix)]
    #[test]
    fn cloning_leaves_links_behind() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch("links");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let outside = scratch("links-outside");
        std::fs::write(outside.join("secret"), "hunter2").unwrap();
        std::os::unix::fs::symlink(outside.join("secret"), dir.join("linked")).unwrap();
        std::fs::write(dir.join("SOUL.md"), "Be terse.").unwrap();

        let clone = create("Copy", Some(DEFAULT)).unwrap();
        assert!(clone.dir.join("SOUL.md").exists());
        assert!(!clone.dir.join("linked").exists());

        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }
}
