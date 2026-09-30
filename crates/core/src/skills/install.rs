//! Adding skills from elsewhere: a GitHub repository, a link to a Markdown
//! file, or a file or folder on this computer.
//!
//! GitHub is read through its API: the commit a branch points at, then the
//! whole tree at that commit in one request. The files of each folder
//! holding a SKILL.md come from raw.githubusercontent.com at that same
//! commit, so a push partway through cannot mix two versions, and those
//! downloads do not count against the API's hourly limit.
//!
//! Everything is gathered in a staging folder and checked before anything
//! in the user's skills folder changes; if placing one then fails, what was
//! placed is taken out and what was archived put back. An installed skill
//! is marked with `.installed-from`, so installing it again is an update,
//! which archives the earlier copy. A skill of the user's own is never
//! replaced by an installed one. Nothing that arrives is run.

use super::{
    ARCHIVE, FILE_BYTES, SKILL_BYTES, SKILL_FILE, SKILLS_DIR, Skill, Source, archive, device,
    field, find, free_folder, migrate_in, read_root, read_skill, same, slug, split,
};
use crate::chat as backend;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Files one install takes at most, all its skills together. The whole of
/// github.com/anthropics/skills is a little over 400.
const MAX_FILES: usize = 1_000;
/// Bytes one install takes at most, all its skills together; that same
/// repository is about 11 MB.
const MAX_BYTES: u64 = 50_000_000;
/// Downloads from raw.githubusercontent.com at once.
const DOWNLOADS: usize = 8;
/// How deep a folder on this computer is searched for SKILL.md files.
const SEARCH_DEPTH: usize = 6;
/// Folders never searched or copied: history, dependencies, build output.
const SKIPPED: &[&str] = &[".git", "node_modules", "target"];
/// Where an install gathers its files, in the data folder.
const INCOMING: &str = "skills-incoming";
/// The file in an installed skill's folder saying where it came from. Hidden,
/// so it is neither listed to the model nor copied by another install.
pub(super) const MARK: &str = ".installed-from";
/// A staging folder this old was left by an install that never finished.
const STALE: Duration = Duration::from_secs(60 * 60);

/// What an install did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Installed {
    /// Skills that were not there before, by name. They start switched on.
    pub added: Vec<String>,
    /// Skills installed over an earlier install of the same name, each with
    /// where the earlier copy was archived. Each keeps its switch.
    pub updated: Vec<(String, PathBuf)>,
    /// Skills left out, each with why: the user has one of their own by that
    /// name.
    pub refused: Vec<String>,
}

/// Where skills come from.
#[derive(Debug, PartialEq)]
enum Origin {
    GitHub {
        owner: String,
        repo: String,
        /// From a tree or blob link: the branch or tag and then the path,
        /// which cannot be told apart until the repository's branches are
        /// known. Empty for the default branch.
        after: Vec<String>,
        /// A folder or file under the default branch, from owner/repo/path.
        path: String,
    },
    /// Any other link: one Markdown file.
    Web(reqwest::Url),
    Local(PathBuf),
}

/// Add the skills at `source` to the user's folder and say what happened.
/// `source` is a GitHub repository (`owner/repo`, or a link to it or to a
/// folder or file in it), any other https link to a SKILL.md or Markdown
/// file, or a SKILL.md, Markdown file or folder on this computer. Nothing
/// changes unless every skill in it can be placed. This waits on the
/// network, so call it off the UI thread.
pub fn install(source: &str) -> Result<Installed, String> {
    install_in(&backend::data_dir(), source)
}

pub(super) fn install_in(data: &Path, source: &str) -> Result<Installed, String> {
    // So a first install does not stop the built-in skills or an old
    // skills.json from ever arriving; a failure is `migrate`'s to report.
    let _ = migrate_in(data);
    sweep(data);
    let staging = staging_folder(data);
    let placed = fetch(data, source, &staging).and_then(|folders| place(data, source, &folders));
    // Whatever is left is a copy of what came in, so a failure to tidy it
    // away changes nothing that matters. The folder above stays while
    // another install is using it.
    let _ = std::fs::remove_dir_all(&staging);
    let _ = std::fs::remove_dir(data.join(INCOMING));
    placed
}

/// A folder of its own for one install, so two at once cannot mix.
fn staging_folder(data: &Path) -> PathBuf {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    data.join(INCOMING)
        .join(format!("{}-{}-{n}", std::process::id(), backend::now()))
}

/// Take away what installs left in staging when the app closed partway, an
/// hour or more ago. A newer one may belong to an install still running.
fn sweep(data: &Path) {
    let Ok(entries) = std::fs::read_dir(data.join(INCOMING)) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| at.elapsed().ok())
            .is_some_and(|age| age > STALE);
        if old {
            let path = entry.path();
            let _ = std::fs::remove_dir_all(&path).or_else(|_| std::fs::remove_file(&path));
        }
    }
}

/// Gather the skills at `source` into `staging`, one folder each, named as a
/// skill with no name in its frontmatter would be called.
fn fetch(data: &Path, source: &str, staging: &Path) -> Result<Vec<PathBuf>, String> {
    match origin(source)? {
        Origin::GitHub {
            owner,
            repo,
            after,
            path,
        } => from_github(&owner, &repo, &after, &path, staging),
        Origin::Web(url) => {
            let client = client()?;
            let (bytes, kind) = get(&client, url.clone(), url.as_str())?;
            if kind.contains("html") {
                return Err(format!(
                    "{url} is a web page, not a SKILL.md or Markdown file. Link to the file \
                     itself, or to its raw version."
                ));
            }
            let segments: Vec<String> = url
                .path_segments()
                .map(|segments| segments.map(decode).collect())
                .unwrap_or_default();
            let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
            single(staging, &segments, bytes, url.as_str()).map(|folder| vec![folder])
        }
        Origin::Local(path) => from_disk(data, &path, staging),
    }
}

