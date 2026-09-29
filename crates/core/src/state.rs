//! The small JSON files the window and the terminal both write:
//! `settings.json` and `keys.json`.
//!
//! Each process keeps its own copy in memory and saves it whenever something
//! moves, so a plain whole-file write let whichever saved last undo what the
//! other had just done. Worse, a file that would not parse was read as the
//! defaults and then saved as them, and the user's setup was gone for good.
//!
//! So a save takes the file's lock and starts from what is on disk at that
//! moment, and writes nothing when it would change nothing. A file that will
//! not parse is moved aside to `<name>.corrupt-<unix>`, where what it held can
//! still be got back, rather than written over. New bytes go to a temp file
//! named for the process and are then moved into place, so neither a crash
//! nor the other process saving at the same moment leaves half a file.

use crate::store::Lock;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// What reading a state file found.
enum Found<T> {
    Nothing,
    Good(T, Value),
    Bad,
}

/// The file as `T`, or the default when there is none; one that will not
/// parse is moved aside first.
pub(crate) fn read<T: DeserializeOwned + Default>(path: &Path) -> Result<T, String> {
    let found = match look::<T>(path)? {
        Found::Nothing => None,
        Found::Good(parsed, _) => Some(parsed),
        // Files are only ever moved into place whole, so reading needs no
        // lock, but moving one aside does: without it, the good file the
        // other process has just put there could be the one moved.
        Found::Bad => {
            let _lock = Lock::take(path).map_err(|e| e.to_string())?;
            settle::<T>(path)?.map(|(parsed, _)| parsed)
        }
    };
    Ok(found.unwrap_or_default())
}

/// Change the file under its lock, starting from what it holds now (null when
/// there is nothing), and write it back only if that changed anything.
pub(crate) fn update<T: DeserializeOwned>(
    path: &Path,
    change: impl FnOnce(&mut Value),
) -> Result<(), String> {
    let _lock = Lock::take(path).map_err(|e| e.to_string())?;
    let before = settle::<T>(path)?
        .map(|(_, value)| value)
        .unwrap_or_default();
    let mut after = before.clone();
    change(&mut after);
    if after == before {
        return keep_private(path);
    }
    replace(path, &after)
}

/// Put onto `disk` each field that differs between `then` and `now`, leaving
/// the others as the file has them; with nothing on disk, `now` is all of it.
pub(crate) fn merge(disk: &mut Value, then: &Value, now: &Value) {
    match (disk.as_object_mut(), now.as_object()) {
        (Some(disk), Some(now)) => {
            for (field, value) in now {
                if then.get(field) != Some(value) {
                    disk.insert(field.clone(), value.clone());
                }
            }
        }
        _ => *disk = now.clone(),
    }
}

/// What the file holds, read with its lock held: one that will not parse is
/// moved aside and counts as nothing.
fn settle<T: DeserializeOwned>(path: &Path) -> Result<Option<(T, Value)>, String> {
    match look(path)? {
        Found::Nothing => Ok(None),
        Found::Good(parsed, value) => Ok(Some((parsed, value))),
        Found::Bad => quarantine(path).map(|()| None),
    }
}

/// Read the file and parse it, as JSON and then as what it should hold.
fn look<T: DeserializeOwned>(path: &Path) -> Result<Found<T>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Found::Nothing),
        Err(e) => return Err(format!("Could not read {}: {e}", path.display())),
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(Found::Bad);
    };
    // JSON of the wrong shape would be read as the defaults and saved as
    // them just the same as garbage would.
    match T::deserialize(&value) {
        Ok(parsed) => Ok(Found::Good(parsed, value)),
        Err(_) => Ok(Found::Bad),
    }
}

/// Move a file that will not parse to `<name>.corrupt-<unix>`, where the user
/// can still get back what it held.
fn quarantine(path: &Path) -> Result<(), String> {
    let mut aside = path.as_os_str().to_owned();
    aside.push(format!(".corrupt-{}", crate::chat::now()));
    match std::fs::rename(path, &aside) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!(
            "{} will not parse and could not be moved aside: {e}",
            path.display()
        )),
        _ => Ok(()),
    }
}

/// Write the value to this process's temp file, then move it over the file.
fn replace(path: &Path, value: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    let temp = temp_path(path);
    // Private whichever file this is: keys.json must be, and a Custom
    // gateway's URL in settings.json can carry a key too.
    let saved =
        crate::store::write_private(&temp, &bytes).and_then(|()| std::fs::rename(&temp, path));
    saved.map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        format!("Could not save {}: {e}", path.display())
    })
}

/// Make the file readable by its owner only when a save has nothing to
/// write, as writing it would have, so one an older build left open is not
/// left that way.
#[cfg(unix)]
fn keep_private(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("Could not make {} private: {e}", path.display()))
        }
        _ => Ok(()),
    }
}

/// Do nothing, since the user's own AppData is private already.
#[cfg(not(unix))]
fn keep_private(_: &Path) -> Result<(), String> {
    Ok(())
}

