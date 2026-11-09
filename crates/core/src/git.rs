//! What changed in the project folder since its last commit, as git sees it.
//!
//! `/diff` and the window's Changes tab show this without asking, so git runs
//! here with the read-only guards (`command::GUARDS`) and with no external
//! diff, no textconv and none of the filters its own config names: a
//! repository's own config must not pick a program for a look at its
//! changes to run.
//!
//! Untracked files are shown too, each diffed against nothing, since a file
//! a tool created is as much a change as one it edited. What git prints is
//! cut at a size, with a line saying so, rather than read whole.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The most of the summary kept: a line per file, so thousands of them.
const STAT_CAP: usize = 64 * 1024;

/// The most of the patch kept. Enough to read a large change through; a
/// generated file or a vendored tree is not worth holding in full.
const PATCH_CAP: usize = 256 * 1024;

/// Untracked files named and diffed, past which the rest are counted.
const UNTRACKED_MAX: usize = 200;

/// Width of the summary's lines, so both front ends get the same.
const STAT_WIDTH: &str = "--stat=100";

/// The changes in a project folder: git's summary of them, then the patch.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WorkingDiff {
    pub stat: String,
    pub patch: String,
}

impl WorkingDiff {
    /// Whether nothing has changed.
    pub fn is_empty(&self) -> bool {
        self.stat.trim().is_empty() && self.patch.trim().is_empty()
    }
}

/// What changed in `folder` since the last commit, tracked and untracked,
/// limited to `only` when given (paths relative to `folder`). Outside a git
/// repository there is nothing to compare with, and the error says so.
pub fn working_diff(folder: &Path, only: Option<&[PathBuf]>) -> Result<WorkingDiff, String> {
    in_repository(folder)?;
    if only.is_some_and(<[PathBuf]>::is_empty) {
        return Ok(WorkingDiff::default());
    }
    let base = base(folder)?;
    let off = filters_off(folder)?;
    let paths: Vec<String> = match only {
        Some(paths) => paths.iter().map(|p| p.display().to_string()).collect(),
        None => vec![".".to_owned()],
    };
    let diff = |extra: &str, cap: usize| {
        let mut args = vec![
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--relative",
            extra,
            base.as_str(),
            "--",
        ];
        args.extend(paths.iter().map(String::as_str));
        run(folder, &args, cap, &[0], &off)
    };
    let (mut stat, stat_cut) = diff(STAT_WIDTH, STAT_CAP)?;
    let (mut patch, mut patch_cut) = diff("--patch", PATCH_CAP)?;

    let mut listing = vec!["ls-files", "-z", "--others", "--exclude-standard", "--"];
    listing.extend(paths.iter().map(String::as_str));
    let (untracked, _) = git(folder, &listing, STAT_CAP)?;
    let untracked: Vec<&str> = untracked.split('\0').filter(|p| !p.is_empty()).collect();
    for (at, path) in untracked.iter().enumerate() {
        if at == UNTRACKED_MAX {
            stat.push_str(&format!(
                " and {} more untracked files\n",
                untracked.len() - UNTRACKED_MAX
            ));
            break;
        }
        stat.push_str(&format!(" {path} (untracked)\n"));
        if patch_cut {
            continue;
        }
        // One that went between the listing and now is only left out.
        let Ok((text, cut)) = new_file(folder, path, PATCH_CAP.saturating_sub(patch.len()), &off)
        else {
            continue;
        };
        patch.push_str(&text);
        patch_cut = cut;
    }
    if stat_cut {
        stat.push_str(&cut_note("summary", STAT_CAP));
    }
    if patch_cut {
        patch.push_str(&cut_note("patch", PATCH_CAP));
    }
    Ok(WorkingDiff {
        stat: printable(&stat),
        patch: printable(&patch),
    })
}

/// Where a project folder's repository stands, for the window's status bar.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    /// The branch checked out, or `None` when the head is detached.
    pub branch: Option<String>,
    /// Files changed, staged or untracked.
    pub changed: usize,
    /// Commits ahead of and behind the branch's upstream.
    pub ahead: usize,
    pub behind: usize,
}

impl Status {
    /// The status in a few words: "main · 3 changed ↑1".
    pub fn describe(&self) -> String {
        let mut parts = vec![self.branch.clone().unwrap_or_else(|| "detached".into())];
        if self.changed > 0 {
            parts.push(format!("{} changed", self.changed));
        }
        let mut line = parts.join(" · ");
        if self.ahead > 0 {
            line.push_str(&format!(" ↑{}", self.ahead));
        }
        if self.behind > 0 {
            line.push_str(&format!(" ↓{}", self.behind));
        }
        line
    }
}

/// Where `folder`'s repository stands. Git is told to take no optional
/// locks, so a look while the user commits never gets in their way, and
/// the repository's own filters are off, as status reads changed files
/// through them.
pub fn status(folder: &Path) -> Result<Status, String> {
    let off = filters_off(folder)?;
    let args = [
        "--no-optional-locks",
        "status",
        "--porcelain=v2",
        "--branch",
        "-z",
        "--untracked-files=normal",
    ];
    let (out, _) = run(folder, &args, STAT_CAP, &[0], &off)?;
    Ok(parse_status(&out))
}

