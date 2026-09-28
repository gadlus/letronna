//! Adding skills from elsewhere: a GitHub repository, a link to a Markdown
//! file, or a file or folder on this computer.
//!
//! GitHub is read through its API -- the default branch, then the whole tree
//! in one request -- and the files of each folder holding a SKILL.md come
//! from raw.githubusercontent.com, which does not count against the API's
//! hourly limit. An install of a whole repository is two API requests.
//!
//! Everything is gathered in a staging folder first and moved into the
//! user's skills folder only once it is whole, so a failed install leaves
//! nothing half there. A skill already there by the same name is archived
//! rather than overwritten. Nothing that arrives is run.

use super::{
    FILE_BYTES, SKILL_FILE, SKILLS_DIR, Source, archive, field, find, free_folder, migrate_in,
    read_root, read_skill, same, slug, split,
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

/// Where skills come from.
#[derive(Debug, PartialEq)]
enum Origin {
    /// A repository, and a folder or file in it; an empty path is all of it.
    GitHub {
        owner: String,
        repo: String,
        reference: Option<String>,
        path: String,
    },
    /// Any other link: one Markdown file.
    Web(reqwest::Url),
    Local(PathBuf),
}

/// Add the skills at `source` to the user's folder, switched on, and say
/// what they are called. `source` is a GitHub repository (`owner/repo`, or a
/// link to it or to a folder or file in it), any other https link to a
/// SKILL.md or Markdown file, or a SKILL.md, Markdown file or folder on this
/// computer. A skill with a name already taken replaces it, and the old one
/// goes to the archive. This waits on the network, so call it off the UI
/// thread.
pub fn install(source: &str) -> Result<Vec<String>, String> {
    install_in(&backend::data_dir(), source)
}

pub(super) fn install_in(data: &Path, source: &str) -> Result<Vec<String>, String> {
    // So a first install does not stop the built-in skills or an old
    // skills.json from ever arriving; a failure is `migrate`'s to report.
    let _ = migrate_in(data);
    let staging = staging_folder(data);
    let placed = fetch(data, source, &staging).and_then(|folders| place(data, &folders));
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

/// Gather the skills at `source` into `staging`, one folder each, named as a
/// skill with no name in its frontmatter would be called.
fn fetch(data: &Path, source: &str, staging: &Path) -> Result<Vec<PathBuf>, String> {
    match origin(source)? {
        Origin::GitHub {
            owner,
            repo,
            reference,
            path,
        } => from_github(&owner, &repo, reference, &path, staging),
        Origin::Web(url) => {
            let client = client()?;
            let (bytes, kind) = get(&client, url.clone(), url.as_str())?;
            if kind.contains("html") {
                return Err(format!(
                    "{url} is a web page, not a SKILL.md or Markdown file. Link to the file \
                     itself, or to its raw version."
                ));
            }
            let segments: Vec<&str> = url
                .path_segments()
                .map(Iterator::collect)
                .unwrap_or_default();
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
    if source.contains("://") {
        let url = reqwest::Url::parse(source)
            .map_err(|_| format!("`{source}` is not a link that can be read."))?;
        if url.scheme() != "https" {
            return Err("Skills are fetched over https only.".into());
        }
        if matches!(url.host_str(), Some("github.com" | "www.github.com")) {
            return github_link(&url);
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
        reference: None,
        path: parts[2..].join("/"),
    })
}

/// A github.com link: to a repository, or through `tree` or `blob` to a
/// branch and a folder or file in it.
fn github_link(url: &reqwest::Url) -> Result<Origin, String> {
    let parts: Vec<&str> = url
        .path_segments()
        .map(|segments| segments.filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    let unknown =
        || format!("{url} is not a link to a repository, nor to a folder or file in one.");
    let [owner, repo, rest @ ..] = parts.as_slice() else {
        return Err(unknown());
    };
    let (reference, path) = match rest {
        [] => (None, String::new()),
        ["tree" | "blob", reference, path @ ..] => (Some((*reference).to_owned()), path.join("/")),
        _ => return Err(unknown()),
    };
    Ok(Origin::GitHub {
        owner: (*owner).to_owned(),
        repo: repo.trim_end_matches(".git").to_owned(),
        reference,
        path,
    })
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

fn is_skill_file(path: &str) -> bool {
    path.rsplit(['/', '\\'])
        .next()
        .is_some_and(|name| name.eq_ignore_ascii_case(SKILL_FILE))
}

/// A `/`-separated path from a repository or a folder being copied, as one to
/// write under a skill's folder: plain names only, nothing that climbs out
/// or that Windows would read as something else.
fn safe(path: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in path.split('/') {
        let bad = matches!(part, "" | "." | "..")
            || part.contains(['\\', ':'])
            || part.chars().any(char::is_control);
        if bad {
            return None;
        }
        out.push(part);
    }
    Some(out)
}

/// Whether a file inside a skill's folder is left behind: hidden files and
/// folders, and the ones in `SKIPPED`.
fn left_out(inside: &str) -> bool {
    inside
        .split('/')
        .any(|part| part.starts_with('.') || SKIPPED.contains(&part))
}

/// Refuse an install over `MAX_FILES`, `MAX_BYTES`, or with a file over
/// `FILE_BYTES`, before anything is fetched or copied.
fn within_limits<'a>(files: impl Iterator<Item = (&'a str, u64)>) -> Result<(), String> {
    let (mut count, mut total) = (0, 0);
    for (path, size) in files {
        if size > FILE_BYTES {
            return Err(format!(
                "{path} is {size} bytes, and a skill's files are at most {FILE_BYTES} each."
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
/// folder above a SKILL.md, or the file's own name.
fn single(
    staging: &Path,
    segments: &[&str],
    bytes: Vec<u8>,
    what: &str,
) -> Result<PathBuf, String> {
    let text = String::from_utf8(bytes).map_err(|_| format!("{what} is not a text file."))?;
    let name = match segments {
        [.., folder, file] if is_skill_file(file) => (*folder).to_owned(),
        [.., file] => {
            let lower = file.to_lowercase();
            lower
                .strip_suffix(".md")
                .map_or(file.to_string(), |stem| file[..stem.len()].to_owned())
        }
        [] => String::new(),
    };
    let name = safe(&name).map_or_else(|| "skill".to_owned(), |_| name);
    let folder = staging.join("0").join(name);
    std::fs::create_dir_all(&folder)
        .and_then(|()| std::fs::write(folder.join(SKILL_FILE), text))
        .map_err(|e| format!("Could not write to {}: {e}", folder.display()))?;
    Ok(folder)
}

fn client() -> Result<reqwest::blocking::Client, String> {
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
fn get(
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

/// One answer from GitHub's API about `name`, with the errors people meet
/// said plainly: a repository that is not there or not public, and the
/// hourly limit on requests without an account.
fn api(
    client: &reqwest::blocking::Client,
    path: &str,
    name: &str,
) -> Result<serde_json::Value, String> {
    let response = client
        .get(format!("https://api.github.com/{path}"))
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
        404 => Err(format!(
            "GitHub has no public repository {name}, or not that branch."
        )),
        409 => Err(format!("{name} is empty.")),
        _ => Err(format!("GitHub answered HTTP {status} about {name}.")),
    }
}

/// A file's address on raw.githubusercontent.com, each part escaped.
fn raw_url(owner: &str, repo: &str, reference: &str, path: &str) -> Result<reqwest::Url, String> {
    let mut url =
        reqwest::Url::parse("https://raw.githubusercontent.com/").map_err(|e| e.to_string())?;
    url.path_segments_mut()
        .map_err(|()| "Could not make a GitHub address.".to_owned())?
        .extend(
            [owner, repo]
                .into_iter()
                .chain(reference.split('/'))
                .chain(path.split('/')),
        );
    Ok(url)
}

/// The skills in a GitHub repository, under `path` when there is one, each
/// folder's files fetched into `staging`.
fn from_github(
    owner: &str,
    repo: &str,
    reference: Option<String>,
    path: &str,
    staging: &Path,
) -> Result<Vec<PathBuf>, String> {
    let client = client()?;
    let name = format!("{owner}/{repo}");
    let reference = match reference {
        Some(reference) => reference,
        None => api(&client, &format!("repos/{owner}/{repo}"), &name)?["default_branch"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("GitHub did not say which branch of {name} to read."))?,
    };
    let tree = api(
        &client,
        &format!("repos/{owner}/{repo}/git/trees/{reference}?recursive=1"),
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
        let url = raw_url(owner, repo, &reference, path)?;
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
            let relative = safe(inside).ok_or_else(|| {
                format!("{name} has a file whose path cannot be written here: {file}")
            })?;
            files.push((file.as_str(), *size, into.join(relative)));
        }
        staged.push(into);
    }
    within_limits(files.iter().map(|(file, size, _)| (*file, *size)))?;
    download_all(&client, owner, repo, &reference, &files)?;
    Ok(staged)
}

/// Fetch each file into place, `DOWNLOADS` at a time, stopping at the first
/// that fails.
fn download_all(
    client: &reqwest::blocking::Client,
    owner: &str,
    repo: &str,
    reference: &str,
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
                    let fetched = raw_url(owner, repo, reference, path)
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
    let mine = std::fs::canonicalize(data.join(SKILLS_DIR)).ok();
    let full = std::fs::canonicalize(path).map_err(|e| format!("Could not read {shown}: {e}"))?;
    if mine.is_some_and(|mine| full.starts_with(mine)) {
        return Err(format!("{shown} is in your skills folder already."));
    }
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
            let bytes = std::fs::read(&full).map_err(|e| format!("Could not read {shown}: {e}"))?;
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
            let Some(to) = safe(&relative) else {
                continue;
            };
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

/// Move staged skills into the user's folder, archiving any already there by
/// the same name, and say what they are called.
fn place(data: &Path, staged: &[PathBuf]) -> Result<Vec<String>, String> {
    let root = data.join(SKILLS_DIR);
    std::fs::create_dir_all(&root)
        .map_err(|e| format!("Could not make {}: {e}", root.display()))?;
    let mut names: Vec<String> = Vec::new();
    for folder in staged {
        let Some(skill) = read_skill(folder, Source::User) else {
            return Err(format!(
                "The SKILL.md for {} could not be read as text.",
                folder.file_name().unwrap_or_default().to_string_lossy()
            ));
        };
        // Two of one name in one install: the first is kept.
        if names.iter().any(|name| same(name, &skill.name)) {
            continue;
        }
        if let Some(old) = find(&read_root(&root, Source::User), &skill.name) {
            archive(data, &old.folder)?;
        }
        // One not named in its frontmatter is called what its folder is, so
        // that folder keeps its name.
        let text = std::fs::read_to_string(folder.join(SKILL_FILE)).unwrap_or_default();
        let target = match field(&split(&text).0, "name") {
            Some(_) => free_folder(&root, &slug(&skill.name)),
            None => free_folder(
                &root,
                &folder.file_name().unwrap_or_default().to_string_lossy(),
            ),
        };
        std::fs::rename(folder, &target)
            .map_err(|e| format!("Could not move {} into {}: {e}", skill.name, root.display()))?;
        // As listed from now on, which a folder name already taken changes.
        names.push(read_skill(&target, Source::User).map_or(skill.name, |placed| placed.name));
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::{Origin, install_in, origin, safe, skill_folders, within_limits};
    use crate::skills::list_in;
    use std::path::{Path, PathBuf};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("letronna-skill-install-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn github(owner: &str, repo: &str, reference: Option<&str>, path: &str) -> Origin {
        Origin::GitHub {
            owner: owner.into(),
            repo: repo.into(),
            reference: reference.map(Into::into),
            path: path.into(),
        }
    }

    #[test]
    fn sources_are_told_apart() {
        let here = scratch("origin");
        assert_eq!(
            origin("anthropics/skills").unwrap(),
            github("anthropics", "skills", None, "")
        );
        assert_eq!(
            origin("anthropics/skills/skills/pdf").unwrap(),
            github("anthropics", "skills", None, "skills/pdf")
        );
        assert_eq!(
            origin(" https://github.com/anthropics/skills.git ").unwrap(),
            github("anthropics", "skills", None, "")
        );
        assert_eq!(
            origin("https://github.com/a/b/tree/main/skills/pdf/").unwrap(),
            github("a", "b", Some("main"), "skills/pdf")
        );
        assert_eq!(
            origin("https://github.com/a/b/blob/v1/skills/pdf/SKILL.md").unwrap(),
            github("a", "b", Some("v1"), "skills/pdf/SKILL.md")
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
            "https://github.com/a",
            "./not/there",
            "C:/not/there",
            "one-word",
        ] {
            assert!(origin(refused).is_err(), "{refused}");
        }
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
    fn paths_that_would_leave_the_folder_are_refused() {
        assert_eq!(
            safe("scripts/fill.py"),
            Some(PathBuf::from("scripts").join("fill.py"))
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
        assert!(within_limits((0..400).map(|_| ("a.md", 25_000))).is_ok());
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
        write(&from.join("tools/review/SKILL.md"), "Review with care.");
        write(
            &from.join("node_modules/dep/SKILL.md"),
            "---\nname: dep\n---\nNot mine.",
        );

        let mut names = install_in(&data, &from.display().to_string()).unwrap();
        names.sort();
        assert_eq!(names, ["pdf", "review"]);
        let pdf = data.join("skills/pdf");
        assert!(pdf.join("scripts/fill.py").is_file());
        assert!(pdf.join("examples/inner/SKILL.md").is_file());
        assert!(!pdf.join(".cache").exists());
        assert!(!data.join("skills-incoming").exists());

        let listed = list_in(&data, None).unwrap();
        let get = |name: &str| listed.iter().find(|s| s.name == name).unwrap();
        assert!(get("pdf").enabled && get("review").enabled);
        assert_eq!(get("review").description, "Review with care.");
        // The source is copied, not moved.
        assert!(from.join("pdf/SKILL.md").is_file());
    }

    #[test]
    fn installing_again_archives_the_skill_it_replaces() {
        let data = scratch("again-data");
        let from = scratch("again-source");
        write(&from.join("pdf/SKILL.md"), "---\nname: pdf\n---\nFirst.");
        install_in(&data, &from.join("pdf/SKILL.md").display().to_string()).unwrap();
        write(&from.join("pdf/SKILL.md"), "---\nname: PDF\n---\nSecond.");
        assert_eq!(
            install_in(&data, &from.display().to_string()).unwrap(),
            ["PDF"]
        );

        let listed = list_in(&data, None).unwrap();
        let pdfs: Vec<&str> = listed
            .iter()
            .filter(|s| s.name.eq_ignore_ascii_case("pdf"))
            .map(|s| s.body.as_str())
            .collect();
        assert_eq!(pdfs, ["Second."]);
        let archived: Vec<PathBuf> = std::fs::read_dir(data.join("skills-archive"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(archived.len(), 1);
        assert!(
            std::fs::read_to_string(archived[0].join("SKILL.md"))
                .unwrap()
                .contains("First.")
        );
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
        let names =
            install_in(&data, &from.join("Release notes.md").display().to_string()).unwrap();
        assert_eq!(names, ["Release notes"]);
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
    fn nothing_is_installed_from_your_own_skills_folder_or_a_folder_without_skills() {
        let data = scratch("own-data");
        list_in(&data, None).unwrap();
        let own = data.join("skills").display().to_string();
        assert!(install_in(&data, &own).unwrap_err().contains("already"));
        let empty = scratch("own-empty");
        write(&empty.join("README.md"), "Hello");
        let error = install_in(&data, &empty.display().to_string()).unwrap_err();
        assert!(error.contains("no SKILL.md"), "{error}");
    }

    #[test]
    #[ignore = "Downloads from GitHub: cargo test -p letronna-core --release -- --ignored anthropics"]
    fn installs_the_skills_in_anthropics_skills() {
        let data = scratch("anthropics");
        let names = install_in(&data, "anthropics/skills").unwrap();
        println!("{names:?}");
        assert!(names.len() >= 15, "{names:?}");
        assert!(names.iter().any(|n| n == "pdf"));
        assert!(data.join("skills/pdf/SKILL.md").is_file());
        assert!(data.join("skills/pdf/scripts").is_dir());

        let one = install_in(
            &data,
            "https://github.com/anthropics/skills/tree/main/skills/docx",
        )
        .unwrap();
        assert_eq!(one, ["docx"]);
        let raw = "https://raw.githubusercontent.com/anthropics/skills/main/skills/pdf/SKILL.md";
        assert_eq!(install_in(&data, raw).unwrap(), ["pdf"]);
        let listed = list_in(&data, None).unwrap();
        for skill in &listed {
            println!("{}  {}", skill.name, skill.description);
        }
    }
}