/// The temp file a save goes through, named for the process so the window
/// and the terminal saving at once never move each other's into place.
fn temp_path(path: &Path) -> PathBuf {
    path.with_extension(format!("{}.tmp", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::{merge, read, temp_path, update};
    use serde_json::{Value, json};
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    type Keys = HashMap<String, String>;

    fn scratch(name: &str) -> PathBuf {
        // One folder per test, reused and emptied each run.
        let dir = std::env::temp_dir().join(format!("letronna-state-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Where `path` was moved aside to, if it was.
    fn set_aside(path: &Path) -> Vec<PathBuf> {
        let prefix = format!("{}.corrupt-", path.file_name().unwrap().to_string_lossy());
        std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&prefix)
            })
            .collect()
    }

    #[test]
    fn a_file_that_will_not_parse_is_moved_aside_not_reset() {
        let dir = scratch("corrupt");
        let path = dir.join("keys.json");
        let torn = "{\"OpenAI\": \"sk-1\", \"Gro";
        std::fs::write(&path, torn).unwrap();
        assert!(read::<Keys>(&path).unwrap().is_empty());
        assert!(!path.exists());
        let aside = set_aside(&path);
        assert_eq!(aside.len(), 1);
        assert_eq!(std::fs::read_to_string(&aside[0]).unwrap(), torn);

        // Saving afterwards starts a new file and leaves the old one be.
        update::<Keys>(&path, |disk| *disk = json!({"Groq": "gsk-2"})).unwrap();
        assert_eq!(read::<Keys>(&path).unwrap()["Groq"], "gsk-2");
        assert_eq!(std::fs::read_to_string(&aside[0]).unwrap(), torn);

        // JSON of the wrong shape is no better, and a save that finds one
        // keeps it the same way.
        let other = dir.join("other.json");
        std::fs::write(&other, "{\"OpenAI\": 5}").unwrap();
        update::<Keys>(&other, |disk| {
            merge(disk, &Value::Null, &json!({"Groq": "gsk-2"}))
        })
        .unwrap();
        let aside = set_aside(&other);
        assert_eq!(aside.len(), 1);
        assert_eq!(
            std::fs::read_to_string(&aside[0]).unwrap(),
            "{\"OpenAI\": 5}"
        );
        assert_eq!(read::<Keys>(&other).unwrap().len(), 1);
    }

    #[test]
    fn a_save_that_changes_nothing_writes_nothing() {
        let dir = scratch("unchanged");
        let path = dir.join("keys.json");
        // Laid out by hand, not the way a save lays it out.
        let by_hand = "{\"OpenAI\":\"sk-1\"}";
        std::fs::write(&path, by_hand).unwrap();
        update::<Keys>(&path, |disk| disk["OpenAI"] = json!("sk-1")).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), by_hand);
    }

    #[test]
    fn each_process_saves_through_a_temp_file_of_its_own() {
        let dir = scratch("temp");
        let path = dir.join("settings.json");
        let mine = dir.join(format!("settings.{}.tmp", std::process::id()));
        assert_eq!(temp_path(&path), mine);
        // Another process part way through a save, under the name every
        // process once shared and under a name of its own.
        for theirs in ["settings.tmp", "settings.1.tmp"] {
            std::fs::write(dir.join(theirs), "{\"half").unwrap();
        }
        update::<Value>(&path, |disk| *disk = json!({"model": "a/one"})).unwrap();
        assert_eq!(read::<Value>(&path).unwrap(), json!({"model": "a/one"}));
        for theirs in ["settings.tmp", "settings.1.tmp"] {
            assert_eq!(
                std::fs::read_to_string(dir.join(theirs)).unwrap(),
                "{\"half"
            );
        }
        assert!(!mine.exists());
    }

    #[test]
    fn a_save_starts_from_the_file_as_it_is_now_under_its_lock() {
        let dir = scratch("reread");
        let path = dir.join("keys.json");
        let lock = dir.join("keys.lock");
        std::fs::write(&path, "{\"OpenAI\":\"sk-1\"}").unwrap();
        // The other process saves a key after this one read the file.
        let _read_earlier = read::<Keys>(&path).unwrap();
        std::fs::write(&path, "{\"OpenAI\":\"sk-1\",\"Groq\":\"gsk-2\"}").unwrap();
        update::<Keys>(&path, |disk| {
            assert!(lock.exists(), "changed without the lock");
            assert_eq!(disk["Groq"], "gsk-2");
            disk["Mistral"] = json!("m-3");
        })
        .unwrap();
        assert!(
            crate::store::Lock::take(&path).is_ok(),
            "the lock was not let go"
        );
        assert_eq!(read::<Keys>(&path).unwrap().len(), 3);
    }

    #[test]
    fn only_the_fields_changed_here_are_put_back() {
        // This process changed effort since it read the file; the other
        // changed model, and a newer build added a field of its own.
        let then = json!({"model": "a/one", "effort": "low"});
        let now = json!({"model": "a/one", "effort": "high"});
        let mut disk = json!({"model": "b/two", "effort": "low", "later": true});
        merge(&mut disk, &then, &now);
        assert_eq!(
            disk,
            json!({"model": "b/two", "effort": "high", "later": true})
        );

        // With no file there is nothing to keep, so all of it is written.
        let mut nothing = Value::Null;
        merge(&mut nothing, &then, &now);
        assert_eq!(nothing, now);
    }

    #[cfg(unix)]
    #[test]
    fn a_saved_file_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("private");
        let path = dir.join("keys.json");
        update::<Keys>(&path, |disk| *disk = json!({"OpenAI": "sk-1"})).unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);

        // One an older build left readable by everyone is made private even
        // by a save with nothing to write.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        update::<Keys>(&path, |disk| disk["OpenAI"] = json!("sk-1")).unwrap();
        assert_eq!(mode(&path), 0o600);
    }
}