fn parse_status(out: &str) -> Status {
    let mut status = Status::default();
    let mut fields = out.split('\0').filter(|field| !field.is_empty());
    while let Some(field) = fields.next() {
        if let Some(head) = field.strip_prefix("# branch.head ") {
            status.branch = (head != "(detached)").then(|| head.to_owned());
        } else if let Some(counts) = field.strip_prefix("# branch.ab ") {
            for count in counts.split(' ') {
                let number = |s: &str| s.parse().unwrap_or(0);
                if let Some(n) = count.strip_prefix('+') {
                    status.ahead = number(n);
                } else if let Some(n) = count.strip_prefix('-') {
                    status.behind = number(n);
                }
            }
        } else if field.starts_with(['1', 'u', '?']) {
            status.changed += 1;
        } else if field.starts_with('2') {
            status.changed += 1;
            // A rename's old name follows as a field of its own.
            fields.next();
        }
    }
    status
}

/// One changed file, with the lines it gained and lost.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChangedFile {
    /// Relative to the project folder.
    pub path: PathBuf,
    pub added: usize,
    pub removed: usize,
    /// Made since the comparison's start: untracked, or created by a tool.
    pub created: bool,
    /// Not text, so there are no lines to count.
    pub binary: bool,
}

/// The files changed in `folder` since the last commit, tracked and
/// untracked, with their counts; outside a git repository, an error that
/// says so.
pub fn changed_files(folder: &Path) -> Result<Vec<ChangedFile>, String> {
    in_repository(folder)?;
    let base = base(folder)?;
    let off = filters_off(folder)?;
    let args = [
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--relative",
        "--numstat",
        "-z",
        base.as_str(),
        "--",
        ".",
    ];
    let (numstat, _) = run(folder, &args, STAT_CAP, &[0], &off)?;
    let mut files: Vec<ChangedFile> = numstat
        .split('\0')
        .filter_map(|record| {
            let mut parts = record.splitn(3, '\t');
            let (added, removed, path) = (parts.next()?, parts.next()?, parts.next()?);
            Some(ChangedFile {
                path: PathBuf::from(path),
                added: added.parse().unwrap_or(0),
                removed: removed.parse().unwrap_or(0),
                created: false,
                binary: added == "-",
            })
        })
        .collect();
    let listing = [
        "ls-files",
        "-z",
        "--others",
        "--exclude-standard",
        "--",
        ".",
    ];
    let (untracked, _) = git(folder, &listing, STAT_CAP)?;
    for path in untracked.split('\0').filter(|p| !p.is_empty()) {
        // Counted from the file itself, as git would for a new one; one too
        // big to be worth reading is listed without its count.
        let text = read_small(&folder.join(path)).map(String::from_utf8);
        files.push(ChangedFile {
            path: PathBuf::from(path),
            added: match &text {
                Some(Ok(text)) => text.lines().count(),
                _ => 0,
            },
            removed: 0,
            created: true,
            binary: matches!(text, Some(Err(_))),
        });
    }
    Ok(files)
}

/// A plain file's bytes, or `None` for a link, a device, a pipe or a file
/// over [`PATCH_CAP`]: a link to `/dev/zero` reports no size and never ends,
/// and a pipe blocks the read for good.
fn read_small(path: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > PATCH_CAP as u64 {
        return None;
    }
    let mut bytes = Vec::new();
    let file = std::fs::File::open(path).ok()?;
    file.take(PATCH_CAP as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= PATCH_CAP).then_some(bytes)
}

/// Ok when `folder` is in a git repository, or an error saying there is
/// nothing to compare it with.
fn in_repository(folder: &Path) -> Result<(), String> {
    let inside = git(folder, &["rev-parse", "--is-inside-work-tree"], 64)
        .map(|(out, _)| out.trim() == "true")
        .unwrap_or(false);
    if !inside {
        return Err(format!(
            "{} is not in a git repository, so there is nothing to compare it with.",
            folder.display()
        ));
    }
    Ok(())
}

/// What the changes are measured from: the last commit, or nothing at all
/// in a repository that has none yet.
fn base(folder: &Path) -> Result<String, String> {
    if git(
        folder,
        &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
        128,
    )
    .is_ok()
    {
        return Ok("HEAD".to_owned());
    }
    // The empty tree, asked of git rather than written in, since its name
    // depends on the repository's hash.
    let (tree, _) = git(folder, &["hash-object", "-t", "tree", "--stdin"], 128)?;
    Ok(tree.trim().to_owned())
}

/// The most of a listing of changed files kept, past which it counts as
/// more than any caller looks through: thousands of names of a few dozen
/// bytes each.
const LISTED_CAP: usize = 4 * 1024 * 1024;

/// A repository as a command found it, for telling what the command
/// changed: its top folder, the commit checked out, and the config that
/// turns its own filters off.
pub(crate) struct Checkout {
    pub top: PathBuf,
    /// `None` in a repository with no commit yet.
    pub head: Option<String>,
    /// The way down from the top to the folder it was found from, ending
    /// in `/`, or empty when that is the top itself.
    pub prefix: String,
    off: Vec<(String, String)>,
}

/// One file `git status` lists, by its path from the repository's top.
pub(crate) struct Listed {
    pub path: String,
    /// Git tracks no such file, in the commit or the index.
    pub untracked: bool,
}

/// What the commit checked out holds at a path, as `git ls-tree` says.
pub(crate) struct Entry {
    /// Git's mode for it: [`PLAIN`] or [`PROGRAM`] for a file, [`LINK`], or
    /// a submodule's.
    pub mode: u32,
    /// Its object, which [`Checkout::committed`] reads.
    pub id: String,
    /// Its size; `None` for a submodule.
    pub size: Option<u64>,
}