/// What `source` names. A path that exists on this computer wins over the
/// same words read as a GitHub repository.
fn origin(source: &str) -> Result<Origin, String> {
    let source = source.trim().trim_matches('"').trim();
    if source.is_empty() {
        return Err("Say where from: a GitHub repository as owner/repo, a link, or a path.".into());
    }
    // A repository's address as it is often pasted, with no scheme.
    let lower = source.to_lowercase();
    if lower.starts_with("github.com/") || lower.starts_with("www.github.com/") {
        return origin(&format!("https://{source}"));
    }
    if source.contains("://") {
        let url = reqwest::Url::parse(source)
            .map_err(|_| format!("`{source}` is not a link that can be read."))?;
        let github = matches!(url.host_str(), Some("github.com" | "www.github.com"));
        // GitHub is read over https whatever the link says.
        if github && matches!(url.scheme(), "https" | "http") {
            return github_link(&url);
        }
        if url.scheme() != "https" {
            return Err("Skills are fetched over https only.".into());
        }
        return Ok(Origin::Web(url));
    }
    let path = home_expanded(source);
    if path.exists() {
        return Ok(Origin::Local(path));
    }
    let parts: Vec<&str> = source.split('/').collect();
    let named = parts.len() >= 2
        && parts.iter().all(|part| {
            !matches!(*part, "" | "." | "..")
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        });
    if !named {
        return Err(format!(
            "Nothing at `{source}`. Give a GitHub repository as owner/repo, a link, or a \
             SKILL.md, Markdown file or folder on this computer."
        ));
    }
    Ok(Origin::GitHub {
        owner: parts[0].to_owned(),
        repo: parts[1].trim_end_matches(".git").to_owned(),
        after: Vec::new(),
        path: parts[2..].join("/"),
    })
}

/// A github.com link: to a repository, or through `tree` or `blob` to a
/// branch and a folder or file in it.
fn github_link(url: &reqwest::Url) -> Result<Origin, String> {
    let parts: Vec<String> = url
        .path_segments()
        .map(|segments| segments.filter(|s| !s.is_empty()).map(decode).collect())
        .unwrap_or_default();
    let unknown =
        || format!("{url} is not a link to a repository, nor to a folder or file in one.");
    let [owner, repo, rest @ ..] = parts.as_slice() else {
        return Err(unknown());
    };
    let after = match rest {
        [] => Vec::new(),
        [kind, after @ ..] if !after.is_empty() && matches!(kind.as_str(), "tree" | "blob") => {
            after.to_vec()
        }
        _ => return Err(unknown()),
    };
    Ok(Origin::GitHub {
        owner: owner.clone(),
        repo: repo.trim_end_matches(".git").to_owned(),
        after,
        path: String::new(),
    })
}

