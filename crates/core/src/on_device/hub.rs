//! Models from a Hugging Face link: reading what was pasted, and asking the
//! hub what the repository holds at this moment.
//!
//! A link names a repository, and sometimes a revision, a file or a
//! quantization tag (`owner/repo:Q4_K_M`, as llama.cpp and Ollama take it).
//! The answer is pinned there and then: the commit the revision points at,
//! and each file's size and SHA-256 from its LFS pointer. A file added from
//! a link is fetched and checked exactly as the catalog's are, so a copy
//! that differs from what the hub said is thrown away, never run.
//!
//! Only single-file GGUF models are offered: split files would need the
//! shelf to carry one model in several downloads, and a model behind a
//! sign-in would need a Hugging Face token Letronna does not keep.

use super::added::Added;
use std::time::Duration;

/// Where the hub is.
pub const HUB: &str = "https://huggingface.co";

/// The page a repository's model card is on.
pub fn page(repo: &str) -> String {
    format!("{HUB}/{repo}")
}

/// What a pasted link asks for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Link {
    /// `owner/repo`.
    pub repo: String,
    /// A branch, tag or commit; the default branch when none.
    pub revision: Option<String>,
    /// One file in it, by its path in the repository.
    pub file: Option<String>,
    /// A folder in it, as a tree link names one.
    pub folder: Option<String>,
    /// A quantization, from `owner/repo:Q4_K_M`.
    pub tag: Option<String>,
}

/// Said when what was pasted is not a link to a model on the hub.
pub const NOT_A_LINK: &str =
    "That isn't a Hugging Face model link. Paste one like huggingface.co/owner/model-GGUF.";

/// Read a pasted link: `https://huggingface.co/owner/repo`, its `tree`,
/// `blob` or `resolve` addresses, `hf.co/owner/repo:Q4_K_M`, or a bare
/// `owner/repo[:TAG]`.
pub fn parse(text: &str) -> Result<Link, String> {
    let text = text.trim().trim_matches(['"', '\'', '<', '>']);
    let text = text.split(['?', '#']).next().unwrap_or_default();
    let rest = ["https://", "http://", "hf://"]
        .iter()
        .find_map(|scheme| {
            text.get(..scheme.len())
                .filter(|head| head.eq_ignore_ascii_case(scheme))
                .map(|_| &text[scheme.len()..])
        })
        .unwrap_or(text);
    let (first, path) = rest.split_once('/').unwrap_or((rest, ""));
    let path = match first.to_ascii_lowercase().as_str() {
        "huggingface.co" | "www.huggingface.co" | "hf.co" => path,
        // Anywhere else with a dot in its first part is another site.
        host if host.contains('.') || host.contains(':') && path.contains('/') => {
            return Err(NOT_A_LINK.into());
        }
        _ => rest,
    };
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    match parts.first().copied() {
        Some("datasets" | "spaces" | "collections" | "papers" | "docs" | "blog") => {
            return Err(
                "That link is to a page that isn't a model. Paste the link of a model's page."
                    .into(),
            );
        }
        Some("models") => return Err(NOT_A_LINK.into()),
        _ => {}
    }
    let [owner, name, more @ ..] = parts.as_slice() else {
        return Err(NOT_A_LINK.into());
    };
    let (name, tag) = match name.split_once(':') {
        Some((name, tag)) if !tag.is_empty() => (name, Some(tag.to_owned())),
        Some((name, _)) => (name, None),
        None => (*name, None),
    };
    if !plain(owner) || !plain(name) {
        return Err(NOT_A_LINK.into());
    }
    let mut link = Link {
        repo: format!("{owner}/{name}"),
        tag,
        ..Link::default()
    };
    if let [kind @ ("tree" | "blob" | "resolve"), revision, inside @ ..] = more {
        link.revision = Some((*revision).to_owned());
        let inside = inside.join("/");
        if inside.is_empty() {
        } else if *kind == "tree" {
            link.folder = Some(inside);
        } else if !inside.to_ascii_lowercase().ends_with(".gguf") {
            return Err(
                "That file isn't a GGUF model, the format Letronna runs. Link to a .gguf file or \
                 to the model's page."
                    .into(),
            );
        } else if vision(&inside) {
            return Err(
                "That file is a model's photo reading, not the model. Link to the model itself; \
                 photo reading comes with it."
                    .into(),
            );
        } else {
            link.file = Some(inside);
        }
    }
    Ok(link)
}

