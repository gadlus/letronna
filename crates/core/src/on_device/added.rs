//! Models added from a Hugging Face link, kept in `models/added.json` and
//! offered beside the catalog's.
//!
//! The rest of `on_device` takes a model as `&'static Entry`, as the
//! catalog's are, so each added one is made into an entry once and kept
//! for the life of the process: a handful of strings per model, interned so
//! that reading the list again, or looking the same repository up twice,
//! makes nothing new.
//!
//! Nobody has measured these models, so what the catalog knows of its own
//! is guessed here, on the side that says "needs more": the KV cache from
//! the parameter count, the share read per token from an `A3B`-style name.
//! They are offered for chat alone, as a model is trusted with a tool only
//! once the eval has passed it.

use super::catalog::{CATALOG, Entry, Pack, Sampling, Thinking, Tools};
use super::{fit, hub, shelf};
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

/// A model added from a link, as it is kept.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Added {
    pub id: String,
    pub name: String,
    /// Who published the repository.
    pub maker: String,
    /// `owner/repo`.
    pub repo: String,
    pub commit: String,
    /// Its path in the repository.
    pub file: String,
    pub size: u64,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eyes: Option<Eyes>,
    /// Its parameters, as the hub counted them from the GGUF.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<u64>,
    /// The context it was trained for, as its GGUF says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<u64>,
}

/// The vision pack beside an added model.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Eyes {
    pub file: String,
    pub size: u64,
    pub sha256: String,
}

/// llama.cpp's own sampling, near enough, for a model nobody here has
/// measured.
const PLAIN: Sampling = Sampling {
    temperature: 0.8,
    top_p: 0.95,
    top_k: 40,
    presence_penalty: 0.0,
};

const KIB: u64 = 1024;
const MB: u64 = 1_000_000;

fn list_path() -> PathBuf {
    shelf::dir().join("added.json")
}

/// The list as last read, and the file's state it was read from: the
/// terminal and the window may each add a model while the other runs.
struct Read {
    path: PathBuf,
    stamp: Option<(SystemTime, u64)>,
    entries: Vec<&'static Entry>,
}

static READ: Mutex<Option<Read>> = Mutex::new(None);
static POOL: Mutex<Vec<(Added, &'static Entry)>> = Mutex::new(Vec::new());

fn stamp(path: &std::path::Path) -> Option<(SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

fn read_list() -> Vec<Added> {
    std::fs::read(list_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Vec<Added>>(&bytes).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(sane)
        .collect()
}

/// Every model added here, in the order they were added.
pub fn all() -> Vec<&'static Entry> {
    let path = list_path();
    let now = stamp(&path);
    let mut read = READ.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(read) = read.as_ref()
        && read.path == path
        && read.stamp == now
    {
        return read.entries.clone();
    }
    let entries: Vec<&'static Entry> = read_list().iter().map(intern).collect();
    *read = Some(Read {
        path,
        stamp: now,
        entries: entries.clone(),
    });
    entries
}

/// The added model with this id.
pub fn entry(id: &str) -> Option<&'static Entry> {
    all().into_iter().find(|entry| entry.id == id)
}

/// Whether `entry` was added from a link rather than offered by Letronna.
pub fn is_added(entry: &Entry) -> bool {
    !CATALOG.iter().any(|known| known.id == entry.id)
}

/// Keep `added`, so it is offered from now on; the one kept already when
/// the same file was added before.
pub fn add(added: &Added) -> Result<&'static Entry, String> {
    if !sane(added) {
        return Err("That model's details didn't look right, so it wasn't added.".into());
    }
    let mut list = read_list();
    if let Some(kept) = list.iter().find(|kept| {
        kept.repo == added.repo && kept.file == added.file && kept.sha256 == added.sha256
    }) {
        return Ok(intern(kept));
    }
    let mut added = added.clone();
    let taken = |id: &str| {
        CATALOG.iter().any(|known| known.id == id) || list.iter().any(|kept| kept.id == id)
    };
    let base = added.id.clone();
    let mut n = 2;
    while taken(&added.id) {
        added.id = format!("{base}-{n}");
        n += 1;
    }
    list.push(added.clone());
    write(&list)?;
    Ok(intern(&added))
}

/// The entry `added` would be, so a verdict can be given before it is kept.
pub fn preview(added: &Added) -> &'static Entry {
    intern(added)
}

/// Keep the model `entry` was previewed from, once its download starts:
/// the kept entry, which is `entry` unless its id was taken meanwhile.
pub fn keep(entry: &'static Entry) -> Result<&'static Entry, String> {
    let added = POOL
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(_, made)| std::ptr::eq(*made, entry))
        .map(|(added, _)| added.clone());
    match added {
        Some(added) => add(&added),
        // One of the catalog's.
        None => Ok(entry),
    }
}

/// Stop offering the added model `id`. Its files are the shelf's to delete.
pub fn forget(id: &str) -> Result<(), String> {
    let mut list = read_list();
    let before = list.len();
    list.retain(|kept| kept.id != id);
    if list.len() == before {
        return Ok(());
    }
    write(&list)
}