/// Git's mode for a plain file, for one marked as a program, and for a link.
pub(crate) const PLAIN: u32 = 0o100644;
pub(crate) const PROGRAM: u32 = 0o100755;
pub(crate) const LINK: u32 = 0o120000;

/// Bytes of paths one `ls-tree` is given, so hundreds of long ones stay
/// well inside what a command line may hold.
const PATHS_PER_CALL: usize = 64 * 1024;

impl Checkout {
    /// The repository `folder` is in, or an error outside one.
    pub(crate) fn of(folder: &Path) -> Result<Checkout, String> {
        let args = ["rev-parse", "--show-toplevel", "--show-prefix"];
        let (out, cut) = git(folder, &args, 64 * 1024)?;
        // The top, then the way down from it to `folder`, empty at the top.
        let mut lines = out.lines();
        let top = lines.next().unwrap_or_default();
        if cut || top.is_empty() {
            return Err(format!("{} is not in a git work tree.", folder.display()));
        }
        Ok(Checkout {
            top: PathBuf::from(top),
            head: head(folder),
            prefix: lines.next().unwrap_or_default().to_owned(),
            off: filters_off(folder)?,
        })
    }

    /// Whether the repository ignores `folder`, the one it was found from:
    /// a project kept in a repository that leaves it out, as a home
    /// folder's may, has none of its files listed.
    pub(crate) fn ignores(&self, folder: &Path) -> Result<bool, String> {
        // Asked about its top, git says a `*` ignores it, when that leaves
        // out only the untracked files below.
        if self.prefix.is_empty() {
            return Ok(false);
        }
        // An ignored path is printed; none is exit status 1. check-ignore
        // refuses literal paths, and `.` is the same either way.
        let patterns = [("GIT_LITERAL_PATHSPECS".to_owned(), "0".to_owned())];
        let args = ["check-ignore", "--", "."];
        let (out, _) = run(folder, &args, 1024, &[0, 1], &patterns)?;
        Ok(!out.is_empty())
    }

    /// The files under `folder` that differ from the commit or are not
    /// tracked, leaving out what git ignores, or `None` when there are
    /// more than `most`.
    pub(crate) fn listed(&self, folder: &Path, most: usize) -> Result<Option<Vec<Listed>>, String> {
        // Renames are a removal and an addition, so each entry is one name.
        // A submodule is listed when it is gone or on another commit, so
        // that can be said, but not for work inside it, which is its own.
        let args = [
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--no-renames",
            "--ignore-submodules=dirty",
            "--",
            ".",
        ];
        let (out, cut) = run_raw(folder, &args, LISTED_CAP, &[0], &self.off, None)?;
        if cut {
            return Ok(None);
        }
        let listed: Vec<Listed> = out
            .split(|&b| b == 0)
            .filter_map(|entry| {
                // Two letters of status and a space come before the name.
                let path = String::from_utf8_lossy(entry.get(3..)?).into_owned();
                Some(Listed {
                    path,
                    untracked: entry.starts_with(b"??"),
                })
            })
            .collect();
        Ok((listed.len() <= most).then_some(listed))
    }

    /// Of `paths`, from the repository's top, those the commit checked out
    /// holds, each with what it holds there; a path it holds nothing at is
    /// left out.
    pub(crate) fn in_commit(&self, paths: &[&str]) -> Result<HashMap<String, Entry>, String> {
        let mut found = HashMap::new();
        let Some(head) = &self.head else {
            return Ok(found);
        };
        let mut rest = paths;
        while !rest.is_empty() {
            // As many as fit on one command line, and always one.
            let mut bytes = 0;
            let mut take = 0;
            while take < rest.len() && (take == 0 || bytes + rest[take].len() < PATHS_PER_CALL) {
                bytes += rest[take].len() + 1;
                take += 1;
            }
            let (asked, more) = rest.split_at(take);
            rest = more;
            let args: Vec<&str> = ["ls-tree", "-z", "-l", head.as_str(), "--"]
                .into_iter()
                .chain(asked.iter().copied())
                .collect();
            let (out, cut) = run_raw(&self.top, &args, LISTED_CAP, &[0], &self.off, None)?;
            if cut {
                return Err("git listed more than is read".into());
            }
            // Each is its mode, kind, object and size, then a tab and its
            // path; a submodule's size is `-`.
            for listed in out.split(|&b| b == 0) {
                let Some(tab) = listed.iter().position(|&b| b == b'\t') else {
                    continue;
                };
                let fields = String::from_utf8_lossy(&listed[..tab]);
                let [mode, _, id, size] = fields.split_whitespace().collect::<Vec<_>>()[..] else {
                    continue;
                };
                let Ok(mode) = u32::from_str_radix(mode, 8) else {
                    continue;
                };
                let entry = Entry {
                    mode,
                    id: id.to_owned(),
                    size: size.parse().ok(),
                };
                let path = String::from_utf8_lossy(&listed[tab + 1..]).into_owned();
                found.insert(path, entry);
            }
        }
        Ok(found)
    }

    /// Of `paths`, from the repository's top, those a `filter` attribute
    /// names a filter for: what checking one out writes is that filter's
    /// to say, and the repository's own filters are off here.
    pub(crate) fn filtered(&self, paths: &[&str]) -> Result<HashSet<String>, String> {
        let input: Vec<u8> = paths
            .iter()
            .flat_map(|path| path.bytes().chain([0]))
            .collect();
        let args = ["check-attr", "--stdin", "-z", "filter"];
        let (out, cut) = run_raw(&self.top, &args, LISTED_CAP, &[0], &self.off, Some(&input))?;
        if cut {
            return Err("git listed more than is read".into());
        }
        // Each path comes with the attribute's name and its value.
        let fields: Vec<&[u8]> = out.split(|&b| b == 0).collect();
        Ok(fields
            .as_chunks::<3>()
            .0
            .iter()
            .filter(|entry| !matches!(entry[2], b"unspecified" | b"unset"))
            .map(|entry| String::from_utf8_lossy(entry[0]).into_owned())
            .collect())
    }