/// Whether a name is an owner or repository name the hub allows.
fn plain(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 96
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && !name.starts_with('.')
}

/// Whether a file is a vision pack rather than a model.
fn vision(file: &str) -> bool {
    file.to_ascii_lowercase().contains("mmproj")
}

/// Whether a file is one part of a model split across several.
fn split_part(file: &str) -> bool {
    let lower = file.to_ascii_lowercase();
    let Some(stem) = lower.strip_suffix(".gguf") else {
        return false;
    };
    // `-00001-of-00003`
    let Some(tail) = stem.len().checked_sub(15).and_then(|at| stem.get(at..)) else {
        return false;
    };
    let t = tail.as_bytes();
    t[0] == b'-'
        && t[1..6].iter().all(u8::is_ascii_digit)
        && &t[6..10] == b"-of-"
        && t[10..].iter().all(u8::is_ascii_digit)
}

/// Whether a path in a repository can be kept under the models folder as
/// it is, and asked for in a URL as it is.
pub(crate) fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 200
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+'))
        })
}

/// The quantization a file's name ends with: "Q4_K_M", "UD-Q4_K_XL",
/// "IQ4_XS", "BF16", "MXFP4".
pub fn quant(file: &str) -> Option<String> {
    let name = file.rsplit('/').next().unwrap_or(file);
    let stem = name
        .get(..name.len().saturating_sub(5))
        .filter(|_| name.to_ascii_lowercase().ends_with(".gguf"))
        .unwrap_or(name);
    let parts: Vec<&str> = stem.split(['-', '.']).collect();
    let i = parts.iter().rposition(|part| is_quant(part))?;
    let found = parts[i].to_ascii_uppercase();
    match i.checked_sub(1).map(|j| parts[j]) {
        Some(ud) if ud.eq_ignore_ascii_case("ud") => Some(format!("UD-{found}")),
        _ => Some(found),
    }
}

fn is_quant(part: &str) -> bool {
    let p = part.to_ascii_lowercase();
    let digit_after = |head: &str| {
        p.strip_prefix(head)
            .and_then(|tail| tail.chars().next())
            .is_some_and(|c| c.is_ascii_digit())
    };
    digit_after("q")
        || digit_after("iq")
        || digit_after("tq")
        || matches!(p.as_str(), "f16" | "bf16" | "f32" | "fp16" | "mxfp4")
}

/// The quantizations taken first when a link names none: 4-bit K-quants
/// lose little and halve an 8-bit file, then the nearest either side.
const PREFERRED: &[&str] = &[
    "Q4_K_M",
    "Q4_K_S",
    "UD-Q4_K_XL",
    "Q4_0",
    "IQ4_XS",
    "IQ4_NL",
    "MXFP4",
    "Q5_K_M",
    "Q5_K_S",
    "Q6_K",
    "Q8_0",
];

/// The vision packs taken first: Q8_0 is as good as F16 at half the size,
/// and BF16 can crash llama-server when several images come at once.
const PREFERRED_EYES: &[&str] = &["Q8_0", "F16", "BF16", "F32"];

/// What a repository offers to run here, as the hub answered.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// `owner/repo`, as the hub spells it.
    pub repo: String,
    /// The commit every file is pinned to.
    pub commit: String,
    pub license: Option<String>,
    /// One model per file that can be downloaded, smallest first.
    pub choices: Vec<Added>,
    /// The one to offer first: what the link named, or the preferred.
    pub picked: usize,
    /// Model files split into parts, left out.
    pub split: usize,
}

/// The hub's answer about a repository at a revision, as far as it is read.
#[derive(serde::Deserialize)]
struct Info {
    id: Option<String>,
    sha: Option<String>,
    #[serde(default)]
    gated: serde_json::Value,
    #[serde(default)]
    disabled: bool,
    #[serde(rename = "cardData", default)]
    card: Option<Card>,
    #[serde(default)]
    gguf: Option<Gguf>,
    #[serde(default)]
    siblings: Vec<Sibling>,
}

#[derive(serde::Deserialize)]
struct Card {
    #[serde(default)]
    license: serde_json::Value,
}

#[derive(serde::Deserialize)]
struct Gguf {
    total: Option<u64>,
    context_length: Option<u64>,
}