fn write(list: &[Added]) -> Result<(), String> {
    let path = list_path();
    let failed = |e: std::io::Error| format!("Could not save the list of models: {e}");
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder).map_err(failed)?;
    }
    let text = serde_json::to_vec_pretty(list).map_err(|e| e.to_string())?;
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&temp, text).map_err(failed)?;
    std::fs::rename(&temp, &path).map_err(failed)?;
    // Read afresh next time, even within the file system's clock tick.
    *READ.lock().unwrap_or_else(PoisonError::into_inner) = None;
    Ok(())
}

/// Whether a kept model can be trusted as a folder name and a download: the
/// list is a file anyone with this account can edit.
fn sane(added: &Added) -> bool {
    let hex =
        |text: &str, len: usize| text.len() == len && text.bytes().all(|b| b.is_ascii_hexdigit());
    let pack = |file: &str, size: u64, sha256: &str| {
        hub::safe_path(file)
            && file.to_ascii_lowercase().ends_with(".gguf")
            && size > 0
            && hex(sha256, 64)
    };
    added.id.starts_with("hf-")
        && added.id.len() <= 80
        && added
            .id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        && hub::parse(&added.repo).is_ok_and(|link| link.repo == added.repo && link.tag.is_none())
        && hex(&added.commit, 40)
        && pack(&added.file, added.size, &added.sha256)
        && added
            .eyes
            .as_ref()
            .is_none_or(|eyes| pack(&eyes.file, eyes.size, &eyes.sha256))
}

/// The entry for `added`, made once.
fn intern(added: &Added) -> &'static Entry {
    let mut pool = POOL.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, entry)) = pool.iter().find(|(kept, _)| kept == added) {
        return entry;
    }
    let entry: &'static Entry = Box::leak(Box::new(shape(added)));
    pool.push((added.clone(), entry));
    entry
}

fn leak(text: &str) -> &'static str {
    Box::leak(text.to_owned().into_boxed_str())
}

/// What the catalog would say of `added`, guessed where nobody measured.
fn shape(added: &Added) -> Entry {
    let repo = leak(&added.repo);
    let commit = leak(&added.commit);
    // A 4-bit file holds about five bits a parameter.
    let params = added.params.unwrap_or(added.size * 8 / 5);
    Entry {
        id: leak(&added.id),
        name: leak(&added.name),
        maker: leak(&added.maker),
        file: Pack {
            repo,
            commit,
            file: leak(&added.file),
            size: added.size,
            sha256: leak(&added.sha256),
        },
        smaller: None,
        eyes: added.eyes.as_ref().map(|eyes| Pack {
            repo,
            commit,
            file: leak(&eyes.file),
            size: eyes.size,
            sha256: leak(&eyes.sha256),
        }),
        kv_per_token: kv_per_token(params),
        fixed_state: 50 * MB,
        active: active(&added.repo, &added.file, params),
        // Below every model Letronna offers, which have been measured.
        rank: 0,
        native_context: added.context.filter(|&c| c > 0).unwrap_or(fit::CONTEXT),
        sampling: PLAIN,
        thinking: Thinking::Off,
        tools: Tools::Chat,
    }
}

/// KV cache a token holds, from the parameter count: a dense model with
/// grouped-query attention at the top of its size (an 8B Llama holds 128
/// KiB), so a verdict errs towards needing more memory than it does.
fn kv_per_token(params: u64) -> u64 {
    let billions = params as f64 / 1e9;
    let kib = match billions {
        b if b < 2.0 => 64,
        b if b < 5.0 => 128,
        b if b < 10.0 => 160,
        b if b < 20.0 => 192,
        b if b < 40.0 => 256,
        _ => 320,
    };
    kib * KIB
}