    /// What each of `files`, plain files from the repository's top, holds
    /// in the commit checked out, as checking it out writes it when no
    /// `filter` attribute names it (see [`Checkout::filtered`]):
    /// end-of-line and encoding changes made, filters not. Reading stops at
    /// `cap` bytes in all, and a file past that is left out.
    pub(crate) fn committed(
        &self,
        files: &[(&str, &Entry)],
        cap: usize,
    ) -> Result<HashMap<String, Vec<u8>>, String> {
        let Some(head) = &self.head else {
            return Ok(HashMap::new());
        };
        let paths: Vec<&str> = files.iter().map(|(path, _)| *path).collect();
        let converted = self.converted(&paths)?;
        // Those written out as stored come in one go. A batch that converts
        // heads each file with its stored size but gives its converted
        // bytes, so it cannot be read apart: those take a git each.
        let (each, stored): (Vec<_>, Vec<_>) = files
            .iter()
            .partition(|(path, _)| converted.contains(*path));
        let mut found = self.stored(&stored, cap)?;
        let mut left = cap.saturating_sub(found.values().map(Vec::len).sum());
        for (path, _) in each {
            let object = format!("{head}:{path}");
            let args = ["cat-file", "--filters", object.as_str()];
            let Ok((bytes, false)) = run_raw(&self.top, &args, left, &[0], &self.off, None) else {
                continue;
            };
            left -= bytes.len();
            found.insert((*path).to_owned(), bytes);
        }
        Ok(found)
    }

    /// What each of `files` holds as the commit stores it, read in one go
    /// until `cap` bytes; a file past that is left out.
    fn stored(
        &self,
        files: &[&(&str, &Entry)],
        cap: usize,
    ) -> Result<HashMap<String, Vec<u8>>, String> {
        let mut found = HashMap::new();
        if files.is_empty() {
            return Ok(found);
        }
        let input: String = files
            .iter()
            .map(|(_, entry)| format!("{}\n", entry.id))
            .collect();
        let args = ["cat-file", "--batch"];
        let input = Some(input.as_bytes());
        let (out, _) = run_raw(&self.top, &args, cap, &[0], &self.off, input)?;
        // Each comes as a line of its object, kind and size, then that many
        // bytes and a line break; one cut short is left out.
        let mut rest = out.as_slice();
        for (path, _) in files {
            let Some(end) = rest.iter().position(|&b| b == b'\n') else {
                break;
            };
            let header = String::from_utf8_lossy(&rest[..end]).into_owned();
            rest = &rest[end + 1..];
            let size = match header.split(' ').collect::<Vec<_>>()[..] {
                [_, "blob", size] => size.parse::<usize>().ok(),
                _ => None,
            };
            let Some(size) = size else {
                continue;
            };
            let Some(bytes) = rest.get(..size) else {
                break;
            };
            found.insert((*path).to_owned(), bytes.to_vec());
            rest = rest.get(size + 1..).unwrap_or_default();
        }
        Ok(found)
    }

    /// Of `paths`, from the repository's top, those checking out may write
    /// otherwise than the commit stores them: with `ident` or an encoding
    /// of their own, or with line endings turned into CR LF, as an `eol`
    /// or `crlf` attribute, the config or Windows may ask, unless marked as
    /// not text. Checking out changes a file in no other way but a filter.
    fn converted(&self, paths: &[&str]) -> Result<HashSet<String>, String> {
        // A key with no value is true. Nothing found is exit status 1.
        let listing = ["config", "-z", "--get-regexp", r"^core\.(autocrlf|eol)$"];
        let (config, _) = run(&self.top, &listing, STAT_CAP, &[0, 1], &self.off)?;
        let crlf_config = config
            .split('\0')
            .filter(|entry| !entry.is_empty())
            .any(|entry| {
                let (key, value) = entry.split_once('\n').unwrap_or((entry, "true"));
                let value = value.to_ascii_lowercase();
                match key {
                    "core.autocrlf" => {
                        !matches!(value.as_str(), "false" | "no" | "off" | "0" | "input")
                    }
                    _ => value == "crlf",
                }
            });
        let input: Vec<u8> = paths
            .iter()
            .flat_map(|path| path.bytes().chain([0]))
            .collect();
        let names = ["text", "eol", "crlf", "ident", "working-tree-encoding"];
        let args: Vec<&str> = ["check-attr", "--stdin", "-z"]
            .into_iter()
            .chain(names)
            .collect();
        let (out, cut) = run_raw(&self.top, &args, LISTED_CAP, &[0], &self.off, Some(&input))?;
        if cut {
            return Err("git listed more than is read".into());
        }
        // Each path comes with each attribute's name and its value.
        let mut set: HashMap<&[u8], HashMap<&[u8], &[u8]>> = HashMap::new();
        let fields: Vec<&[u8]> = out.split(|&b| b == 0).collect();
        for entry in fields.as_chunks::<3>().0 {
            set.entry(entry[0]).or_default().insert(entry[1], entry[2]);
        }
        let as_stored = |path: &str| {
            let Some(set) = set.get(path.as_bytes()) else {
                return false;
            };
            let value = |name: &str| set.get(name.as_bytes()).copied().unwrap_or(b"unspecified");
            let not_text = value("text") == b"unset" || value("crlf") == b"unset";
            let crlf = value("eol") == b"crlf" || value("crlf") == b"set" || crlf_config;
            matches!(value("ident"), b"unspecified" | b"unset")
                && value("working-tree-encoding") == b"unspecified"
                && (not_text || !(crlf || cfg!(windows)))
        };
        Ok(paths
            .iter()
            .filter(|path| !as_stored(path))
            .map(|path| (*path).to_owned())
            .collect())
    }
}