/// A link's path segment as it reads, `%20` and the like decoded.
fn decode(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let escaped = (bytes[at] == b'%')
            .then(|| segment.get(at + 1..at + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match escaped {
            Some(byte) => {
                out.push(byte);
                at += 3;
            }
            None => {
                out.push(bytes[at]);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A path typed with `~` for the home folder, as a shell would read it.
fn home_expanded(path: &str) -> PathBuf {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    match (path.strip_prefix("~/").or(path.strip_prefix("~\\")), home) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

/// The folders holding a SKILL.md among `paths`, `/`-separated. One inside
/// another skill's folder is one of that skill's files, not a skill.
fn skill_folders<'a>(paths: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut folders: Vec<String> = paths
        .filter(|p| is_skill_file(p))
        .filter(|p| !p.split('/').any(|part| SKIPPED.contains(&part)))
        .map(|p| {
            p.rsplit_once('/')
                .map_or("", |(folder, _)| folder)
                .to_owned()
        })
        .collect();
    folders.sort();
    let mut kept: Vec<String> = Vec::new();
    for folder in folders {
        let inside = kept
            .iter()
            .any(|k| k.is_empty() || folder.starts_with(&format!("{k}/")));
        if !inside {
            kept.push(folder);
        }
    }
    kept
}

/// Whether a path ends in a SKILL.md, in any case: a repository made on a
/// system that does not mind may call it skill.md.
fn is_skill_file(path: &str) -> bool {
    path.rsplit(['/', '\\'])
        .next()
        .is_some_and(|name| name.eq_ignore_ascii_case(SKILL_FILE))
}

/// A `/`-separated path from a repository or a folder being copied, as one to
/// write under a skill's folder, or None when a part of it could not be
/// written the same everywhere. The skill's own SKILL.md is always named so,
/// whatever its case, since that is the name it is read by.
pub(crate) fn safe(path: &str) -> Option<PathBuf> {
    if !path.contains('/') && is_skill_file(path) {
        return Some(PathBuf::from(SKILL_FILE));
    }
    let mut out = PathBuf::new();
    for part in path.split('/') {
        if !plain(part) {
            return None;
        }
        out.push(part);
    }
    Some(out)
}

/// Whether `part` names a file or folder the same on every system: not
/// empty, `.` or `..`; no character Windows forbids; not ending in a dot or a
/// space, which Windows drops; and not a device's name, which Windows opens
/// as the device.
pub(crate) fn plain(part: &str) -> bool {
    !matches!(part, "" | "." | "..")
        && !part.ends_with(['.', ' '])
        && !part
            .chars()
            .any(|c| c.is_control() || "<>:\"|?*\\".contains(c))
        && !device(part)
}

/// Why a file cannot be installed: its name.
pub(crate) fn unwritable(file: &str) -> String {
    format!(
        "{file} has a name that cannot be written on every computer -- a device's name such as \
         CON, a character Windows forbids, or a dot or space at the end -- so nothing was \
         installed."
    )
}

/// Whether a file inside a skill's folder is left behind: hidden files and
/// folders, and the ones in `SKIPPED`.
fn left_out(inside: &str) -> bool {
    inside
        .split('/')
        .any(|part| part.starts_with('.') || SKIPPED.contains(&part))
}

/// Refuse an install over `MAX_FILES` or `MAX_BYTES`, with a file over
/// `FILE_BYTES`, or with a SKILL.md over the `SKILL_BYTES` a skill is read
/// to, before anything is fetched or copied.
pub(crate) fn within_limits<'a>(files: impl Iterator<Item = (&'a str, u64)>) -> Result<(), String> {
    let (mut count, mut total) = (0, 0);
    for (path, size) in files {
        if size > FILE_BYTES {
            return Err(format!(
                "{path} is {size} bytes, and a skill's files are at most {FILE_BYTES} each."
            ));
        }
        if is_skill_file(path) && size > SKILL_BYTES {
            return Err(format!(
                "{path} is {size} bytes, and a SKILL.md is at most {SKILL_BYTES}."
            ));
        }
        count += 1;
        total += size;
    }
    if count > MAX_FILES {
        return Err(format!(
            "That is {count} files, more than the {MAX_FILES} one install takes. Pick one \
             skill's folder instead."
        ));
    }
    if total > MAX_BYTES {
        return Err(format!(
            "That is {total} bytes, more than the {MAX_BYTES} one install takes. Pick one \
             skill's folder instead."
        ));
    }
    Ok(())
}

/// One Markdown file as a skill by itself, in a folder named for it: the
/// folder above a SKILL.md, or the file's own name less `.md`.
fn single(
    staging: &Path,
    segments: &[&str],
    bytes: Vec<u8>,
    what: &str,
) -> Result<PathBuf, String> {
    let text = String::from_utf8(bytes).map_err(|_| format!("{what} is not UTF-8 text."))?;
    let name = match segments {
        [.., folder, file] if is_skill_file(file) => (*folder).to_owned(),
        [.., file] => {
            // Measured on the name itself: lowercasing can change its length.
            let stem = file
                .len()
                .checked_sub(3)
                .filter(|at| {
                    file.get(*at..)
                        .is_some_and(|end| end.eq_ignore_ascii_case(".md"))
                })
                .map_or(*file, |at| &file[..at]);
            stem.trim_end_matches(['.', ' ']).to_owned()
        }
        [] => String::new(),
    };
    let name = if plain(&name) {
        name
    } else {
        "skill".to_owned()
    };
    let folder = staging.join("0").join(name);
    std::fs::create_dir_all(&folder)
        .and_then(|()| std::fs::write(folder.join(SKILL_FILE), text))
        .map_err(|e| format!("Could not write to {}: {e}", folder.display()))?;
    Ok(folder)
}

/// The client every download goes through: https only, redirects included.
pub(crate) fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .user_agent(concat!("Letronna/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("too many redirects")
            } else if attempt.url().scheme() != "https" {
                attempt.error("skills are fetched over https only")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|e| format!("Could not start fetching: {e}"))
}

/// A file from the web, whole, and what kind the server says it is. Refused
/// past `FILE_BYTES`.
pub(crate) fn get(
    client: &reqwest::blocking::Client,
    url: reqwest::Url,
    what: &str,
) -> Result<(Vec<u8>, String), String> {
    let response = client
        .get(url)
        .send()
        .map_err(|e| format!("Could not fetch {what}: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "Fetching {what} answered HTTP {}.",
            status.as_u16()
        ));
    }
    let kind = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut bytes = Vec::new();
    response
        .take(FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{what} stopped arriving: {e}"))?;
    if bytes.len() as u64 > FILE_BYTES {
        return Err(format!(
            "{what} is over {FILE_BYTES} bytes, more than a skill's file may be."
        ));
    }
    Ok((bytes, kind))
}

/// An address at `base` made of `parts`, each escaped.
fn address(base: &str, parts: &[&str]) -> Result<reqwest::Url, String> {
    let mut url = reqwest::Url::parse(base).map_err(|e| e.to_string())?;
    url.path_segments_mut()
        .map_err(|()| "Could not make a GitHub address.".to_owned())?
        .extend(parts);
    Ok(url)
}

/// One answer from GitHub's API about `name`, with the errors people meet
/// said plainly: a repository that is not there or not public, and the
/// hourly limit on requests without an account.
fn api(
    client: &reqwest::blocking::Client,
    parts: &[&str],
    query: Option<&str>,
    name: &str,
) -> Result<serde_json::Value, String> {
    let mut url = address("https://api.github.com/", parts)?;
    url.set_query(query);
    let response = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .map_err(|e| format!("Could not reach GitHub: {e}"))?;
    let status = response.status().as_u16();
    let header = |key: &str| {
        response
            .headers()
            .get(key)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let spent = header("x-ratelimit-remaining").as_deref() == Some("0");
    match status {
        200 => response
            .json()
            .map_err(|e| format!("GitHub's answer about {name} did not read: {e}")),
        403 | 429 if spent || status == 429 => {
            let minutes = header("x-ratelimit-reset")
                .and_then(|at| at.parse::<u64>().ok())
                .map(|at| at.saturating_sub(backend::now()) / 60 + 1);
            Err(match minutes {
                Some(minutes) => format!(
                    "GitHub takes 60 requests an hour from a computer without an account, and \
                     this one has used them. Try again in {minutes} minutes."
                ),
                None => "GitHub takes 60 requests an hour from a computer without an account, \
                         and this one has used them. Try again later."
                    .to_owned(),
            })
        }
        404 | 422 => Err(format!(
            "GitHub has no public repository {name}, or not that branch."
        )),
        409 => Err(format!("{name} is empty.")),
        _ => Err(format!("GitHub answered HTTP {status} about {name}.")),
    }
}

/// Which of `after` is the branch or tag and which the path, given the names
/// of the repository's branches or tags that start with its first part: the
/// longest that fits, as GitHub reads its own links, or else the first part.
fn split_reference(names: &[String], after: &[String]) -> (String, String) {
    let fits = |name: &&String| {
        let parts: Vec<&str> = name.split('/').collect();
        parts.len() <= after.len() && parts.iter().zip(after).all(|(a, b)| *a == b.as_str())
    };
    let reference = names
        .iter()
        .filter(fits)
        .max_by_key(|name| name.split('/').count())
        .cloned()
        .unwrap_or_else(|| after.first().cloned().unwrap_or_default());
    let used = reference.split('/').count().min(after.len());
    (reference, after[used..].join("/"))
}

/// The branch or tag a link names, and the path after it. Only a link with a
/// path needs asking: a branch name may have `/` in it.
fn reference_of(
    client: &reqwest::blocking::Client,
    owner: &str,
    repo: &str,
    after: &[String],
    name: &str,
) -> Result<(String, String), String> {
    let Some(first) = after.first() else {
        return Ok(("HEAD".to_owned(), String::new()));
    };
    if after.len() == 1 {
        return Ok((first.clone(), String::new()));
    }
    let mut names = Vec::new();
    for kind in ["heads", "tags"] {
        let refs = api(
            client,
            &["repos", owner, repo, "git", "matching-refs", kind, first],
            None,
            name,
        )?;
        let prefix = format!("refs/{kind}/");
        names.extend(
            refs.as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r["ref"].as_str()?.strip_prefix(&prefix).map(str::to_owned)),
        );
        if !names.is_empty() {
            break;
        }
    }
    Ok(split_reference(&names, after))
}

/// The skills in a GitHub repository, under the path the source names when
/// it names one, each folder's files fetched into `staging`.
fn from_github(
    owner: &str,
    repo: &str,
    after: &[String],
    path: &str,
    staging: &Path,
) -> Result<Vec<PathBuf>, String> {
    let client = client()?;
    let name = format!("{owner}/{repo}");
    let (reference, linked) = reference_of(&client, owner, repo, after, &name)?;
    let path = if after.is_empty() {
        path
    } else {
        linked.as_str()
    };
    let mut parts = vec!["repos", owner, repo, "commits"];
    parts.extend(reference.split('/'));
    let commit = api(&client, &parts, None, &name)?;
    let (Some(sha), Some(tree_sha)) = (
        commit["sha"].as_str(),
        commit["commit"]["tree"]["sha"].as_str(),
    ) else {
        return Err(format!("GitHub did not say which commit {name} is at."));
    };
    let tree = api(
        &client,
        &["repos", owner, repo, "git", "trees", tree_sha],
        Some("recursive=1"),
        &name,
    )?;
    if tree["truncated"].as_bool() == Some(true) {
        return Err(format!(
            "{name} is too big for GitHub to list in one go, so its skills cannot be found \
             this way. Download it and add the folder instead."
        ));
    }
    // Links are left out: raw.githubusercontent.com serves the link itself.
    let blobs: Vec<(String, u64)> = tree["tree"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| entry["type"] == "blob" && entry["mode"] != "120000")
        .filter_map(|entry| {
            let path = entry["path"].as_str()?.to_owned();
            Some((path, entry["size"].as_u64().unwrap_or(0)))
        })
        .collect();
    let path = path.trim_matches('/');
    let under = if path.is_empty() {
        String::new()
    } else {
        format!(" under {path}")
    };

    // A link to one Markdown file that is not a SKILL.md is a skill by itself.
    if path.to_lowercase().ends_with(".md") && !is_skill_file(path) {
        let Some((_, size)) = blobs.iter().find(|(p, _)| p == path) else {
            return Err(format!("{name} has no file {path} on {reference}."));
        };
        within_limits(std::iter::once((path, *size)))?;
        let url = address(
            "https://raw.githubusercontent.com/",
            &raw_parts(owner, repo, sha, path),
        )?;
        let (bytes, _) = get(&client, url, path)?;
        let segments: Vec<&str> = path.split('/').collect();
        return single(staging, &segments, bytes, path).map(|folder| vec![folder]);
    }

    let wanted = |p: &str| path.is_empty() || p == path || p.starts_with(&format!("{path}/"));
    let folders = skill_folders(blobs.iter().map(|(p, _)| p.as_str()).filter(|p| wanted(p)));
    if folders.is_empty() {
        return Err(format!("There is no SKILL.md in {name}{under}."));
    }
    let mut files: Vec<(&str, u64, PathBuf)> = Vec::new();
    let mut staged = Vec::new();
    for (n, folder) in folders.iter().enumerate() {
        let leaf = folder
            .rsplit('/')
            .next()
            .filter(|l| !l.is_empty())
            .unwrap_or(repo);
        let into = staging
            .join(n.to_string())
            .join(safe(leaf).unwrap_or_else(|| "skill".into()));
        for (file, size) in &blobs {
            let inside = match folder.as_str() {
                "" => Some(file.as_str()),
                folder => file
                    .strip_prefix(folder)
                    .and_then(|rest| rest.strip_prefix('/')),
            };
            let Some(inside) = inside.filter(|inside| !left_out(inside)) else {
                continue;
            };
            let relative = safe(inside).ok_or_else(|| unwritable(&format!("{name}: {file}")))?;
            files.push((file.as_str(), *size, into.join(relative)));
        }
        staged.push(into);
    }
    within_limits(files.iter().map(|(file, size, _)| (*file, *size)))?;
    download_all(&client, owner, repo, sha, &files)?;
    Ok(staged)
}

/// A file's place on raw.githubusercontent.com, at one commit.
fn raw_parts<'a>(owner: &'a str, repo: &'a str, sha: &'a str, path: &'a str) -> Vec<&'a str> {
    [owner, repo, sha]
        .into_iter()
        .chain(path.split('/'))
        .collect()
}

/// Fetch each file into place, `DOWNLOADS` at a time, stopping at the first
/// that fails.
fn download_all(
    client: &reqwest::blocking::Client,
    owner: &str,
    repo: &str,
    sha: &str,
    files: &[(&str, u64, PathBuf)],
) -> Result<(), String> {
    let queue = Mutex::new(files.iter());
    let failed: Mutex<Option<String>> = Mutex::new(None);
    std::thread::scope(|scope| {
        for _ in 0..DOWNLOADS {
            scope.spawn(|| {
                loop {
                    if !failed.lock().is_ok_and(|failed| failed.is_none()) {
                        return;
                    }
                    let Some((path, _, to)) = queue.lock().ok().and_then(|mut queue| queue.next())
                    else {
                        return;
                    };
                    let fetched = address(
                        "https://raw.githubusercontent.com/",
                        &raw_parts(owner, repo, sha, path),
                    )
                    .and_then(|url| get(client, url, path))
                    .and_then(|(bytes, _)| {
                        to.parent()
                            .map_or(Ok(()), std::fs::create_dir_all)
                            .and_then(|()| std::fs::write(to, bytes))
                            .map_err(|e| format!("Could not write {}: {e}", to.display()))
                    });
                    if let Err(why) = fetched {
                        if let Ok(mut failed) = failed.lock() {
                            failed.get_or_insert(why);
                        }
                        return;
                    }
                }
            });
        }
    });
    match failed.into_inner() {
        Ok(Some(why)) => Err(why),
        Ok(None) => Ok(()),
        Err(_) => Err("A download stopped partway.".into()),
    }
}

/// The skills in a file or folder on this computer, copied into `staging`: a
/// SKILL.md brings its folder, another Markdown file is a skill by itself,
/// and a folder brings every skill folder in it.
fn from_disk(data: &Path, path: &Path, staging: &Path) -> Result<Vec<PathBuf>, String> {
    let shown = path.display();
    let full = std::fs::canonicalize(path).map_err(|e| format!("Could not read {shown}: {e}"))?;
    in_own_folder(data, &full, &shown.to_string())?;
    let folders: Vec<PathBuf> = if full.is_file() {
        let name = full
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !is_skill_file(&name) {
            if !name.to_lowercase().ends_with(".md") {
                return Err(format!("{shown} is not a SKILL.md or Markdown file."));
            }
            let size = std::fs::metadata(&full).map_or(0, |m| m.len());
            within_limits(std::iter::once((name.as_str(), size)))?;
            let mut bytes = Vec::new();
            std::fs::File::open(&full)
                .and_then(|file| file.take(FILE_BYTES).read_to_end(&mut bytes))
                .map_err(|e| format!("Could not read {shown}: {e}"))?;
            return single(staging, &[name.as_str()], bytes, &shown.to_string())
                .map(|folder| vec![folder]);
        }
        full.parent().map(Path::to_path_buf).into_iter().collect()
    } else {
        let mut found = Vec::new();
        find_skill_files(&full, 0, &mut found);
        let relative: Vec<String> = found
            .iter()
            .filter_map(|file| file.strip_prefix(&full).ok())
            .map(slashed)
            .collect();
        skill_folders(relative.iter().map(String::as_str))
            .iter()
            .map(|folder| match folder.as_str() {
                "" => full.clone(),
                folder => full.join(folder),
            })
            .collect()
    };
    if folders.is_empty() {
        return Err(format!("There is no SKILL.md in {shown}."));
    }
    let mut files: Vec<(String, u64, PathBuf, PathBuf)> = Vec::new();
    let mut staged = Vec::new();
    for (n, folder) in folders.iter().enumerate() {
        let leaf = folder
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "skill".into());
        let into = staging
            .join(n.to_string())
            .join(safe(&leaf).unwrap_or_else(|| "skill".into()));
        let mut inside = Vec::new();
        files_in(folder, 0, &mut inside);
        for file in inside {
            let relative = file.strip_prefix(folder).map(slashed).unwrap_or_default();
            let to = safe(&relative).ok_or_else(|| unwritable(&file.display().to_string()))?;
            let size = std::fs::metadata(&file).map_or(0, |m| m.len());
            files.push((relative, size, file, into.join(to)));
        }
        staged.push(into);
    }
    within_limits(
        files
            .iter()
            .map(|(relative, size, ..)| (relative.as_str(), *size)),
    )?;
    for (_, _, from, to) in &files {
        to.parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::copy(from, to))
            .map_err(|e| format!("Could not copy {}: {e}", from.display()))?;
    }
    Ok(staged)
}

/// Refuse a source in Letronna's own folder, where the user's skills, the
/// archive and the keys are, except one archived skill being brought back.
fn in_own_folder(data: &Path, full: &Path, shown: &str) -> Result<(), String> {
    let Some(own) = std::fs::canonicalize(data)
        .ok()
        .filter(|own| full.starts_with(own))
    else {
        return Ok(());
    };
    let archived = std::fs::canonicalize(data.join(ARCHIVE))
        .ok()
        .and_then(|archive| full.strip_prefix(archive).ok().map(Path::to_path_buf));
    let one = archived.is_some_and(|inside| {
        let depth = inside.components().count();
        depth == 1 || (depth == 2 && is_skill_file(&inside.to_string_lossy()))
    });
    if one {
        return Ok(());
    }
    if full.starts_with(own.join(SKILLS_DIR)) {
        return Err(format!("{shown} is in your skills folder already."));
    }
    Err(format!(
        "{shown} is in Letronna's own folder. Only one skill at a time can be brought back from \
         its {ARCHIVE} folder."
    ))
}

/// A path inside a folder with `/` between its parts, as a repository's are.
fn slashed(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Every SKILL.md under `folder`, to `SEARCH_DEPTH`, never through a link or
/// into `SKIPPED`.
fn find_skill_files(folder: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() && depth < SEARCH_DEPTH && !SKIPPED.contains(&name.as_str()) {
            find_skill_files(&entry.path(), depth + 1, found);
        } else if kind.is_file() && is_skill_file(&name) {
            found.push(entry.path());
        }
    }
}

/// The files of one skill's folder, hidden ones and `SKIPPED` folders left
/// out, never through a link; stops once there are more than an install
/// takes, which `within_limits` then says.
fn files_in(folder: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        if found.len() > MAX_FILES {
            return;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if left_out(&name) {
            continue;
        }
        if kind.is_dir() && depth < SEARCH_DEPTH {
            files_in(&entry.path(), depth + 1, found);
        } else if kind.is_file() {
            found.push(entry.path());
        }
    }
}

/// One staged skill and where it goes.
struct Placing {
    from: PathBuf,
    skill: Skill,
    /// The earlier install of it, archived to make way.
    replacing: Option<PathBuf>,
}

/// Move staged skills into the user's folder, once every one of them has been
/// read and checked, and say what happened. A skill of the user's own by the
/// same name is left alone and the installed one refused.
fn place(data: &Path, source: &str, staged: &[PathBuf]) -> Result<Installed, String> {
    let root = data.join(SKILLS_DIR);
    std::fs::create_dir_all(&root)
        .map_err(|e| format!("Could not make {}: {e}", root.display()))?;
    let mine = read_root(&root, Source::User);
    let mut plan: Vec<Placing> = Vec::new();
    let mut refused = Vec::new();
    for from in staged {
        let skill = checked(from)?;
        // Two of one name in one install: the first is kept.
        if plan.iter().any(|p| same(&p.skill.name, &skill.name)) {
            continue;
        }
        let replacing = match find(&mine, &skill.name) {
            None => None,
            Some(old) if old.folder.join(MARK).is_file() => Some(old.folder.clone()),
            Some(old) => {
                refused.push(format!(
                    "\"{}\" is one of your own skills, so the one from {source} was left out. \
                     Rename or remove yours to install it.",
                    old.name
                ));
                continue;
            }
        };
        plan.push(Placing {
            from: from.clone(),
            skill,
            replacing,
        });
    }
    if plan.is_empty() {
        return Err(refused.join(" "));
    }
    for placing in &plan {
        std::fs::write(placing.from.join(MARK), format!("{source}\n"))
            .map_err(|e| format!("Could not write to {}: {e}", placing.from.display()))?;
    }
    let mut archived: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut moved: Vec<PathBuf> = Vec::new();
    match apply(data, &root, &plan, &mut archived, &mut moved) {
        Ok(mut installed) => {
            installed.refused = refused;
            Ok(installed)
        }
        Err(why) => Err(match undo(&moved, &archived) {
            None => format!("{why} Nothing was changed."),
            Some(stuck) => format!("{why} {stuck}"),
        }),
    }
}

/// A staged skill, read as it will be listed, or why it cannot be.
fn checked(from: &Path) -> Result<Skill, String> {
    let label = from.file_name().unwrap_or_default().to_string_lossy();
    let file = from.join(SKILL_FILE);
    let size = std::fs::metadata(&file)
        .map_err(|e| format!("Could not read the SKILL.md of {label}: {e}"))?
        .len();
    if size > SKILL_BYTES {
        return Err(format!(
            "The SKILL.md of {label} is {size} bytes, and a SKILL.md is at most {SKILL_BYTES}."
        ));
    }
    read_skill(from, Source::User)
        .ok_or_else(|| format!("The SKILL.md of {label} is not UTF-8 text."))
}

/// Archive what the plan replaces and move the new skills in, noting each
/// step in `archived` and `moved` so a failure can be undone.
fn apply(
    data: &Path,
    root: &Path,
    plan: &[Placing],
    archived: &mut Vec<(PathBuf, PathBuf)>,
    moved: &mut Vec<PathBuf>,
) -> Result<Installed, String> {
    let mut installed = Installed::default();
    for placing in plan {
        if let Some(old) = &placing.replacing {
            archived.push((old.clone(), archive(data, old)?));
        }
    }
    for placing in plan {
        let skill = &placing.skill;
        // One not named in its frontmatter is called what its folder is, so
        // that folder keeps its name; an update takes the earlier one's place.
        let text = std::fs::read_to_string(placing.from.join(SKILL_FILE)).unwrap_or_default();
        let target = match (&placing.replacing, field(&split(&text).0, "name")) {
            (Some(old), _) => old.clone(),
            (None, Some(_)) => free_folder(root, &slug(&skill.name)),
            (None, None) => free_folder(
                root,
                &placing
                    .from
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
            ),
        };
        std::fs::rename(&placing.from, &target).map_err(|e| {
            format!(
                "Could not move {} into {}: {e}.",
                skill.name,
                root.display()
            )
        })?;
        moved.push(target.clone());
        // As listed from now on, which a folder name already taken changes.
        let name = read_skill(&target, Source::User).map_or(skill.name.clone(), |s| s.name);
        match archived.iter().find(|(old, _)| *old == target) {
            Some((_, kept)) => installed.updated.push((name, kept.clone())),
            None => installed.added.push(name),
        }
    }
    Ok(installed)
}

/// Take out the skills moved in and put the archived ones back. None when
/// all of it went back, or else what could not.
fn undo(moved: &[PathBuf], archived: &[(PathBuf, PathBuf)]) -> Option<String> {
    let mut stuck = Vec::new();
    for target in moved.iter().rev() {
        if let Err(e) = std::fs::remove_dir_all(target) {
            stuck.push(format!(
                "{} could not be taken out again: {e}.",
                target.display()
            ));
        }
    }
    for (original, kept) in archived.iter().rev() {
        if let Err(e) = std::fs::rename(kept, original) {
            stuck.push(format!(
                "{} could not be put back from {}: {e}.",
                original.display(),
                kept.display()
            ));
        }
    }
    (!stuck.is_empty()).then(|| stuck.join(" "))
}

#[cfg(test)]
mod tests {
    use super::{
        Installed, Origin, decode, install_in, origin, safe, single, skill_folders,
        split_reference, within_limits,
    };
    use crate::skills::{create_in, list_in, set_enabled_in};
    use std::path::{Path, PathBuf};

    fn scratch(name: &str) -> PathBuf {
        // The process's id too, so two checkouts testing at once keep apart.
        let dir = std::env::temp_dir().join(format!(
            "letronna-skill-install-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn github(owner: &str, repo: &str, after: &[&str], path: &str) -> Origin {
        Origin::GitHub {
            owner: owner.into(),
            repo: repo.into(),
            after: after.iter().map(|a| (*a).to_owned()).collect(),
            path: path.into(),
        }
    }

    fn added(names: &[&str]) -> Installed {
        Installed {
            added: names.iter().map(|n| (*n).to_owned()).collect(),
            ..Default::default()
        }
    }

    fn body(data: &Path, name: &str) -> String {
        list_in(data, None)
            .unwrap()
            .into_iter()
            .find(|s| s.name == name)
            .map(|s| s.body)
            .unwrap_or_default()
    }

    #[test]
    fn sources_are_told_apart() {
        let here = scratch("origin");
        assert_eq!(
            origin("anthropics/skills").unwrap(),
            github("anthropics", "skills", &[], "")
        );
        assert_eq!(
            origin("anthropics/skills/skills/pdf").unwrap(),
            github("anthropics", "skills", &[], "skills/pdf")
        );
        assert_eq!(
            origin(" https://github.com/anthropics/skills.git ").unwrap(),
            github("anthropics", "skills", &[], "")
        );
        // Pasted without a scheme, or with plain http.
        for pasted in [
            "github.com/anthropics/skills",
            "www.GitHub.com/anthropics/skills",
            "http://github.com/anthropics/skills",
        ] {
            assert_eq!(
                origin(pasted).unwrap(),
                github("anthropics", "skills", &[], ""),
                "{pasted}"
            );
        }
        assert_eq!(
            origin("https://github.com/a/b/tree/main/skills/pdf/").unwrap(),
            github("a", "b", &["main", "skills", "pdf"], "")
        );
        assert_eq!(
            origin("https://github.com/a/b/blob/v1/my%20skill/SKILL.md").unwrap(),
            github("a", "b", &["v1", "my skill", "SKILL.md"], "")
        );
        let raw = "https://raw.githubusercontent.com/a/b/main/pdf/SKILL.md";
        assert_eq!(
            origin(raw).unwrap(),
            Origin::Web(reqwest::Url::parse(raw).unwrap())
        );
        assert_eq!(
            origin(&here.display().to_string()).unwrap(),
            Origin::Local(here)
        );
        for refused in [
            "",
            "http://example.com/SKILL.md",
            "https://github.com/a/b/issues/3",
            "https://github.com/a/b/tree",
            "https://github.com/a",
            "./not/there",
            "C:/not/there",
            "one-word",
        ] {
            assert!(origin(refused).is_err(), "{refused}");
        }
    }

    #[test]
    fn link_paths_are_read_as_they_are_written() {
        assert_eq!(decode("my%20skill"), "my skill");
        assert_eq!(decode("r%C3%A9sum%C3%A9"), "résumé");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz"), "%zz");
    }

    #[test]
    fn a_branch_with_a_slash_in_its_name_is_told_from_the_path() {
        let after: Vec<String> = ["feature", "x", "skills", "pdf"]
            .iter()
            .map(|a| (*a).to_owned())
            .collect();
        let names = vec![
            "feature".to_owned(),
            "feature/x".to_owned(),
            "feature/y".to_owned(),
        ];
        assert_eq!(
            split_reference(&names, &after),
            ("feature/x".to_owned(), "skills/pdf".to_owned())
        );
        assert_eq!(
            split_reference(&[], &after),
            ("feature".to_owned(), "x/skills/pdf".to_owned())
        );
    }

    #[test]
    fn skill_folders_are_found_and_nested_ones_belong_to_their_parent() {
        let paths = [
            "README.md",
            "skills/pdf/SKILL.md",
            "skills/pdf/scripts/fill.py",
            "skills/pdf/examples/inner/SKILL.md",
            "skills/pdf-extra/skill.md",
            "node_modules/x/SKILL.md",
            ".claude/skills/review/SKILL.md",
        ];
        assert_eq!(
            skill_folders(paths.into_iter()),
            [".claude/skills/review", "skills/pdf", "skills/pdf-extra"]
        );
        assert_eq!(
            skill_folders(["SKILL.md", "sub/SKILL.md"].into_iter()),
            [""]
        );
    }

    #[test]
    fn paths_that_would_leave_the_folder_or_misbehave_on_windows_are_refused() {
        assert_eq!(
            safe("scripts/fill.py"),
            Some(PathBuf::from("scripts").join("fill.py"))
        );
        assert_eq!(safe("skill.md"), Some(PathBuf::from("SKILL.md")));
        assert_eq!(
            safe("docs/skill.md"),
            Some(PathBuf::from("docs").join("skill.md"))
        );
        for bad in [
            "../keys.json",
            "a/../../b",
            "",
            "a//b",
            "C:x",
            "a\\b",
            "./a",
            "a/\u{7}",
            "CON",
            "docs/aux.md",
            "COM\u{b9}.txt",
            "trail.",
            "trail ",
            "a<b",
            "what?",
        ] {
            assert_eq!(safe(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn an_install_too_big_is_refused_before_anything_is_fetched() {
        let many = (0..1_001).map(|_| ("a.md", 1));
        assert!(within_limits(many).unwrap_err().contains("1001 files"));
        let big = std::iter::once(("big.bin", 3_000_000));
        assert!(within_limits(big).unwrap_err().contains("big.bin"));
        let heavy = (0..40).map(|_| ("a.bin", 1_900_000));
        assert!(within_limits(heavy).unwrap_err().contains("bytes"));
        let long = std::iter::once(("b/SKILL.md", 1_500_000));
        assert!(within_limits(long).unwrap_err().contains("SKILL.md"));
        assert!(within_limits((0..400).map(|_| ("a.md", 25_000))).is_ok());
    }

    #[test]
    fn a_markdown_file_is_named_without_its_md() {
        let staging = scratch("names");
        let named = |file: &str| {
            let folder = single(&staging, &[file], b"Body".to_vec(), file).unwrap();
            let name = folder.file_name().unwrap().to_string_lossy().into_owned();
            std::fs::remove_dir_all(&folder).unwrap();
            name
        };
        // Lowercasing the Kelvin sign changes its length in bytes.
        assert_eq!(named("\u{212a}.md"), "\u{212a}");
        assert_eq!(named("İstanbul.md"), "İstanbul");
        assert_eq!(named("notes..MD"), "notes");
        assert_eq!(named("con.md"), "skill");
        assert!(
            single(&staging, &["x.md"], vec![0xff, 0xfe], "x.md")
                .unwrap_err()
                .contains("not UTF-8")
        );
    }

    #[test]
    fn a_folder_on_this_computer_installs_every_skill_in_it() {
        let data = scratch("folder-data");
        let from = scratch("folder-source");
        write(
            &from.join("pdf/SKILL.md"),
            "---\nname: pdf\ndescription: Forms.\n---\nUse scripts/fill.py.",
        );
        write(&from.join("pdf/scripts/fill.py"), "print('filled')");
        write(&from.join("pdf/.cache/x"), "hidden");
        write(
            &from.join("pdf/examples/inner/SKILL.md"),
            "---\nname: inner\n---\nPart of pdf.",
        );
        write(&from.join("tools/review/skill.md"), "Review with care.");
        write(
            &from.join("node_modules/dep/SKILL.md"),
            "---\nname: dep\n---\nNot mine.",
        );

        let mut installed = install_in(&data, &from.display().to_string()).unwrap();
        installed.added.sort();
        assert_eq!(installed, added(&["pdf", "review"]));
        let pdf = data.join("skills/pdf");
        assert!(pdf.join("scripts/fill.py").is_file());
        assert!(pdf.join("examples/inner/SKILL.md").is_file());
        assert!(pdf.join(".installed-from").is_file());
        assert!(!pdf.join(".cache").exists());
        assert!(!data.join("skills-incoming").exists());
        // A skill.md in lower case arrives under the name it is read by.
        let review: Vec<String> = std::fs::read_dir(data.join("skills/review"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(review.contains(&"SKILL.md".to_owned()), "{review:?}");

        let listed = list_in(&data, None).unwrap();
        let get = |name: &str| listed.iter().find(|s| s.name == name).unwrap();
        assert!(get("pdf").enabled && get("review").enabled);
        assert_eq!(get("review").description, "Review with care.");
        // The source is copied, not moved.
        assert!(from.join("pdf/SKILL.md").is_file());
    }

    #[test]
    fn installing_again_updates_and_keeps_the_switch() {
        let data = scratch("again-data");
        let from = scratch("again-source");
        write(&from.join("pdf/SKILL.md"), "---\nname: pdf\n---\nFirst.");
        install_in(&data, &from.join("pdf/SKILL.md").display().to_string()).unwrap();
        set_enabled_in(&data, "pdf", false).unwrap();
        write(&from.join("pdf/SKILL.md"), "---\nname: PDF\n---\nSecond.");
        let installed = install_in(&data, &from.display().to_string()).unwrap();
        assert!(installed.added.is_empty());
        let [(name, kept)] = installed.updated.as_slice() else {
            panic!("{installed:?}");
        };
        assert_eq!(name, "PDF");
        assert!(
            std::fs::read_to_string(kept.join("SKILL.md"))
                .unwrap()
                .contains("First.")
        );
        assert_eq!(body(&data, "PDF"), "Second.");
        assert!(
            list_in(&data, None)
                .unwrap()
                .iter()
                .any(|s| s.name == "PDF" && !s.enabled)
        );
    }

    #[test]
    fn a_skill_of_your_own_is_never_replaced_by_an_installed_one() {
        let data = scratch("own-name-data");
        let from = scratch("own-name-source");
        create_in(&data, "pdf", "", "Mine.").unwrap();
        write(&from.join("pdf/SKILL.md"), "---\nname: pdf\n---\nTheirs.");
        write(&from.join("docx/SKILL.md"), "---\nname: docx\n---\nTheirs.");
        let installed = install_in(&data, &from.display().to_string()).unwrap();
        assert_eq!(installed.added, ["docx"]);
        assert_eq!(installed.refused.len(), 1);
        assert!(installed.refused[0].contains("\"pdf\" is one of your own"));
        assert_eq!(body(&data, "pdf"), "Mine.");
        // Nothing left to install is an error.
        let only = install_in(&data, &from.join("pdf/SKILL.md").display().to_string());
        assert!(only.unwrap_err().contains("one of your own"));
    }

    #[test]
    fn a_failed_install_changes_nothing() {
        let data = scratch("all-or-nothing-data");
        let from = scratch("all-or-nothing-source");
        write(&from.join("alpha/SKILL.md"), "---\nname: alpha\n---\nOld.");
        install_in(&data, &from.display().to_string()).unwrap();
        write(&from.join("alpha/SKILL.md"), "---\nname: alpha\n---\nNew.");
        std::fs::create_dir_all(from.join("beta")).unwrap();
        std::fs::write(from.join("beta/SKILL.md"), [b'-', 0xff, 0xfe]).unwrap();

        let error = install_in(&data, &from.display().to_string()).unwrap_err();
        assert!(error.contains("not UTF-8"), "{error}");
        assert_eq!(body(&data, "alpha"), "Old.");
        assert!(!data.join("skills-archive").exists());
        assert!(!names(&data).contains(&"beta".to_owned()));
    }

    fn names(data: &Path) -> Vec<String> {
        list_in(data, None)
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect()
    }

    // Windows will not make such a file to test with; the check is the same
    // everywhere.
    #[cfg(unix)]
    #[test]
    fn a_file_named_for_a_windows_device_stops_the_install() {
        let data = scratch("device-data");
        let from = scratch("device-source");
        write(&from.join("x/SKILL.md"), "---\nname: x\n---\nBody");
        write(&from.join("x/CON"), "to the console");
        let error = install_in(&data, &from.display().to_string()).unwrap_err();
        assert!(
            error.contains("CON") && error.contains("nothing was installed"),
            "{error}"
        );
        assert!(!data.join("skills/x").exists());
    }

    #[test]
    fn a_markdown_file_is_a_skill_by_itself() {
        let data = scratch("markdown-data");
        let from = scratch("markdown-source");
        write(
            &from.join("Release notes.md"),
            "# Notes\n\nWrite them from the log.",
        );
        write(&from.join("notes.txt"), "Not Markdown.");
        let installed =
            install_in(&data, &from.join("Release notes.md").display().to_string()).unwrap();
        assert_eq!(installed, added(&["Release notes"]));
        let skill = list_in(&data, None)
            .unwrap()
            .into_iter()
            .find(|s| s.name == "Release notes")
            .unwrap();
        assert_eq!(skill.description, "Write them from the log.");
        assert_eq!(skill.folder, data.join("skills").join("Release notes"));
        assert!(install_in(&data, &from.join("notes.txt").display().to_string()).is_err());
    }

    #[test]
    fn letronnas_own_folder_is_no_source_but_for_one_archived_skill() {
        let data = scratch("own-data");
        create_in(&data, "Keep", "", "Kept.").unwrap();
        let own = data.join("skills").display().to_string();
        assert!(install_in(&data, &own).unwrap_err().contains("already"));
        let whole = install_in(&data, &data.display().to_string()).unwrap_err();
        assert!(whole.contains("Letronna's own folder"), "{whole}");

        let archived = crate::skills::remove_in(&data, "Keep").unwrap();
        let back = install_in(&data, &archived.display().to_string()).unwrap();
        assert_eq!(back.added, ["Keep"]);
        assert_eq!(body(&data, "Keep"), "Kept.");

        let empty = scratch("own-empty");
        write(&empty.join("README.md"), "Hello");
        let error = install_in(&data, &empty.display().to_string()).unwrap_err();
        assert!(error.contains("no SKILL.md"), "{error}");
    }

    #[test]
    fn staging_left_by_an_install_that_never_finished_is_swept() {
        let data = scratch("sweep-data");
        let from = scratch("sweep-source");
        write(&from.join("x/SKILL.md"), "Body");
        let left = data.join("skills-incoming").join("left");
        write(&left, "half");
        let hours_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(7_200);
        std::fs::File::options()
            .write(true)
            .open(&left)
            .unwrap()
            .set_modified(hours_ago)
            .unwrap();
        install_in(&data, &from.display().to_string()).unwrap();
        assert!(!left.exists());
    }

    #[test]
    #[ignore = "Downloads from GitHub: cargo test -p letronna-core --release -- --ignored anthropics"]
    fn installs_the_skills_in_anthropics_skills() {
        let data = scratch("anthropics");
        let installed = install_in(&data, "anthropics/skills").unwrap();
        println!("{installed:?}");
        assert!(installed.added.len() >= 15, "{installed:?}");
        assert!(installed.added.iter().any(|n| n == "pdf"));
        assert!(data.join("skills/pdf/SKILL.md").is_file());
        assert!(data.join("skills/pdf/scripts").is_dir());

        let one = install_in(&data, "github.com/anthropics/skills/tree/main/skills/docx").unwrap();
        assert_eq!(one.updated.len(), 1, "{one:?}");
        assert_eq!(one.updated[0].0, "docx");
        let raw = "https://raw.githubusercontent.com/anthropics/skills/main/skills/pdf/SKILL.md";
        let pdf = install_in(&data, raw).unwrap();
        assert_eq!(pdf.updated[0].0, "pdf");
        for skill in list_in(&data, None).unwrap() {
            println!("{}  {}", skill.name, skill.description);
        }
    }
}