#[derive(serde::Deserialize)]
struct Sibling {
    rfilename: String,
    lfs: Option<Lfs>,
}

#[derive(serde::Deserialize)]
struct Lfs {
    sha256: Option<String>,
    size: Option<u64>,
}

/// A file with what its LFS pointer says of it.
struct Listed {
    path: String,
    size: u64,
    sha256: String,
}

/// Ask the hub what `link` holds, off the window's thread: it waits on the
/// network.
pub fn look(link: &Link) -> Result<Found, String> {
    look_at(HUB, link)
}

fn look_at(hub: &str, link: &Link) -> Result<Found, String> {
    let revision = link.revision.as_deref().unwrap_or("main");
    let url = format!(
        "{hub}/api/models/{}/revision/{}?blobs=true",
        link.repo,
        revision.replace('/', "%2F")
    );
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("Letronna/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(&url)
        .send()
        .map_err(|_| "Couldn't reach Hugging Face. Check the connection and try again.")?;
    match response.status().as_u16() {
        200 => {}
        401 | 403 | 404 => {
            return Err(match &link.revision {
                Some(revision) => format!(
                    "Hugging Face has no public model {} at {revision}. Check the link; private \
                     models can't be added.",
                    link.repo
                ),
                None => format!(
                    "Hugging Face has no public model called {}. Check the link; private models \
                     can't be added.",
                    link.repo
                ),
            });
        }
        429 => return Err("Hugging Face is busy. Try again in a minute.".into()),
        status => return Err(format!("Hugging Face answered {status}. Try again later.")),
    }
    let info: Info = response
        .json()
        .map_err(|_| "Hugging Face's answer could not be read. Try again later.")?;
    read(link, info)
}

/// What a repository offers, from the hub's answer.
fn read(link: &Link, info: Info) -> Result<Found, String> {
    // As the hub spells it, when that is a name a link could hold.
    let repo = info
        .id
        .filter(|id| parse(id).is_ok_and(|named| named.repo == *id && named.tag.is_none()))
        .unwrap_or_else(|| link.repo.clone());
    if info.disabled {
        return Err(format!("{repo} has been disabled on Hugging Face."));
    }
    if !matches!(
        info.gated,
        serde_json::Value::Null | serde_json::Value::Bool(false)
    ) {
        return Err(format!(
            "{repo} asks you to sign in on Hugging Face and accept its terms first, which \
             Letronna can't do for you. Look for an ungated GGUF copy of it."
        ));
    }
    let Some(commit) = info
        .sha
        .filter(|sha| sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
    else {
        return Err("Hugging Face didn't say which commit that is. Try again later.".into());
    };
    let license = info.card.and_then(|card| match card.license {
        serde_json::Value::String(text) => Some(text),
        serde_json::Value::Array(list) => list
            .first()
            .and_then(|first| first.as_str())
            .map(str::to_owned),
        _ => None,
    });
    let (params, context) = info
        .gguf
        .map_or((None, None), |g| (g.total, g.context_length));
    let listed: Vec<Listed> = info
        .siblings
        .into_iter()
        .filter(|s| s.rfilename.to_ascii_lowercase().ends_with(".gguf"))
        .filter(|s| safe_path(&s.rfilename))
        .filter_map(|s| {
            let lfs = s.lfs?;
            let sha256 = lfs.sha256.filter(|h| h.len() == 64)?.to_ascii_lowercase();
            let size = lfs.size.filter(|&n| n > 0)?;
            Some(Listed {
                path: s.rfilename,
                size,
                sha256,
            })
        })
        .collect();
    let in_folder = |path: &str| {
        link.folder
            .as_ref()
            .is_none_or(|folder| path.starts_with(&format!("{folder}/")))
    };
    let eyes = PREFERRED_EYES
        .iter()
        .find_map(|want| {
            listed
                .iter()
                .find(|l| vision(&l.path) && quant(&l.path).as_deref() == Some(*want))
        })
        .or_else(|| {
            listed
                .iter()
                .filter(|l| vision(&l.path))
                .min_by_key(|l| l.size)
        });
    let models: Vec<&Listed> = listed
        .iter()
        .filter(|l| !vision(&l.path) && in_folder(&l.path))
        .collect();
    let split = models.iter().filter(|l| split_part(&l.path)).count();
    let mut whole: Vec<&Listed> = models
        .into_iter()
        .filter(|l| !split_part(&l.path))
        .collect();
    whole.sort_by_key(|l| l.size);
    if let Some(file) = &link.file {
        if split_part(file) {
            return Err(SPLIT.into());
        }
        if !whole.iter().any(|l| l.path == *file) {
            return Err(format!(
                "{repo} has no file {file} at this revision. Paste the model's page link to see \
                 what it has."
            ));
        }
    }
    if whole.is_empty() {
        return Err(if split > 0 {
            SPLIT.into()
        } else {
            format!(
                "{repo} has no GGUF file, the format Letronna runs. Look for a copy of it with \
                 GGUF in its name."
            )
        });
    }
    let picked = match (&link.file, &link.tag) {
        (Some(file), _) => whole.iter().position(|l| l.path == *file).unwrap_or(0),
        (None, Some(tag)) => whole
            .iter()
            .position(|l| {
                quant(&l.path).is_some_and(|q| {
                    q.eq_ignore_ascii_case(tag)
                        || q.strip_prefix("UD-")
                            .is_some_and(|q| q.eq_ignore_ascii_case(tag))
                })
            })
            .ok_or_else(|| {
                let offered: Vec<String> = whole.iter().filter_map(|l| quant(&l.path)).collect();
                format!(
                    "{repo} has no {tag} file. It has {}.",
                    if offered.is_empty() {
                        "other files".to_owned()
                    } else {
                        offered.join(", ")
                    }
                )
            })?,
        (None, None) => PREFERRED
            .iter()
            .find_map(|want| {
                whole
                    .iter()
                    .position(|l| quant(&l.path).as_deref() == Some(*want))
            })
            .unwrap_or(0),
    };
    let choices = whole
        .iter()
        .map(|l| Added {
            id: id(&repo, &l.path),
            name: name(&repo, &l.path),
            maker: repo.split('/').next().unwrap_or_default().to_owned(),
            repo: repo.clone(),
            commit: commit.clone(),
            file: l.path.clone(),
            size: l.size,
            sha256: l.sha256.clone(),
            eyes: eyes.map(|e| super::added::Eyes {
                file: e.path.clone(),
                size: e.size,
                sha256: e.sha256.clone(),
            }),
            params,
            context,
        })
        .collect();
    Ok(Found {
        repo,
        commit,
        license,
        choices,
        picked,
        split,
    })
}

/// Said when the only files are split into parts.
const SPLIT: &str = "That model is split into several files, which Letronna can't download \
                     yet. Pick a smaller quantization that comes as one file.";

/// What a model added from `repo`'s `file` is called: "Qwen3.5-9B · Q4_K_M".
pub fn name(repo: &str, file: &str) -> String {
    let base = repo.rsplit('/').next().unwrap_or(repo);
    let base = ["-gguf", "_gguf", ".gguf"]
        .iter()
        .find_map(|tail| {
            base.len()
                .checked_sub(tail.len())
                .filter(|&at| {
                    at > 0
                        && base
                            .get(at..)
                            .is_some_and(|end| end.eq_ignore_ascii_case(tail))
                })
                .and_then(|at| base.get(..at))
        })
        .unwrap_or(base);
    match quant(file) {
        Some(quant) => format!("{base} · {quant}"),
        None => {
            let name = file.rsplit('/').next().unwrap_or(file);
            name.get(..name.len().saturating_sub(5))
                .filter(|_| name.to_ascii_lowercase().ends_with(".gguf"))
                .unwrap_or(name)
                .to_owned()
        }
    }
}

/// The id a model added from `repo`'s `file` is kept under: a folder name
/// on every system, and never one of the catalog's.
pub fn id(repo: &str, file: &str) -> String {
    let owner = repo.split('/').next().unwrap_or_default();
    let name = file.rsplit('/').next().unwrap_or(file);
    let stem = name
        .get(..name.len().saturating_sub(5))
        .filter(|_| name.to_ascii_lowercase().ends_with(".gguf"))
        .unwrap_or(name);
    let mut slug = String::from("hf-");
    for c in format!("{owner}-{stem}").chars() {
        let c = c.to_ascii_lowercase();
        let c = if c.is_ascii_alphanumeric() || c == '.' {
            c
        } else {
            '-'
        };
        if !(c == '-' && slug.ends_with('-')) {
            slug.push(c);
        }
    }
    let mut slug: String = slug.trim_end_matches(['-', '.']).chars().take(72).collect();
    while slug.ends_with(['-', '.']) {
        slug.pop();
    }
    slug
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_auth::tests::serve;

    fn link(text: &str) -> Link {
        parse(text).unwrap_or_else(|why| panic!("{text}: {why}"))
    }

    #[test]
    fn every_way_of_naming_a_model_is_read() {
        let repo = |text: &str| link(text).repo;
        for text in [
            "https://huggingface.co/unsloth/Qwen3.5-9B-GGUF",
            "https://huggingface.co/unsloth/Qwen3.5-9B-GGUF/",
            "huggingface.co/unsloth/Qwen3.5-9B-GGUF",
            "HTTPS://HuggingFace.co/unsloth/Qwen3.5-9B-GGUF?library=llama-cpp#files",
            "https://www.huggingface.co/unsloth/Qwen3.5-9B-GGUF",
            "hf.co/unsloth/Qwen3.5-9B-GGUF",
            "  unsloth/Qwen3.5-9B-GGUF  ",
            "https://huggingface.co/unsloth/Qwen3.5-9B-GGUF/discussions/3",
        ] {
            assert_eq!(repo(text), "unsloth/Qwen3.5-9B-GGUF", "{text}");
        }
        assert_eq!(
            link("https://huggingface.co/unsloth/Qwen3.5-9B-GGUF/blob/main/Qwen3.5-9B-Q4_K_M.gguf"),
            Link {
                repo: "unsloth/Qwen3.5-9B-GGUF".into(),
                revision: Some("main".into()),
                file: Some("Qwen3.5-9B-Q4_K_M.gguf".into()),
                ..Link::default()
            }
        );
        let resolve = link(
            "https://huggingface.co/unsloth/Qwen3.5-9B-GGUF/resolve/3885219b6810b007914f3a7950a8d1b469d598a5/Qwen3.5-9B-Q4_K_M.gguf?download=true",
        );
        assert_eq!(
            resolve.revision.as_deref(),
            Some("3885219b6810b007914f3a7950a8d1b469d598a5")
        );
        assert_eq!(resolve.file.as_deref(), Some("Qwen3.5-9B-Q4_K_M.gguf"));
        let tree = link("https://huggingface.co/unsloth/big-GGUF/tree/main/UD-Q4_K_XL");
        assert_eq!(tree.folder.as_deref(), Some("UD-Q4_K_XL"));
        assert_eq!(tree.file, None);
        assert_eq!(
            link("hf.co/bartowski/gemma-GGUF:Q4_K_M").tag.as_deref(),
            Some("Q4_K_M")
        );
        assert_eq!(
            link("bartowski/gemma-GGUF:q8_0").tag.as_deref(),
            Some("q8_0")
        );
    }

    #[test]
    fn what_is_not_a_model_link_says_so() {
        for text in [
            "",
            "qwen",
            "https://github.com/ggml-org/llama.cpp",
            "https://huggingface.co/",
            "https://huggingface.co/unsloth",
            "https://huggingface.co/models?search=qwen",
            "../etc/passwd",
            "owner/re po",
        ] {
            assert_eq!(parse(text), Err(NOT_A_LINK.into()), "{text:?}");
        }
        assert!(
            parse("https://huggingface.co/datasets/owner/data")
                .unwrap_err()
                .contains("isn't a model")
        );
        assert!(
            parse("https://huggingface.co/o/r/blob/main/README.md")
                .unwrap_err()
                .contains("isn't a GGUF")
        );
        assert!(
            parse("https://huggingface.co/o/r/blob/main/mmproj-F16.gguf")
                .unwrap_err()
                .contains("photo reading")
        );
    }

    #[test]
    fn a_files_quantization_is_read_from_its_name() {
        for (file, want) in [
            ("Qwen3.5-2B-Q4_K_M.gguf", Some("Q4_K_M")),
            ("Qwen3.5-2B-UD-Q4_K_XL.gguf", Some("UD-Q4_K_XL")),
            ("Ternary-Bonsai-2-4B-Q2_0.gguf", Some("Q2_0")),
            ("Llama-3.2-1B-Instruct.Q4_K_M.gguf", Some("Q4_K_M")),
            ("phi-3-mini-4k-instruct-q4.gguf", Some("Q4")),
            ("Qwen3-Coder-Next-UD-IQ4_XS.gguf", Some("UD-IQ4_XS")),
            ("gpt-oss-20b-mxfp4.gguf", Some("MXFP4")),
            ("Qwen3.5-2B-BF16.gguf", Some("BF16")),
            ("Q4_K_M/model.gguf", None),
            ("model.gguf", None),
        ] {
            assert_eq!(quant(file).as_deref(), want, "{file}");
        }
    }

    #[test]
    fn a_model_from_a_link_is_named_by_its_repo_and_quantization() {
        assert_eq!(
            name("unsloth/Qwen3.5-9B-GGUF", "Qwen3.5-9B-Q4_K_M.gguf"),
            "Qwen3.5-9B · Q4_K_M"
        );
        assert_eq!(
            name("someone/tiny_gguf", "tiny-UD-Q8_K_XL.gguf"),
            "tiny · UD-Q8_K_XL"
        );
        assert_eq!(name("someone/GGUF", "model.gguf"), "model");
        assert_eq!(
            id("unsloth/Qwen3.5-9B-GGUF", "Qwen3.5-9B-Q4_K_M.gguf"),
            "hf-unsloth-qwen3.5-9b-q4-k-m"
        );
        assert_eq!(id("Some_One/x", "sub/A  B+C.gguf"), "hf-some-one-a-b-c");
        assert!(id("o/r", &format!("{}.gguf", "x".repeat(200))).len() <= 72);
        assert!(split_part("big-Q4_K_M-00001-of-00003.gguf"));
        assert!(!split_part("big-Q4_K_M.gguf"));
        // Names from a pasted link are not only ASCII.
        assert!(!split_part("模型模型模型模型模型.gguf"));
        assert_eq!(name("o/模型-gguf", "模型.gguf"), "模型");
        assert!(safe_path("UD-Q4_K_XL/model-Q4.gguf"));
        assert!(!safe_path("../model.gguf"));
        assert!(!safe_path("/model.gguf"));
        assert!(!safe_path("a b.gguf"));
    }

    const COMMIT: &str = "f6d5376be1edb4d416d56da11e5397a961aca8ae";

    fn sibling(path: &str, size: u64) -> serde_json::Value {
        serde_json::json!({
            "rfilename": path,
            "size": size,
            "lfs": {"sha256": format!("{:064x}", size), "size": size, "pointerSize": 135},
        })
    }

    fn hub(answer: serde_json::Value) -> String {
        let body = answer.to_string();
        serve(move |request| {
            assert!(request.target.contains("?blobs=true"), "{}", request.target);
            match request.target.as_str() {
                t if t.starts_with("/api/models/gone/") => (
                    401,
                    Vec::new(),
                    r#"{"error":"Invalid username or password."}"#.into(),
                ),
                _ => (
                    200,
                    vec![("Content-Type", "application/json".into())],
                    body.clone(),
                ),
            }
        })
    }

    fn qwen() -> serde_json::Value {
        serde_json::json!({
            "id": "unsloth/Qwen3.5-2B-GGUF",
            "sha": COMMIT,
            "gated": false,
            "cardData": {"license": "apache-2.0"},
            "gguf": {"total": 1_881_825_088u64, "context_length": 262_144},
            "siblings": [
                {"rfilename": "README.md", "size": 64059},
                sibling("Qwen3.5-2B-BF16.gguf", 3_775_709_216),
                sibling("Qwen3.5-2B-Q8_0.gguf", 2_012_012_800),
                sibling("Qwen3.5-2B-Q4_K_M.gguf", 1_280_835_840),
                sibling("Qwen3.5-2B-UD-IQ2_M.gguf", 859_857_152),
                sibling("mmproj-BF16.gguf", 671_372_992),
                sibling("mmproj-F16.gguf", 668_227_264),
                sibling("big/Qwen3.5-2B-F32-00001-of-00002.gguf", 4_000_000_000u64),
                {"rfilename": "no-lfs.gguf", "size": 10},
            ],
        })
    }

    #[test]
    fn a_repo_offers_each_whole_file_pinned_with_the_preferred_first() {
        let base = hub(qwen());
        let found = look_at(&base, &link("unsloth/Qwen3.5-2B-GGUF")).unwrap();
        assert_eq!(found.repo, "unsloth/Qwen3.5-2B-GGUF");
        assert_eq!(found.commit, COMMIT);
        assert_eq!(found.license.as_deref(), Some("apache-2.0"));
        assert_eq!(found.split, 1);
        let files: Vec<&str> = found.choices.iter().map(|c| c.file.as_str()).collect();
        assert_eq!(
            files,
            [
                "Qwen3.5-2B-UD-IQ2_M.gguf",
                "Qwen3.5-2B-Q4_K_M.gguf",
                "Qwen3.5-2B-Q8_0.gguf",
                "Qwen3.5-2B-BF16.gguf"
            ]
        );
        let picked = &found.choices[found.picked];
        assert_eq!(picked.file, "Qwen3.5-2B-Q4_K_M.gguf");
        assert_eq!(picked.name, "Qwen3.5-2B · Q4_K_M");
        assert_eq!(picked.maker, "unsloth");
        assert_eq!(picked.sha256, format!("{:064x}", 1_280_835_840u64));
        assert_eq!(picked.context, Some(262_144));
        assert_eq!(
            picked.eyes.as_ref().map(|e| e.file.as_str()),
            Some("mmproj-F16.gguf")
        );
        let tagged = look_at(&base, &link("hf.co/unsloth/Qwen3.5-2B-GGUF:q8_0")).unwrap();
        assert_eq!(tagged.choices[tagged.picked].file, "Qwen3.5-2B-Q8_0.gguf");
        let ud = look_at(&base, &link("unsloth/Qwen3.5-2B-GGUF:IQ2_M")).unwrap();
        assert_eq!(ud.choices[ud.picked].file, "Qwen3.5-2B-UD-IQ2_M.gguf");
        let named = look_at(
            &base,
            &link("https://huggingface.co/unsloth/Qwen3.5-2B-GGUF/blob/main/Qwen3.5-2B-BF16.gguf"),
        )
        .unwrap();
        assert_eq!(named.choices[named.picked].file, "Qwen3.5-2B-BF16.gguf");
        let missing = look_at(&base, &link("unsloth/Qwen3.5-2B-GGUF:Q3_K_S")).unwrap_err();
        assert!(
            missing.contains("UD-IQ2_M, Q4_K_M, Q8_0, BF16"),
            "{missing}"
        );
    }

    #[test]
    fn what_cannot_be_downloaded_says_why() {
        let gone = look_at(&hub(qwen()), &link("gone/model")).unwrap_err();
        assert!(gone.contains("no public model called gone/model"), "{gone}");
        let mut gated = qwen();
        gated["gated"] = "manual".into();
        let why = look_at(&hub(gated), &link("unsloth/Qwen3.5-2B-GGUF")).unwrap_err();
        assert!(why.contains("sign in"), "{why}");
        let mut none = qwen();
        none["siblings"] = serde_json::json!([{"rfilename": "model.safetensors", "size": 5}]);
        let why = look_at(&hub(none), &link("unsloth/Qwen3.5-2B-GGUF")).unwrap_err();
        assert!(why.contains("no GGUF file"), "{why}");
        let mut split = qwen();
        split["siblings"] = serde_json::json!([
            sibling("m-Q4_K_M-00001-of-00002.gguf", 5),
            sibling("m-Q4_K_M-00002-of-00002.gguf", 5),
        ]);
        assert_eq!(
            look_at(&hub(split), &link("unsloth/Qwen3.5-2B-GGUF")).unwrap_err(),
            SPLIT
        );
        let no_internet = look_at("http://127.0.0.1:9", &link("o/r")).unwrap_err();
        assert!(no_internet.contains("Couldn't reach"), "{no_internet}");
    }

    #[test]
    #[ignore = "Asks huggingface.co: cargo test -p letronna-core -- --ignored a_real_repo"]
    fn a_real_repo_is_read_and_pinned_as_the_catalog_pins_its_own() {
        let found = look(&link("https://huggingface.co/unsloth/Qwen3.5-9B-GGUF")).unwrap();
        let picked = &found.choices[found.picked];
        assert_eq!(picked.file, "Qwen3.5-9B-Q4_K_M.gguf");
        let known = super::super::catalog::entry("qwen-3.5-9b").unwrap();
        assert_eq!(picked.sha256, known.file.sha256);
        assert_eq!(picked.size, known.file.size);
        assert!(picked.eyes.is_some());
        assert_eq!(found.license.as_deref(), Some("apache-2.0"));
    }
}