/// The commit checked out in `folder`'s repository, or `None` when it has
/// none yet or there is no repository.
pub(crate) fn head(folder: &Path) -> Option<String> {
    let args = ["rev-parse", "--verify", "--quiet", "HEAD^{commit}"];
    let (out, _) = git(folder, &args, 1024).ok()?;
    Some(out.trim().to_owned()).filter(|head| !head.is_empty())
}

/// The text of the file at `path` in `folder`'s repository as the commit
/// checked out holds it, read without filters, or `None` where the commit
/// has no such file or there is no repository.
pub(crate) fn at_head(folder: &Path, path: &str) -> Option<String> {
    let blob = format!("HEAD:{path}");
    let (out, _) = git(folder, &["cat-file", "blob", &blob], 8 << 20).ok()?;
    Some(out)
}

/// The files below `folder` its repository neither tracks nor ignores whose
/// paths match `pattern`, a pathspec as git reads one, by their paths from
/// there; `None` where there is no repository or git could not say. Git
/// looks into no repository inside it, a submodule's or another's, and
/// lists only what it finds, so a name it reads otherwise than the disk
/// spells it, by case or by its Unicode form, is left out rather than in.
pub(crate) fn untracked(folder: &Path, pattern: &str) -> Option<Vec<String>> {
    let args = [
        "ls-files",
        "-z",
        "--others",
        "--exclude-standard",
        "--",
        pattern,
    ];
    // A pattern this once, where every other call reads paths as names.
    let globbed = [("GIT_LITERAL_PATHSPECS".to_owned(), "0".to_owned())];
    let (out, cut) = run(folder, &args, 1 << 20, &[0], &globbed).ok()?;
    let listed = out.split('\0').filter(|path| !path.is_empty());
    (!cut).then(|| listed.map(str::to_owned).collect())
}

/// Whether `folder` is the top of a repository's working tree, not a folder
/// inside one.
pub(crate) fn is_top(folder: &Path) -> bool {
    let Ok((out, _)) = git(folder, &["rev-parse", "--show-toplevel"], 4096) else {
        return false;
    };
    let canonical = |path: &Path| std::fs::canonicalize(path).ok();
    canonical(Path::new(out.trim())).is_some_and(|top| canonical(folder) == Some(top))
}

/// The paths of the files in the commit checked out in `folder`'s
/// repository with a line that matches the extended regular expression
/// `pattern`; none where nothing matches or there is no repository.
pub(crate) fn matching_at_head(folder: &Path, pattern: &str) -> Vec<String> {
    let args = ["grep", "-l", "-I", "-E", "-e", pattern, "HEAD", "--"];
    let Ok((out, _)) = run(folder, &args, 1 << 20, &[0, 1], &[]) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|line| line.strip_prefix("HEAD:"))
        .map(str::to_owned)
        .collect()
}

/// The config that turns off each filter the repository's own config
/// names, as `GIT_CONFIG_*` pairs to follow the guards': a clean filter
/// runs on every changed file a diff reads, a smudge filter on every file
/// read out of a commit, and a repository must not pick a program for a
/// look at its changes to run. The user's own filters, Git LFS's among
/// them, are left to run as they would for `git diff`.
fn filters_off(folder: &Path) -> Result<Vec<(String, String)>, String> {
    let listing = [
        "config",
        "--show-scope",
        "-z",
        "--get-regexp",
        r"^filter\..*\.(clean|smudge|process)$",
    ];
    // Nothing found is exit status 1.
    let (text, cut) = run(folder, &listing, STAT_CAP, &[0, 1], &[])?;
    if cut {
        return Err("The repository's config names too many filters to turn off.".into());
    }
    let mut names: Vec<&str> = Vec::new();
    // Each entry is its scope, then its key and value on one line.
    let fields: Vec<&str> = text.split('\0').collect();
    for pair in fields.as_chunks::<2>().0 {
        let key = pair[1].split('\n').next().unwrap_or_default();
        let name = key
            .strip_prefix("filter.")
            .and_then(|key| key.rsplit_once('.'))
            .map(|(name, _)| name);
        if let (Some(name), "local" | "worktree") = (name, pair[0])
            && !names.contains(&name)
        {
            names.push(name);
        }
    }
    let guards = crate::command::GUARDS
        .iter()
        .find(|(key, _)| *key == "GIT_CONFIG_COUNT")
        .and_then(|(_, count)| count.parse::<usize>().ok())
        .unwrap_or(0);
    let mut pairs = Vec::new();
    let keys = names.iter().flat_map(|name| {
        ["clean", "smudge", "process", "required"]
            .map(|part| (format!("filter.{name}.{part}"), part))
    });
    for (at, (key, part)) in keys.enumerate() {
        let value = if part == "required" { "false" } else { "" };
        pairs.push((format!("GIT_CONFIG_KEY_{}", guards + at), key));
        pairs.push((
            format!("GIT_CONFIG_VALUE_{}", guards + at),
            value.to_owned(),
        ));
    }
    let count = guards + pairs.len() / 2;
    pairs.push(("GIT_CONFIG_COUNT".to_owned(), count.to_string()));
    Ok(pairs)
}