/// The share of the weights a token reads: a mixture of experts says so in
/// its name, "35B-A3B", and is otherwise taken as dense.
fn active(repo: &str, file: &str, params: u64) -> f64 {
    let billions = |part: &str| {
        let part = part.to_ascii_lowercase();
        part.strip_suffix('b')?
            .parse::<f64>()
            .ok()
            .filter(|n| *n > 0.0)
    };
    for name in [file.rsplit('/').next().unwrap_or(file), repo] {
        let parts: Vec<&str> = name.split(['-', '_', '/']).collect();
        for (i, part) in parts.iter().enumerate() {
            let Some(on) = part.strip_prefix(['A', 'a']).and_then(billions) else {
                continue;
            };
            let total = i
                .checked_sub(1)
                .and_then(|j| billions(parts[j]))
                .unwrap_or(params as f64 / 1e9);
            if total > on {
                // Plus the attention and embeddings every token reads.
                return (on / total * 1.15).min(1.0);
            }
        }
    }
    1.0
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::store::tests::{DATA_DIR, Scratch};

    pub(crate) fn qwen() -> Added {
        Added {
            id: "hf-unsloth-qwen3.5-2b-q4-k-m".into(),
            name: "Qwen3.5-2B · Q4_K_M".into(),
            maker: "unsloth".into(),
            repo: "unsloth/Qwen3.5-2B-GGUF".into(),
            commit: "f6d5376be1edb4d416d56da11e5397a961aca8ae".into(),
            file: "Qwen3.5-2B-Q4_K_M.gguf".into(),
            size: 1_280_835_840,
            sha256: "aaf42c8b7c3cab2bf3d69c355048d4a0ee9973d48f16c731c0520ee914699223".into(),
            eyes: Some(Eyes {
                file: "mmproj-F16.gguf".into(),
                size: 668_227_264,
                sha256: "7035e9cb8d7c6a9681d07eef9a364783e86ea4cd73faab2eabb4f43a101830c7".into(),
            }),
            params: Some(1_881_825_088),
            context: Some(262_144),
        }
    }

    fn scratch(name: &str) -> (std::sync::MutexGuard<'static, ()>, Scratch) {
        let lock = DATA_DIR.lock().unwrap_or_else(PoisonError::into_inner);
        let scratch = Scratch::new(name);
        // SAFETY: tests that set the data folder hold DATA_DIR.
        unsafe { std::env::set_var("AGENT_DATA_DIR", scratch.as_path()) };
        (lock, scratch)
    }

    #[test]
    fn a_model_added_is_offered_pinned_beside_the_catalog_until_forgotten() {
        let (_lock, _scratch) = scratch("added-list");
        assert!(all().is_empty());
        // Shown before it is kept, and kept as the same entry.
        let shown = preview(&qwen());
        assert!(all().is_empty());
        let entry = add(&qwen()).unwrap();
        assert!(std::ptr::eq(entry, shown));
        assert_eq!(entry.id, "hf-unsloth-qwen3.5-2b-q4-k-m");
        assert!(is_added(entry));
        assert!(!is_added(
            super::super::catalog::entry("bonsai-2-4b").unwrap()
        ));
        assert_eq!(
            entry.file.url(),
            "https://huggingface.co/unsloth/Qwen3.5-2B-GGUF/resolve/f6d5376be1edb4d416d56da11e5397a961aca8ae/Qwen3.5-2B-Q4_K_M.gguf"
        );
        assert_eq!(
            entry.page(),
            "https://huggingface.co/unsloth/Qwen3.5-2B-GGUF"
        );
        assert_eq!(entry.tools, Tools::Chat);
        assert_eq!(entry.native_context, 262_144);
        assert!(entry.eyes.is_some());
        // The same file twice is one model, and the same entry.
        assert!(std::ptr::eq(add(&qwen()).unwrap(), entry));
        assert!(std::ptr::eq(keep(preview(&qwen())).unwrap(), entry));
        assert_eq!(all(), [entry]);
        assert_eq!(super::super::catalog::entry(entry.id), Some(entry));
        assert!(super::super::catalog::all().contains(&entry));
        // Another file under the same id is told apart.
        let mut other = qwen();
        other.file = "q/Qwen3.5-2B-Q4_K_M.gguf".into();
        other.sha256 = "b".repeat(64);
        assert_eq!(add(&other).unwrap().id, "hf-unsloth-qwen3.5-2b-q4-k-m-2");
        assert_eq!(all().len(), 2);
        forget(entry.id).unwrap();
        assert_eq!(all().len(), 1);
        assert_eq!(super::super::catalog::entry(entry.id), None);
    }

    #[test]
    fn a_list_edited_to_reach_outside_the_models_folder_is_ignored() {
        let (_lock, _scratch) = scratch("added-sane");
        let mut bad = qwen();
        bad.file = "../../evil.gguf".into();
        assert!(add(&bad).is_err());
        let mut catalog_id = qwen();
        catalog_id.id = "bonsai-2-4b".into();
        std::fs::create_dir_all(shelf::dir()).unwrap();
        std::fs::write(
            list_path(),
            serde_json::to_vec(&[bad, catalog_id, qwen()]).unwrap(),
        )
        .unwrap();
        let ids: Vec<&str> = all().iter().map(|e| e.id).collect();
        assert_eq!(ids, ["hf-unsloth-qwen3.5-2b-q4-k-m"]);
    }

    #[test]
    fn what_nobody_measured_is_guessed_on_the_side_of_needing_more() {
        assert_eq!(kv_per_token(1_900_000_000), 64 * KIB);
        assert_eq!(kv_per_token(8_000_000_000), 160 * KIB);
        assert_eq!(kv_per_token(70_000_000_000), 320 * KIB);
        let moe = active(
            "unsloth/Qwen3.6-35B-A3B-GGUF",
            "Qwen3.6-35B-A3B-UD-Q4_K_XL.gguf",
            0,
        );
        assert!((0.09..0.11).contains(&moe), "{moe}");
        assert_eq!(
            active("o/Llama-3-8B-GGUF", "Llama-3-8B-Q4_K_M.gguf", 8_000_000_000),
            1.0
        );
        let unnamed = active("o/x-A3B", "x.gguf", 30_000_000_000);
        assert!((0.1..0.13).contains(&unnamed), "{unnamed}");
    }
}