/// An untracked file as a patch that adds it, at most `cap` bytes of it.
fn new_file(
    folder: &Path,
    path: &str,
    cap: usize,
    off: &[(String, String)],
) -> Result<(String, bool), String> {
    // A difference is exit status 1 here, which `git` counts as a failure.
    let args = [
        "diff",
        "--no-index",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--patch",
        "--",
        "/dev/null",
        path,
    ];
    run(folder, &args, cap, &[0, 1], off)
}

/// The line put where a part was cut.
fn cut_note(part: &str, cap: usize) -> String {
    format!(
        "… the {part} is cut here at {} KB; run git diff to see all of it.\n",
        cap / 1024
    )
}

/// Run git in `folder` with the guards on, keeping at most `cap` bytes of
/// what it prints; whether it was cut rides along.
fn git(folder: &Path, args: &[&str], cap: usize) -> Result<(String, bool), String> {
    run(folder, args, cap, &[0], &[])
}

/// `git` for any exit status in `fine`, with the config pairs in `off`
/// laid over the guards.
fn run(
    folder: &Path,
    args: &[&str],
    cap: usize,
    fine: &[i32],
    off: &[(String, String)],
) -> Result<(String, bool), String> {
    let (bytes, cut) = run_raw(folder, args, cap, fine, off, None)?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if cut {
        // End on a whole line, or a whole name of a `-z` listing, rather
        // than halfway through one.
        let end = text.rfind(['\n', '\0']).map_or(0, |at| at + 1);
        text.truncate(end);
    }
    Ok((text, cut))
}

/// `run`, giving back the bytes git printed as they are: a file read out
/// of a commit need not be text. What `input` holds is given to git to
/// read; without it git reads nothing.
fn run_raw(
    folder: &Path,
    args: &[&str],
    cap: usize,
    fine: &[i32],
    off: &[(String, String)],
    input: Option<&[u8]>,
) -> Result<(Vec<u8>, bool), String> {
    let mut cmd = Command::new("git");
    cmd.args([
        "--no-pager",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.quotePath=false",
    ])
    .args(args)
    .current_dir(folder)
    // Set by a hook or a shell this started from, these would choose the
    // repository, its index or how a path is read in place of `folder`.
    .env_remove("GIT_DIR")
    .env_remove("GIT_WORK_TREE")
    .env_remove("GIT_INDEX_FILE")
    .env_remove("GIT_COMMON_DIR")
    .env_remove("GIT_OBJECT_DIRECTORY")
    .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
    .env_remove("GIT_NAMESPACE")
    .env_remove("GIT_GLOB_PATHSPECS")
    .env_remove("GIT_NOGLOB_PATHSPECS")
    .env_remove("GIT_ICASE_PATHSPECS")
    .envs(crate::command::GUARDS.iter().copied())
    // Paths are names, never patterns: `--literal-pathspecs`, said where
    // `off` can still take it back for a command that refuses it.
    .env("GIT_LITERAL_PATHSPECS", "1")
    .envs(off.iter().map(|(key, value)| (key, value)))
    .env_remove("GIT_EXTERNAL_DIFF")
    .stdin(match input {
        Some(_) => Stdio::piped(),
        None => Stdio::null(),
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("git could not be run: {e}"))?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("git gave no output to read.".into());
    };
    // Written beside the reading too: git prints as it reads, and would
    // stop reading once its output filled the pipe. Write errors are left
    // to git's status, as git stopping early is what causes them.
    let feed = child.stdin.take().zip(input).map(|(mut stdin, input)| {
        let input = input.to_vec();
        std::thread::spawn(move || stdin.write_all(&input))
    });
    // Read beside the output, not after it: git can warn a line per file
    // before it prints anything, and would wait on a full pipe for ever.
    let errors = std::thread::spawn(move || why_failed(stderr));
    let mut bytes = Vec::new();
    let read = stdout.take(cap as u64 + 1).read_to_end(&mut bytes);
    let cut = bytes.len() > cap;
    if cut {
        // The rest is not wanted, and git would wait on a full pipe for it.
        let _ = child.kill();
    }
    let status = child
        .wait()
        .map_err(|e| format!("git did not finish: {e}"))?;
    if let Some(feed) = feed {
        let _ = feed.join();
    }
    let why = errors.join().unwrap_or_default();
    read.map_err(|e| format!("git's output could not be read: {e}"))?;
    if !cut && !status.code().is_some_and(|code| fine.contains(&code)) {
        let why = why.as_deref().unwrap_or("it failed");
        // Named by its command, not by an option given before it.
        let command = args.iter().find(|arg| !arg.starts_with('-'));
        return Err(format!("git {} failed: {why}", command.unwrap_or(&"")));
    }
    bytes.truncate(cap);
    Ok((bytes, cut))
}

/// The line of what git printed to `stderr` that says why it stopped: its
/// fatal or error line past any warnings, else its first. Read to the end,
/// keeping only that line, so a flood of warnings costs no memory.
fn why_failed(stderr: impl Read) -> Option<String> {
    let (mut first, mut fatal) = (None, None);
    let mut reader = std::io::BufReader::new(stderr);
    let mut bytes = Vec::new();
    while reader.read_until(b'\n', &mut bytes).is_ok_and(|n| n > 0) {
        let line = String::from_utf8_lossy(&bytes).trim().to_owned();
        bytes.clear();
        if fatal.is_none() && (line.starts_with("fatal:") || line.starts_with("error:")) {
            fatal = Some(line);
        } else if first.is_none() && !line.is_empty() {
            first = Some(line);
        }
    }
    fatal.or(first)
}

/// `text` without what a terminal would act on: a file's own escapes must
/// not reach the screen as escapes. Tabs and line ends stay.
pub(crate) fn printable(text: &str) -> String {
    text.chars()
        .filter(|&c| c != '\r')
        .map(|c| match c {
            '\n' | '\t' => c,
            c if c.is_control() => '?',
            c => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of its own for one test, under the system's temp folder.
    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("letronna-git-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// Run git in `folder` as a test sets a repository up, ignoring the
    /// user's own config.
    fn setup(folder: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(folder)
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "nul" } else { "/dev/null" },
            )
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    /// A repository with one committed file.
    fn repo(name: &str) -> PathBuf {
        let folder = scratch(name);
        setup(&folder, &["init", "-q"]);
        std::fs::write(folder.join("a.txt"), "one\ntwo\n").unwrap();
        setup(&folder, &["add", "a.txt"]);
        setup(&folder, &["commit", "-q", "-m", "first"]);
        folder
    }

    #[test]
    fn only_files_the_repository_neither_tracks_nor_ignores_are_untracked() {
        let folder = repo("untracked-list");
        std::fs::create_dir_all(folder.join("sub/deep")).unwrap();
        std::fs::create_dir_all(folder.join("built")).unwrap();
        std::fs::write(folder.join(".gitignore"), "built/\n").unwrap();
        for file in [
            "sub/a_spec.txt",
            "sub/deep/b[1]_spec.txt",
            "built/c_spec.txt",
            "d.txt",
        ] {
            std::fs::write(folder.join(file), "x").unwrap();
        }
        std::fs::write(folder.join("staged_spec.txt"), "x").unwrap();
        setup(&folder, &["add", "staged_spec.txt"]);
        // A repository inside, whose files are its own.
        let inner = folder.join("vendor/lib");
        std::fs::create_dir_all(&inner).unwrap();
        setup(&inner, &["init", "-q"]);
        std::fs::write(inner.join("e_spec.txt"), "x").unwrap();
        setup(&inner, &["add", "e_spec.txt"]);
        assert_eq!(
            untracked(&folder, "*_spec*").unwrap(),
            ["sub/a_spec.txt", "sub/deep/b[1]_spec.txt"]
        );
        // Asked from a folder inside, the paths are from there.
        assert_eq!(
            untracked(&folder.join("sub"), "*_spec*").unwrap(),
            ["a_spec.txt", "deep/b[1]_spec.txt"]
        );
        assert!(is_top(&folder));
        assert!(!is_top(&folder.join("sub")));
        // Outside a repository git cannot say.
        let none = scratch("untracked-none");
        assert_eq!(untracked(&none, "*_spec*"), None);
        assert!(!is_top(&none));
    }

    #[test]
    fn an_edit_shows_in_the_summary_and_the_patch() {
        let folder = repo("edit");
        std::fs::write(folder.join("a.txt"), "one\n2\n").unwrap();
        let diff = working_diff(&folder, None).unwrap();
        assert!(diff.stat.contains("a.txt"), "{}", diff.stat);
        assert!(diff.patch.contains("-two\n+2\n"), "{}", diff.patch);
    }

    #[test]
    fn an_untracked_file_shows_as_added() {
        let folder = repo("untracked");
        std::fs::write(folder.join("new.txt"), "fresh\n").unwrap();
        let diff = working_diff(&folder, None).unwrap();
        assert!(diff.stat.contains("new.txt (untracked)"), "{}", diff.stat);
        assert!(diff.patch.contains("+fresh"), "{}", diff.patch);
        // Limited to another file, it is left out.
        let only = working_diff(&folder, Some(&[PathBuf::from("a.txt")])).unwrap();
        assert!(only.is_empty(), "{only:?}");
    }

    #[test]
    fn a_repository_s_own_diff_programs_do_not_run() {
        let folder = repo("drivers");
        std::fs::write(
            folder.join(".gitattributes"),
            "*.txt diff=evil filter=evil\n",
        )
        .unwrap();
        setup(
            &folder,
            &["config", "diff.evil.command", "touch ran-command"],
        );
        setup(
            &folder,
            &["config", "diff.evil.textconv", "touch ran-textconv; cat"],
        );
        setup(&folder, &["config", "diff.external", "touch ran-external"]);
        setup(&folder, &["config", "core.pager", "touch ran-pager; cat"]);
        setup(
            &folder,
            &["config", "filter.evil.clean", "touch ran-clean; cat"],
        );
        setup(
            &folder,
            &["config", "filter.evil.process", "touch ran-process"],
        );
        setup(&folder, &["config", "filter.evil.required", "true"]);
        std::fs::write(folder.join("a.txt"), "one\nthree\n").unwrap();
        std::fs::write(folder.join("new.txt"), "fresh\n").unwrap();
        let diff = working_diff(&folder, None).unwrap();
        assert!(diff.patch.contains("+three"), "{}", diff.patch);
        assert!(diff.patch.contains("+fresh"), "{}", diff.patch);
        // The Changes tab's list and the status bar read the same files.
        let files = changed_files(&folder).unwrap();
        assert!(
            files.iter().any(|f| f.path == Path::new("a.txt")),
            "{files:?}"
        );
        assert!(status(&folder).unwrap().changed > 0);
        let markers = [
            "ran-command",
            "ran-textconv",
            "ran-external",
            "ran-pager",
            "ran-clean",
            "ran-process",
        ];
        for marker in markers {
            assert!(!folder.join(marker).exists(), "{marker} ran");
        }
        // A plain git diff there does run them, so a marker can appear.
        setup(&folder, &["config", "--unset", "filter.evil.process"]);
        setup(&folder, &["config", "--unset", "filter.evil.required"]);
        setup(&folder, &["diff", "HEAD"]);
        for marker in ["ran-command", "ran-clean"] {
            assert!(folder.join(marker).exists(), "{marker} did not run");
        }
    }

    #[test]
    fn a_long_patch_is_cut_with_a_note() {
        let folder = repo("long");
        let big: String = (0..40_000).map(|n| format!("line {n}\n")).collect();
        std::fs::write(folder.join("a.txt"), big).unwrap();
        let diff = working_diff(&folder, None).unwrap();
        assert!(diff.patch.len() < PATCH_CAP + 200);
        assert!(
            diff.patch.contains("the patch is cut here"),
            "{}",
            &diff.patch[..200]
        );
    }

    #[test]
    fn a_repository_with_no_commit_compares_with_nothing() {
        let folder = scratch("unborn");
        setup(&folder, &["init", "-q"]);
        std::fs::write(folder.join("a.txt"), "one\n").unwrap();
        setup(&folder, &["add", "a.txt"]);
        let diff = working_diff(&folder, None).unwrap();
        assert!(diff.patch.contains("+one"), "{}", diff.patch);
    }

    #[test]
    fn a_flood_of_warnings_does_not_stall_the_diff() {
        let folder = scratch("warnings");
        setup(&folder, &["init", "-q"]);
        for n in 0..1000 {
            std::fs::write(folder.join(format!("f{n}.txt")), "a\n").unwrap();
        }
        setup(&folder, &["add", "."]);
        setup(&folder, &["commit", "-q", "-m", "first"]);
        // Each changed file then earns a line of warning, well past what a
        // pipe holds before git waits for it to be read.
        setup(&folder, &["config", "core.autocrlf", "true"]);
        for n in 0..1000 {
            std::fs::write(folder.join(format!("f{n}.txt")), "a\nb\n").unwrap();
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let asked = folder.clone();
        std::thread::spawn(move || tx.send(working_diff(&asked, None)));
        let diff = rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("the diff stalled")
            .unwrap();
        assert!(diff.stat.contains("1000 files changed"), "{}", diff.stat);
    }

    #[test]
    fn a_folder_outside_git_says_so() {
        let folder = scratch("none");
        // A temp folder inside someone's repository is in one, and there
        // is nothing to check.
        let error = Command::new("git")
            .args(["rev-parse", "--is-inside-work-tree"])
            .current_dir(&folder)
            .output()
            .unwrap();
        if error.status.success() {
            return;
        }
        let why = working_diff(&folder, None).unwrap_err();
        assert!(why.contains("not in a git repository"), "{why}");
    }

    #[test]
    fn escapes_in_a_file_do_not_reach_the_screen() {
        assert_eq!(printable("a\x1b[31mb\r\n\tc"), "a?[31mb\n\tc");
    }

    #[test]
    fn changed_files_are_counted_tracked_and_untracked() {
        let folder = repo("counts");
        std::fs::write(folder.join("a.txt"), "one\n2\nthree\n").unwrap();
        std::fs::write(folder.join("new.txt"), "x\ny\n").unwrap();
        let files = changed_files(&folder).unwrap();
        let counts: Vec<(String, usize, usize, bool)> = files
            .iter()
            .map(|f| (f.path.display().to_string(), f.added, f.removed, f.created))
            .collect();
        assert_eq!(
            counts,
            vec![
                ("a.txt".into(), 2, 1, false),
                ("new.txt".into(), 2, 0, true)
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_untracked_link_to_a_device_is_listed_without_being_read() {
        let folder = repo("device-link");
        std::os::unix::fs::symlink("/dev/zero", folder.join("zero")).unwrap();
        let files = changed_files(&folder).unwrap();
        let zero = files.iter().find(|f| f.path == Path::new("zero")).unwrap();
        assert!(zero.created && !zero.binary && zero.added == 0, "{zero:?}");
    }

    #[test]
    fn the_status_counts_changes_and_names_the_branch() {
        let folder = repo("status");
        std::fs::write(folder.join("a.txt"), "changed\n").unwrap();
        std::fs::write(folder.join("new.txt"), "x\n").unwrap();
        let status = status(&folder).unwrap();
        assert_eq!(status.changed, 2, "{status:?}");
        assert!(status.branch.is_some(), "{status:?}");
    }

    #[test]
    fn a_status_line_reads_as_the_status_bar_shows_it() {
        let out = "# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0\
                   # branch.ab +1 -2\0\
                   1 .M N... 100644 100644 100644 a b a.txt\0\
                   2 R. N... 100644 100644 100644 a b R100 new.txt\0old.txt\0\
                   ? loose.txt\0";
        let status = parse_status(out);
        assert_eq!(
            status,
            Status {
                branch: Some("main".into()),
                changed: 3,
                ahead: 1,
                behind: 2,
            }
        );
        assert_eq!(status.describe(), "main · 3 changed ↑1 ↓2");
        let detached = parse_status("# branch.head (detached)\0");
        assert_eq!(detached.describe(), "detached");
    }
}
