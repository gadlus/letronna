//! Skills: instructions for a kind of task, kept as folders.
//!
//! A skill is a folder holding SKILL.md -- a few lines of frontmatter naming
//! and describing it, then the instructions in Markdown -- beside whatever
//! scripts, references or templates the instructions point at. It is the
//! layout of github.com/anthropics/skills, so a skill written for another
//! agent works here as it is, and the frontmatter is read tolerantly rather
//! than as strict YAML for the same reason.
//!
//! The user's own live in `<data>/skills`. A project can bring more in
//! `.agents/skills` or `.claude/skills`, anywhere from its folder up to its
//! git root. When two share a name the user's wins, then the one nearest the
//! project folder, so a repository cannot quietly swap out a skill the user
//! relies on.
//!
//! Each skill can be switched off. What is kept is the names that are off, in
//! `skills-off.json`, so a skill that is new, installed or brought by a
//! project starts on.
//!
//! Using one in the window or the terminal drops its instructions into the
//! composer rather than sending them, so they can be adjusted first: a
//! playbook is a starting point, not a command.

use crate::chat::{self as backend};
use crate::tools::one_line;
use serde::Deserialize;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The file that makes a folder a skill.
const SKILL_FILE: &str = "SKILL.md";
/// The user's skills, one folder each, in the data folder.
const SKILLS_DIR: &str = "skills";
/// The names switched off, in the data folder.
const OFF_FILE: &str = "skills-off.json";
/// Where a removed skill's folder goes, whole, rather than being deleted.
const ARCHIVE: &str = "skills-archive";
/// A description taken from the instructions is cut here: it only has to say
/// what the skill is for.
const DESCRIPTION_CHARS: usize = 200;
/// A bigger SKILL.md is not instructions anybody wrote, and reading it on
/// every list would stall whoever asked.
const SKILL_BYTES: u64 = 1_000_000;
/// Skill folders looked at in one folder of them. A checkout can hold any
/// number, and reading them all is how one would stall every list.
const ROOT_SKILLS: usize = 200;

/// Where a skill comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// The user's skills folder: made, installed or moved over from
    /// skills.json.
    User,
    /// A project's `.agents/skills` or `.claude/skills`.
    Project,
}

/// A skill as the list shows it.
#[derive(Clone, Debug)]
pub struct Skill {
    /// What the list shows and a skill is asked for by.
    pub name: String,
    /// When to use it: the frontmatter's, or else the start of the body.
    pub description: String,
    /// The instructions: SKILL.md after its frontmatter.
    pub body: String,
    /// The folder holding SKILL.md and whatever files it refers to.
    pub folder: PathBuf,
    pub source: Source,
    /// Switched on. A skill switched off stays on disk and in the list.
    pub enabled: bool,
}

/// The user's skills folder.
pub fn dir() -> PathBuf {
    backend::data_dir().join(SKILLS_DIR)
}

/// Every skill a session in `workspace` can see, by name, after moving any
/// old skills.json across. Skills with the same name as an earlier one are
/// left out. A skills.json that cannot be moved does not stop the list;
/// `migrate` says why.
pub fn list(workspace: Option<&Path>) -> Result<Vec<Skill>, String> {
    list_in(&backend::data_dir(), workspace)
}

fn list_in(data: &Path, workspace: Option<&Path>) -> Result<Vec<Skill>, String> {
    // Reported by `migrate`; the skills already in folders are fine.
    let _ = migrate_in(data);
    let off = switched_off(data)?;
    let mut skills: Vec<Skill> = Vec::new();
    for (root, source) in roots(data, workspace) {
        for mut skill in read_root(&root, source) {
            if find(&skills, &skill.name).is_some() {
                continue;
            }
            skill.enabled = !off.iter().any(|name| same(name, &skill.name));
            skills.push(skill);
        }
    }
    skills.sort_by_key(|s| s.name.to_lowercase());
    Ok(skills)
}

/// The skill called `name`, whatever its case.
pub fn find<'a>(skills: &'a [Skill], name: &str) -> Option<&'a Skill> {
    skills.iter().find(|s| same(&s.name, name))
}

/// Names compare without case, the way they are asked for.
fn same(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Switch a skill on or off by name.
pub fn set_enabled(name: &str, on: bool) -> Result<(), String> {
    set_enabled_in(&backend::data_dir(), name, on)
}

fn set_enabled_in(data: &Path, name: &str, on: bool) -> Result<(), String> {
    let mut off = switched_off(data)?;
    off.retain(|n| !same(n, name));
    if !on {
        off.push(one_line(name));
        off.sort_by_key(|n| n.to_lowercase());
    }
    save_off(data, &off)
}

/// Make a skill in the user's folder. An empty description is taken from the
/// start of the instructions.
pub fn create(name: &str, description: &str, body: &str) -> Result<Skill, String> {
    create_in(&backend::data_dir(), name, description, body)
}

fn create_in(data: &Path, name: &str, description: &str, body: &str) -> Result<Skill, String> {
    let (name, body) = (one_line(name), clean(body));
    if name.is_empty() {
        return Err("A skill needs a name.".into());
    }
    if body.is_empty() {
        return Err("A skill needs instructions.".into());
    }
    // First, so a skill made now cannot keep the built-in ones or the old
    // skills.json from arriving; a failure is `migrate`'s to report.
    let _ = migrate_in(data);
    let root = data.join(SKILLS_DIR);
    if find(&read_root(&root, Source::User), &name).is_some() {
        return Err(format!("You already have a skill called \"{name}\"."));
    }
    let description = match one_line(description) {
        given if given.is_empty() => summary_of(&body),
        given => given,
    };
    let folder = free_folder(&root, &slug(&name));
    write_skill(&folder, &render(&name, &description, &[], &body))?;
    loaded(data, &folder)
}

/// Rename one of the user's skills or change its instructions, keeping the
/// rest of its frontmatter. With no `description`, a written one stays unless
/// it was only the start of the old instructions, when it follows the new.
pub fn edit(
    name: &str,
    new_name: &str,
    description: Option<&str>,
    body: &str,
) -> Result<Skill, String> {
    edit_in(&backend::data_dir(), name, new_name, description, body)
}

fn edit_in(
    data: &Path,
    name: &str,
    new_name: &str,
    description: Option<&str>,
    body: &str,
) -> Result<Skill, String> {
    let (new_name, body) = (one_line(new_name), clean(body));
    if new_name.is_empty() {
        return Err("A skill needs a name.".into());
    }
    if body.is_empty() {
        return Err("A skill needs instructions.".into());
    }
    let mine = read_root(&data.join(SKILLS_DIR), Source::User);
    let Some(skill) = find(&mine, name) else {
        return Err(not_yours(name));
    };
    if !same(name, &new_name) && find(&mine, &new_name).is_some() {
        return Err(format!("You already have a skill called \"{new_name}\"."));
    }
    let file = skill.folder.join(SKILL_FILE);
    let text = read_text(&file, SKILL_BYTES)
        .ok_or_else(|| format!("Could not read {}.", file.display()))?;
    let (entries, old_body) = split(&text);
    let written = field(&entries, "description").filter(|d| *d != summary_of(&old_body));
    let description = match description.map(one_line) {
        Some(given) if !given.is_empty() => given,
        Some(_) => summary_of(&body),
        None => written.unwrap_or_else(|| summary_of(&body)),
    };
    let kept: Vec<Entry> = entries
        .into_iter()
        .filter(|e| {
            !e.key.eq_ignore_ascii_case("name") && !e.key.eq_ignore_ascii_case("description")
        })
        .collect();
    // The switch goes with the skill. The new name is switched off before
    // the skill takes it, so a failure between the two leaves it off rather
    // than on.
    let mut off = switched_off(data)?;
    let moving = !same(name, &new_name) && off.iter().any(|n| same(n, name));
    if moving {
        off.push(new_name.clone());
        save_off(data, &off)?;
    }
    write_skill(
        &skill.folder,
        &render(&new_name, &description, &kept, &body),
    )?;
    if moving {
        off.retain(|n| !same(n, name));
        save_off(data, &off)?;
    }
    loaded(data, &skill.folder)
}

/// Take one of the user's skills out of the list by moving its folder into
/// `skills-archive`, and say where it went.
pub fn remove(name: &str) -> Result<PathBuf, String> {
    remove_in(&backend::data_dir(), name)
}

fn remove_in(data: &Path, name: &str) -> Result<PathBuf, String> {
    let mine = read_root(&data.join(SKILLS_DIR), Source::User);
    let Some(skill) = find(&mine, name) else {
        return Err(not_yours(name));
    };
    let mut off = switched_off(data)?;
    let archived = archive(data, &skill.folder)?;
    // Only once it is gone, or a failed move would leave it switched on.
    // Forgotten, since a new skill of the same name should start on.
    if off.iter().any(|n| same(n, name)) {
        off.retain(|n| !same(n, name));
        save_off(data, &off)
            .map_err(|why| format!("Moved \"{name}\" to {}, but: {why}", archived.display()))?;
    }
    Ok(archived)
}

/// The error for a name that is none of the user's own skills.
fn not_yours(name: &str) -> String {
    format!(
        "None of your skills is called \"{name}\". A skill that comes with a project is changed \
         in the project's folder."
    )
}

/// The skill just written to `folder`, switched on or off as its name says.
fn loaded(data: &Path, folder: &Path) -> Result<Skill, String> {
    let mut skill = read_skill(folder, Source::User)
        .ok_or_else(|| format!("Could not read back the skill in {}.", folder.display()))?;
    skill.enabled = !switched_off(data)?.iter().any(|n| same(n, &skill.name));
    Ok(skill)
}

/// The folders skills are read from, first one first when names clash: the
/// user's, then each folder from the project's up to its git root, with
/// `.agents` before `.claude`. Outside a repository only the project folder
/// itself is looked in, so a session in a downloads folder does not pick up
/// skills from everything above it.
fn roots(data: &Path, workspace: Option<&Path>) -> Vec<(PathBuf, Source)> {
    let mut roots = vec![(data.join(SKILLS_DIR), Source::User)];
    let Some(workspace) = workspace else {
        return roots;
    };
    let top = workspace.ancestors().find(|dir| dir.join(".git").exists());
    for dir in workspace.ancestors() {
        for hidden in [".agents", ".claude"] {
            roots.push((dir.join(hidden).join(SKILLS_DIR), Source::Project));
        }
        if top.is_none_or(|top| dir == top) {
            break;
        }
    }
    roots
}

/// The skills in one folder of them, in the order of their folders' names,
/// from the first `ROOT_SKILLS` folders. A project's skill folder that is a
/// link is passed over, so a repository's skills are read from inside it;
/// the user's own may be links they made.
fn read_root(root: &Path, source: Source) -> Vec<Skill> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut folders: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| source == Source::User || entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .filter(|path| {
            !path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        })
        .collect();
    folders.sort();
    folders
        .iter()
        .take(ROOT_SKILLS)
        .filter_map(|folder| read_skill(folder, source))
        .collect()
}

/// The skill in `folder`, if it holds a SKILL.md that can be read. A missing
/// name is the folder's; a missing description, the start of the body.
fn read_skill(folder: &Path, source: Source) -> Option<Skill> {
    let text = read_text(&folder.join(SKILL_FILE), SKILL_BYTES)?;
    let (entries, body) = split(&text);
    let name = field(&entries, "name")
        .or_else(|| Some(printable(&folder.file_name()?.to_string_lossy())))
        .filter(|name| !name.is_empty())?;
    let description = field(&entries, "description").unwrap_or_else(|| summary_of(&body));
    Some(Skill {
        name,
        description,
        body,
        folder: folder.to_path_buf(),
        source,
        enabled: true,
    })
}

/// A file's text, when it is a regular file of at most `cap` bytes. Checked
/// before it is opened and bounded while it is read: a SKILL.md that links to
/// a device such as /dev/zero would otherwise never finish.
fn read_text(path: &Path, cap: u64) -> Option<String> {
    use std::io::Read;
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(cap + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > cap {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Text for one line of a list or the model's index: whitespace collapsed and
/// control characters dropped, so a name cannot start new lines or send a
/// terminal escape.
fn printable(text: &str) -> String {
    one_line(text).chars().filter(|c| !c.is_control()).collect()
}

/// One top-level entry of the frontmatter: its key, and its lines as written,
/// continuation lines included, so an edit can put back the ones it does not
/// change.
struct Entry {
    key: String,
    lines: Vec<String>,
}

/// SKILL.md split into its frontmatter entries and the body after them. Text
/// with no frontmatter is all body.
fn split(text: &str) -> (Vec<Entry>, String) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text.lines().collect();
    let fenced = lines.first().is_some_and(|l| l.trim_end() == "---");
    let end = lines
        .iter()
        .skip(1)
        .position(|l| matches!(l.trim_end(), "---" | "..."));
    let (true, Some(end)) = (fenced, end) else {
        return (Vec::new(), lines.join("\n").trim().to_owned());
    };
    let mut entries: Vec<Entry> = Vec::new();
    for line in &lines[1..=end] {
        // A key starts at the margin; indented lines, list items and blank
        // lines belong to the entry above.
        let top = line.starts_with(|c: char| !c.is_whitespace() && !matches!(c, '-' | '#'));
        match line.split_once(':') {
            Some((key, _)) if top => entries.push(Entry {
                key: key.trim().to_owned(),
                lines: vec![(*line).to_owned()],
            }),
            _ => {
                if let Some(entry) = entries.last_mut() {
                    entry.lines.push((*line).to_owned());
                }
            }
        }
    }
    (entries, lines[end + 2..].join("\n").trim().to_owned())
}

/// An entry's value on one printable line, or None when it is missing or
/// empty.
fn field(entries: &[Entry], key: &str) -> Option<String> {
    entries
        .iter()
        .find(|e| e.key.eq_ignore_ascii_case(key))
        .map(|e| printable(&value(e)))
        .filter(|v| !v.is_empty())
}

/// An entry's value: a `|` block kept line by line, and a `>` block or a
/// plain value spread over lines joined into one, with its quotes taken off.
fn value(entry: &Entry) -> String {
    let first = entry.lines[0].split_once(':').map_or("", |(_, v)| v).trim();
    let rest = entry.lines[1..].iter().map(|l| l.trim());
    if first.starts_with('|') {
        return rest.collect::<Vec<_>>().join("\n").trim().to_owned();
    }
    let joined = std::iter::once(first)
        .filter(|first| !first.starts_with('>'))
        .chain(rest)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    unquote(&joined)
}

/// A YAML scalar without its quotes, with `''` and backslash escapes read
/// back.
fn unquote(value: &str) -> String {
    let value = value.trim();
    if let Some(inner) = value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
        return inner.replace("''", "'");
    }
    let Some(inner) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) else {
        return value.to_owned();
    };
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// A value written so a YAML reader gets it back as it is: plain when that is
/// safe, quoted otherwise.
fn scalar(value: &str) -> String {
    let plain = !value.is_empty()
        && !value.starts_with(|c: char| c.is_whitespace() || "-?:,[]{}#&*!|>'\"%@`".contains(c))
        && !value.ends_with(|c: char| c.is_whitespace() || c == ':')
        && !value.contains(": ")
        && !value.contains(" #");
    if plain {
        return value.to_owned();
    }
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// SKILL.md for a name, a description and a body, with the frontmatter
/// entries an edit keeps.
fn render(name: &str, description: &str, kept: &[Entry], body: &str) -> String {
    let mut text = format!(
        "---\nname: {}\ndescription: {}\n",
        scalar(name),
        scalar(description)
    );
    for line in kept.iter().flat_map(|entry| &entry.lines) {
        text.push_str(line);
        text.push('\n');
    }
    text.push_str("---\n\n");
    text.push_str(body.trim());
    text.push('\n');
    text
}

/// Write SKILL.md beside itself and move it over, so a crash cannot leave
/// half of one.
fn write_skill(folder: &Path, text: &str) -> Result<(), String> {
    let file = folder.join(SKILL_FILE);
    let temp = temp_beside(&file);
    std::fs::create_dir_all(folder)
        .and_then(|()| std::fs::write(&temp, text))
        .and_then(|()| std::fs::rename(&temp, &file))
        .map_err(|e| format!("Could not save the skill in {}: {e}", folder.display()))
}

/// A name to write beside `path` before moving over it, this process's and
/// this call's own, so two saves at once never move each other's half-written
/// file into place. Hidden, so one left by a crash is not read as part of a
/// skill.
fn temp_beside(path: &Path) -> PathBuf {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(".{name}.{}-{n}.tmp", std::process::id()))
}

/// Instructions as they are kept: trimmed, with Windows line ends made plain
/// so a reread compares equal.
fn clean(body: &str) -> String {
    body.trim().replace("\r\n", "\n")
}

/// What a skill with no description is for: its first paragraph that is not
/// a heading, cut short.
fn summary_of(body: &str) -> String {
    let paragraphs = || body.split("\n\n").map(str::trim).filter(|p| !p.is_empty());
    let first = paragraphs()
        .find(|p| !p.starts_with('#'))
        .or_else(|| paragraphs().next())
        .unwrap_or_default();
    let first = printable(first.trim_start_matches('#'));
    if first.chars().count() <= DESCRIPTION_CHARS {
        return first;
    }
    let cut: String = first.chars().take(DESCRIPTION_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

/// A folder name for a skill: its words in lowercase, joined by hyphens.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out.chars().take(64).collect();
    let out = out.trim_end_matches('-');
    match out {
        "" => "skill".to_owned(),
        // Windows will not make a folder with a device's name.
        out if device(out) => format!("{out}-skill"),
        out => out.to_owned(),
    }
}

/// Whether Windows takes `name` for a device, which it does whatever
/// extension follows: CON, PRN, AUX, NUL, CONIN$, CONOUT$, and COM or LPT
/// with a digit, superscripts included.
fn device(name: &str) -> bool {
    let base = name.split('.').next().unwrap_or_default().trim_end();
    let base = base.to_lowercase();
    let numbered = |prefix: &str| {
        base.strip_prefix(prefix).is_some_and(|rest| {
            let mut chars = rest.chars();
            matches!(
                (chars.next(), chars.next()),
                (Some('0'..='9' | '¹' | '²' | '³'), None)
            )
        })
    };
    matches!(
        base.as_str(),
        "con" | "prn" | "aux" | "nul" | "conin$" | "conout$"
    ) || numbered("com")
        || numbered("lpt")
}

/// `root/slug`, or with a number after it when that is taken.
fn free_folder(root: &Path, slug: &str) -> PathBuf {
    let mut folder = root.join(slug);
    let mut n = 2;
    while folder.exists() {
        folder = root.join(format!("{slug}-{n}"));
        n += 1;
    }
    folder
}

/// Move a skill's folder into the archive, whole, and say where it went.
fn archive(data: &Path, folder: &Path) -> Result<PathBuf, String> {
    let name = folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let archive = data.join(ARCHIVE);
    let target = free_folder(&archive, &format!("{name}-{}", backend::now()));
    std::fs::create_dir_all(&archive)
        .and_then(|()| std::fs::rename(folder, &target))
        .map_err(|e| {
            format!(
                "Could not move {} into {}: {e}",
                folder.display(),
                archive.display()
            )
        })?;
    Ok(target)
}

/// The names switched off. A file that will not parse is reported rather
/// than read as none, which would switch every skill back on.
fn switched_off(data: &Path) -> Result<Vec<String>, String> {
    let path = data.join(OFF_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(format!(
                "Could not read which skills are off from {}: {e}",
                path.display()
            ));
        }
    };
    serde_json::from_slice(&bytes).map_err(|e| {
        format!(
            "Could not read which skills are off from {}: {e}. Mend it, or delete it to switch \
             every skill on.",
            path.display()
        )
    })
}

/// Write the names switched off beside the file and move them over it.
fn save_off(data: &Path, off: &[String]) -> Result<(), String> {
    let path = data.join(OFF_FILE);
    let temp = temp_beside(&path);
    let bytes = serde_json::to_vec_pretty(off)
        .map_err(|e| format!("Could not save which skills are off: {e}"))?;
    std::fs::create_dir_all(data)
        .and_then(|()| std::fs::write(&temp, bytes))
        .and_then(|()| std::fs::rename(&temp, &path))
        .map_err(|e| {
            format!(
                "Could not save which skills are off to {}: {e}",
                path.display()
            )
        })
}

/// A skill as skills.json kept it, before skills were folders.
#[derive(Deserialize)]
struct Saved {
    name: String,
    prompt: String,
}

/// Move the old skills.json across if it is still there, and say why when it
/// cannot be. Listing and making skills go on without it, so this is where a
/// skills.json that will not parse is reported.
pub fn migrate() -> Result<(), String> {
    migrate_in(&backend::data_dir())
}

/// Move skills.json's prompts into folders, once, then set the file aside as
/// skills.json.migrated rather than delete it. On a first run, with neither,
/// the built-in skills are written the same way so the page is not empty.
///
/// They arrive switched off. They were written to be put in the composer and
/// sent as they are, and a model that loaded "Run the tests" by itself would
/// follow its "do not change anything yet" when asked to fix what fails.
fn migrate_in(data: &Path) -> Result<(), String> {
    let old = data.join("skills.json");
    let root = data.join(SKILLS_DIR);
    if !old.exists() && root.exists() {
        return Ok(());
    }
    // Someone else is moving it: the window and the terminal started at once.
    let Some(_lock) = Lock::take(data)? else {
        return Ok(());
    };
    // Looked at again under the lock, which the other may just have let go.
    let saved = match std::fs::read(&old) {
        Ok(bytes) => parse_saved(&old, &bytes)?,
        Err(e) if e.kind() == ErrorKind::NotFound && root.exists() => return Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => defaults(),
        Err(e) => {
            return Err(format!(
                "Could not read your skills from {}: {e}",
                old.display()
            ));
        }
    };
    // Made even for no skills at all, or the defaults would come back.
    std::fs::create_dir_all(&root)
        .map_err(|e| format!("Could not make {}: {e}", root.display()))?;
    let have = read_root(&root, Source::User);
    let mut taken: Vec<String> = have.iter().map(|s| s.name.clone()).collect();
    let mut moving: Vec<(String, String)> = Vec::new();
    for Saved { name, prompt } in saved {
        let prompt = clean(&prompt);
        if prompt.is_empty() {
            continue;
        }
        let name = match printable(&name) {
            name if name.is_empty() => prompt.chars().take(40).collect(),
            name => name,
        };
        if prompt.len() as u64 > SKILL_BYTES {
            return Err(format!(
                "Your saved skill \"{name}\" is over {SKILL_BYTES} bytes, too long for a skill. \
                 Shorten it in {}, or move the file away to go on without it.",
                old.display()
            ));
        }
        // Moved already, and skills.json written again since by an older
        // copy of the app; perhaps under a number, when its name was taken.
        let moved = have
            .iter()
            .any(|s| s.body == prompt && numbered(&s.name, &name))
            || moving
                .iter()
                .any(|(other, body)| *body == prompt && numbered(other, &name));
        if moved {
            continue;
        }
        let name = unique_name(&taken, &name);
        taken.push(name.clone());
        moving.push((name, prompt));
    }
    // Switched off before they exist, so a crash between the two cannot
    // leave one on.
    if !moving.is_empty() {
        let mut off = switched_off(data)?;
        off.extend(moving.iter().map(|(name, _)| name.clone()));
        off.sort_by_key(|n| n.to_lowercase());
        off.dedup_by(|a, b| same(a, b));
        save_off(data, &off)?;
    }
    for (name, prompt) in &moving {
        let folder = free_folder(&root, &slug(name));
        write_skill(&folder, &render(name, &summary_of(prompt), &[], prompt))?;
    }
    if old.exists() {
        // Numbered rather than over an earlier one, which a rename replaces.
        let mut aside = old.with_file_name("skills.json.migrated");
        let mut n = 2;
        while aside.exists() {
            aside = old.with_file_name(format!("skills.json.migrated.{n}"));
            n += 1;
        }
        std::fs::rename(&old, &aside)
            .map_err(|e| format!("Could not set {} aside: {e}", old.display()))?;
    }
    Ok(())
}

/// `name`, or `name (2)` and so on when one of `taken` already has it.
fn unique_name(taken: &[String], name: &str) -> String {
    let mut unique = name.to_owned();
    let mut n = 2;
    while taken.iter().any(|t| same(t, &unique)) {
        unique = format!("{name} ({n})");
        n += 1;
    }
    unique
}

/// Whether `candidate` is `name`, or `name` given a number by `unique_name`.
fn numbered(candidate: &str, name: &str) -> bool {
    let (candidate, name) = (candidate.to_lowercase(), name.to_lowercase());
    candidate == name
        || candidate
            .strip_prefix(&name)
            .and_then(|rest| rest.strip_prefix(" ("))
            .and_then(|rest| rest.strip_suffix(')'))
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// Held while skills.json moves across, so two copies of the app starting
/// together do not both move it. One left by a crash is taken over after a
/// minute.
struct Lock(PathBuf);

impl Lock {
    /// The lock, or None while someone else holds it.
    fn take(data: &Path) -> Result<Option<Lock>, String> {
        let path = data.join("skills.lock");
        std::fs::create_dir_all(data)
            .map_err(|e| format!("Could not make {}: {e}", data.display()))?;
        for _ in 0..2 {
            let made = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path);
            match made {
                Ok(_) => return Ok(Some(Lock(path))),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    let stale = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|at| at.elapsed().ok())
                        .is_some_and(|age| age > std::time::Duration::from_secs(60));
                    if !stale {
                        return Ok(None);
                    }
                    let _ = std::fs::remove_file(&path);
                }
                Err(e) => return Err(format!("Could not make {}: {e}", path.display())),
            }
        }
        Ok(None)
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// skills.json's skills, or an error naming the copy kept of a file that
/// will not parse. The file is left where it is: moving on without it would
/// lose the user's skills.
fn parse_saved(path: &Path, bytes: &[u8]) -> Result<Vec<Saved>, String> {
    let why = match serde_json::from_slice(bytes) {
        Ok(saved) => return Ok(saved),
        Err(why) => why,
    };
    let kept = keep_aside(path, bytes)?;
    Err(format!(
        "Could not read your skills from {}: {why}. A copy is kept at {}. Mend the file, or move \
         it away to go on without it.",
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

/// A few to start with, so the page explains itself by example rather than
/// opening empty. They are ordinary skills: editable and removable.
fn defaults() -> Vec<Saved> {
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
    .map(|(name, prompt)| Saved {
        name: name.to_owned(),
        prompt: prompt.to_owned(),
    })
    .collect()
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{
        Skill, Source, create_in, defaults, edit_in, list_in, migrate_in, remove_in, scalar,
        set_enabled_in, slug, split, value,
    };
    use std::path::{Path, PathBuf};

    fn scratch(name: &str) -> PathBuf {
        // The process's id too, so two checkouts testing at once keep apart.
        let dir =
            std::env::temp_dir().join(format!("letronna-skills-{name}-{}", std::process::id()));
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

    fn names(skills: &[Skill]) -> Vec<&str> {
        skills.iter().map(|s| s.name.as_str()).collect()
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn a_first_run_writes_the_built_in_skills_as_folders() {
        let data = scratch("first-run");
        let skills = list_in(&data, None).unwrap();
        assert_eq!(skills.len(), defaults().len());
        for skill in &skills {
            assert_eq!(skill.source, Source::User);
            // Off for the model until the user switches them on.
            assert!(!skill.enabled);
            assert!(skill.folder.join("SKILL.md").is_file());
            assert!(!skill.description.is_empty());
        }
        assert_eq!(list_in(&data, None).unwrap().len(), skills.len());

        // Removing them all leaves an empty folder, not the defaults again.
        for skill in &skills {
            remove_in(&data, &skill.name).unwrap();
        }
        assert!(list_in(&data, None).unwrap().is_empty());
    }

    #[test]
    fn skills_json_becomes_folders_once_and_is_set_aside() {
        let data = scratch("migrate");
        let old = br#"[{"id": 1, "name": "Mine", "prompt": "Keep me\r\nwhole"},
                       {"id": 2, "name": "", "prompt": "Nameless prompt"},
                       {"id": 3, "name": "Blank", "prompt": "  "}]"#;
        std::fs::write(data.join("skills.json"), old).unwrap();

        let skills = list_in(&data, None).unwrap();
        assert_eq!(names(&skills), ["Mine", "Nameless prompt"]);
        assert_eq!(skills[0].body, "Keep me\nwhole");
        assert_eq!(skills[0].description, "Keep me whole");
        assert!(skills.iter().all(|s| !s.enabled));
        assert!(!data.join("skills.json").exists());
        assert_eq!(
            std::fs::read(data.join("skills.json.migrated")).unwrap(),
            old
        );

        // An older copy of the app wrote it again: nothing is doubled, and
        // the first copy set aside is kept.
        std::fs::write(data.join("skills.json"), old).unwrap();
        assert_eq!(
            names(&list_in(&data, None).unwrap()),
            ["Mine", "Nameless prompt"]
        );
        assert!(data.join("skills.json.migrated").exists());
        assert!(data.join("skills.json.migrated.2").exists());
        assert!(!data.join("skills.json").exists());

        // A different prompt under a name already taken is kept beside it.
        std::fs::write(
            data.join("skills.json"),
            br#"[{"id": 1, "name": "Mine", "prompt": "Something else"}]"#,
        )
        .unwrap();
        assert_eq!(
            names(&list_in(&data, None).unwrap()),
            ["Mine", "Mine (2)", "Nameless prompt"]
        );
        // And written again, it is not numbered once more.
        std::fs::write(
            data.join("skills.json"),
            br#"[{"id": 1, "name": "Mine", "prompt": "Something else"}]"#,
        )
        .unwrap();
        assert_eq!(
            names(&list_in(&data, None).unwrap()),
            ["Mine", "Mine (2)", "Nameless prompt"]
        );
    }

    #[test]
    fn two_migrations_at_once_move_each_prompt_once() {
        let data = scratch("race");
        let prompts: Vec<String> = (0..60)
            .map(|n| format!(r#"{{"id": {n}, "name": "Prompt {n}", "prompt": "Do {n}."}}"#))
            .collect();
        std::fs::write(data.join("skills.json"), format!("[{}]", prompts.join(","))).unwrap();
        let racers: Vec<_> = (0..2)
            .map(|_| {
                let data = data.clone();
                std::thread::spawn(move || list_in(&data, None).map(|_| ()))
            })
            .collect();
        for racer in racers {
            racer.join().unwrap().unwrap();
        }
        let folders = std::fs::read_dir(data.join("skills")).unwrap().count();
        assert_eq!(folders, 60);
        assert!(!data.join("skills.lock").exists());
    }

    #[test]
    fn a_broken_skills_json_does_not_stop_skills_being_listed_or_made() {
        let data = scratch("broken-goes-on");
        std::fs::write(data.join("skills.json"), b"not json").unwrap();
        write(&data.join("skills/mine/SKILL.md"), "Mine.");
        assert_eq!(names(&list_in(&data, None).unwrap()), ["mine"]);
        create_in(&data, "Another", "", "Body").unwrap();
        assert!(migrate_in(&data).unwrap_err().contains("skills.json"));
        assert!(data.join("skills.json").exists());
    }

    #[test]
    fn a_prompt_too_long_for_a_skill_keeps_skills_json_where_it_is() {
        let data = scratch("too-long");
        let long = "x".repeat(super::SKILL_BYTES as usize + 1);
        let old = format!(r#"[{{"id": 1, "name": "Big", "prompt": "{long}"}}]"#);
        std::fs::write(data.join("skills.json"), &old).unwrap();
        let error = migrate_in(&data).unwrap_err();
        assert!(error.contains("\"Big\""), "{error}");
        assert!(data.join("skills.json").exists());
        assert!(!data.join("skills").join("big").exists());
    }

    #[test]
    fn a_skills_json_that_will_not_parse_is_copied_aside_and_reported() {
        let data = scratch("broken");
        let path = data.join("skills.json");
        let broken = b"[{\"id\": 1, \"name\": \"Mine\", \"prompt\": \"Keep me\",}]";
        std::fs::write(&path, broken).unwrap();

        let Err(error) = migrate_in(&data) else {
            panic!("a broken file loaded");
        };
        let kept = kept_copies(&data);
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert!(error.contains(&kept[0].display().to_string()), "{error}");
        assert_eq!(std::fs::read(&kept[0]).unwrap(), broken);
        // The file itself is left for the user to mend, and nothing else is
        // written in its place.
        assert_eq!(std::fs::read(&path).unwrap(), broken);
        assert!(!data.join("skills").exists());
    }

    #[test]
    fn the_same_broken_file_is_copied_only_once() {
        let data = scratch("once");
        std::fs::write(data.join("skills.json"), b"not json").unwrap();
        let earlier = data.join("skills.json.bad.1");
        std::fs::write(&earlier, b"not json").unwrap();

        let Err(error) = migrate_in(&data) else {
            panic!("a broken file loaded");
        };
        assert!(error.contains(&earlier.display().to_string()), "{error}");
        assert_eq!(kept_copies(&data), [earlier]);
    }

    #[test]
    fn frontmatter_is_read_tolerantly() {
        let data = scratch("frontmatter");
        std::fs::create_dir_all(data.join("skills")).unwrap();
        write(
            &data.join("skills/pdf/SKILL.md"),
            "\u{feff}---\r\nname: \"pdf\"\r\ndescription: >\r\n  Fill and merge\r\n  PDF forms.\r\nlicense: Proprietary\r\nmetadata:\r\n  version: 2\r\n---\r\n\r\n# PDF\r\n\r\nUse pypdf.\r\n",
        );
        write(
            &data.join("skills/quoted/SKILL.md"),
            "---\nname: 'It''s quoted'\ndescription: \"Says \\\"hi\\\"\"\n---\nBody\n",
        );
        write(
            &data.join("skills/continued/SKILL.md"),
            "---\nname: continued\ndescription: Spread over\n  two lines\nallowed-tools:\n- Read\n---\nBody\n",
        );
        write(
            &data.join("skills/bare-folder/SKILL.md"),
            "# Title\n\nThe first real paragraph\nsays what it is for.\n\nMore.",
        );
        write(
            &data.join("skills/unclosed/SKILL.md"),
            "---\nname: never closed\n",
        );
        write(
            &data.join("skills/.hidden/SKILL.md"),
            "---\nname: hidden\n---\n",
        );
        write(&data.join("skills/no-skill-file/README.md"), "Not a skill.");

        let skills = list_in(&data, None).unwrap();
        let get = |name: &str| skills.iter().find(|s| s.name == name).unwrap();
        assert_eq!(
            names(&skills),
            ["bare-folder", "continued", "It's quoted", "pdf", "unclosed"]
        );
        assert_eq!(get("pdf").description, "Fill and merge PDF forms.");
        assert_eq!(get("pdf").body, "# PDF\n\nUse pypdf.");
        assert_eq!(get("It's quoted").description, "Says \"hi\"");
        assert_eq!(get("continued").description, "Spread over two lines");
        assert_eq!(
            get("bare-folder").description,
            "The first real paragraph says what it is for."
        );
        // Frontmatter that never closes is read as body, not lost.
        assert_eq!(get("unclosed").body, "---\nname: never closed");
    }

    #[test]
    fn a_literal_block_keeps_its_lines() {
        let (entries, body) = split("---\nnotes: |\n  one\n  two\n---\n");
        assert_eq!(value(&entries[0]), "one\ntwo");
        assert_eq!(body, "");
    }

    #[test]
    fn values_that_yaml_would_misread_are_quoted() {
        for text in [
            "plain words",
            "a: b",
            "#tag",
            "\"quoted\"",
            "- dash",
            "back\\slash",
            "ends:",
        ] {
            let (entries, _) = split(&format!("---\nname: {}\n---\n", scalar(text)));
            assert_eq!(value(&entries[0]), text);
        }
        assert_eq!(scalar("plain words"), "plain words");
    }

    #[test]
    fn project_skills_are_found_up_to_the_git_root_and_yours_win() {
        let data = scratch("project");
        let top = data.join("outside");
        let repo = top.join("repo");
        let deep = repo.join("crates").join("deep");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(&deep).unwrap();
        let skill = |dir: &Path, name: &str, body: &str| {
            write(
                &dir.join("skills").join(name).join("SKILL.md"),
                &format!("---\nname: {name}\ndescription: {body}\n---\n{body}"),
            );
        };
        skill(&data, "mine", "from the user");
        skill(&repo.join(".claude"), "mine", "from the repo");
        skill(&repo.join(".claude"), "shared", "from the repo root");
        skill(&deep.join(".agents"), "shared", "from nearest the project");
        skill(&repo.join(".agents"), "rooted", "at the git root");
        skill(&top.join(".agents"), "above", "beyond the git root");

        let skills = list_in(&data, Some(&deep)).unwrap();
        let get = |name: &str| skills.iter().find(|s| s.name == name).unwrap();
        assert!(get("mine").source == Source::User && get("mine").body == "from the user");
        assert_eq!(get("shared").body, "from nearest the project");
        assert_eq!(get("shared").source, Source::Project);
        assert_eq!(get("rooted").source, Source::Project);
        assert!(!names(&skills).contains(&"above"));
        assert_eq!(skills.iter().filter(|s| s.name == "mine").count(), 1);
    }

    #[test]
    fn outside_a_repository_only_the_project_folder_is_looked_in() {
        let data = scratch("no-repo");
        let project = data.join("downloads").join("thing");
        write(
            &project.join(".agents/skills/here/SKILL.md"),
            "---\nname: here\n---\nBody",
        );
        write(
            &data.join("downloads/.agents/skills/above/SKILL.md"),
            "---\nname: above\n---\nBody",
        );
        let listed = list_in(&data, Some(&project)).unwrap();
        assert!(names(&listed).contains(&"here"));
        assert!(!names(&listed).contains(&"above"));
    }

    #[test]
    fn switched_off_skills_stay_listed_and_new_ones_start_on() {
        let data = scratch("switches");
        list_in(&data, None).unwrap();
        set_enabled_in(&data, "run the tests", true).unwrap();
        let skills = list_in(&data, None).unwrap();
        let on: Vec<&str> = skills
            .iter()
            .filter(|s| s.enabled)
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(on, ["Run the tests"]);

        let made = create_in(&data, "Deploy", "", "Ship it.").unwrap();
        assert!(made.enabled);

        for name in ["Explain this project", "Review these changes"] {
            set_enabled_in(&data, name, true).unwrap();
        }
        assert!(list_in(&data, None).unwrap().iter().all(|s| s.enabled));
        assert_eq!(
            std::fs::read_to_string(data.join("skills-off.json")).unwrap(),
            "[]"
        );
    }

    #[test]
    fn a_broken_switch_file_is_reported_rather_than_read_as_all_on() {
        let data = scratch("broken-switches");
        std::fs::write(data.join("skills-off.json"), "[\"a\",").unwrap();
        let error = list_in(&data, None).unwrap_err();
        assert!(error.contains("skills-off.json"), "{error}");
        assert!(set_enabled_in(&data, "a", true).is_err());
    }

    #[test]
    fn skills_are_made_edited_and_removed_in_your_folder() {
        let data = scratch("edits");
        let made = create_in(
            &data,
            "Write a release note",
            "",
            "Read the log.\n\nThen write.",
        )
        .unwrap();
        assert_eq!(made.description, "Read the log.");
        assert_eq!(
            made.folder,
            data.join("skills").join("write-a-release-note")
        );
        assert!(create_in(&data, "write a RELEASE note", "", "Again").is_err());
        assert!(create_in(&data, "Empty", "", "  ").is_err());
        assert!(create_in(&data, " ", "", "No name").is_err());

        // A description taken from the body follows it.
        let edited = edit_in(&data, &made.name, "Release note", None, "Read the tags.").unwrap();
        assert_eq!(edited.name, "Release note");
        assert_eq!(edited.description, "Read the tags.");
        assert_eq!(edited.folder, made.folder);

        // A written one stays, and the switch goes with a new name.
        set_enabled_in(&data, "Release note", false).unwrap();
        edit_in(
            &data,
            "release note",
            "Notes",
            Some("When shipping."),
            "Body",
        )
        .unwrap();
        let edited = edit_in(&data, "Notes", "Notes", None, "Another body").unwrap();
        assert_eq!(edited.description, "When shipping.");
        assert!(!edited.enabled);

        let archived = remove_in(&data, "notes").unwrap();
        assert!(archived.join("SKILL.md").is_file());
        assert!(archived.starts_with(data.join("skills-archive")));
        assert!(!made.folder.exists());
        assert!(!names(&list_in(&data, None).unwrap()).contains(&"Notes"));
        assert!(remove_in(&data, "Notes").is_err());
        // Made again, it starts on.
        assert!(create_in(&data, "Notes", "", "Fresh").unwrap().enabled);
    }

    #[test]
    fn an_edit_keeps_the_frontmatter_it_does_not_change() {
        let data = scratch("keeps");
        write(
            &data.join("skills/pdf/SKILL.md"),
            "---\nname: pdf\ndescription: >\n  Fill PDF forms.\nlicense: Proprietary\nmetadata:\n  version: 2\n---\nOld body",
        );
        edit_in(&data, "pdf", "pdf", None, "New body").unwrap();
        let text = std::fs::read_to_string(data.join("skills/pdf/SKILL.md")).unwrap();
        assert_eq!(
            text,
            "---\nname: pdf\ndescription: Fill PDF forms.\nlicense: Proprietary\nmetadata:\n  version: 2\n---\n\nNew body\n"
        );
    }

    #[test]
    fn a_project_skill_is_not_changed_from_here() {
        let data = scratch("not-yours");
        let project = data.join("project");
        write(
            &project.join(".agents/skills/theirs/SKILL.md"),
            "---\nname: theirs\n---\nBody",
        );
        assert!(names(&list_in(&data, Some(&project)).unwrap()).contains(&"theirs"));
        let error = edit_in(&data, "theirs", "mine", None, "Body").unwrap_err();
        assert!(error.contains("project"), "{error}");
        assert!(remove_in(&data, "theirs").is_err());
    }

    #[test]
    fn folder_names_are_safe_everywhere() {
        assert_eq!(slug("Review these changes"), "review-these-changes");
        assert_eq!(slug("  C++ / Rust!  "), "c-rust");
        assert_eq!(slug("CON"), "con-skill");
        assert_eq!(slug("com1"), "com1-skill");
        assert_eq!(slug("COM\u{b9}"), "com\u{b9}-skill");
        assert_eq!(slug("lpt0"), "lpt0-skill");
        assert_eq!(slug("com10"), "com10");
        assert!(super::device("Aux.md") && super::device("con .txt") && !super::device("console"));
        assert_eq!(slug("!!!"), "skill");
        assert_eq!(slug("Über"), "über");
        assert_eq!(slug(&"a".repeat(100)).len(), 64);
        // Lowercasing can change a name's length in bytes.
        assert_eq!(slug("\u{212a}elvin"), "kelvin");
        assert_eq!(slug("İstanbul."), "i-stanbul");
        assert_eq!(slug(&"é".repeat(100)).chars().count(), 64);
    }

    #[test]
    fn a_skill_file_that_is_not_a_plain_file_is_passed_over() {
        let data = scratch("not-a-file");
        std::fs::create_dir_all(data.join("skills/folder/SKILL.md")).unwrap();
        write(
            &data.join("skills/big/SKILL.md"),
            &"x".repeat(super::SKILL_BYTES as usize + 1),
        );
        write(&data.join("skills/fine/SKILL.md"), "Fine.");
        #[cfg(unix)]
        {
            std::fs::create_dir_all(data.join("skills/device")).unwrap();
            std::os::unix::fs::symlink("/dev/zero", data.join("skills/device/SKILL.md")).unwrap();
        }
        assert_eq!(names(&list_in(&data, None).unwrap()), ["fine"]);
    }

    #[test]
    fn names_cannot_carry_new_lines_or_escapes() {
        let data = scratch("printable");
        write(
            &data.join("skills/x/SKILL.md"),
            "---\nname: \"two\\nlines\"\ndescription: \"red \u{1b}[31mtext\"\n---\nBody",
        );
        let skills = list_in(&data, None).unwrap();
        assert_eq!(skills[0].name, "two lines");
        assert_eq!(skills[0].description, "red [31mtext");
    }

    #[test]
    fn a_project_skill_folder_that_is_a_link_is_passed_over() {
        let data = scratch("linked");
        let project = data.join("project");
        write(
            &data.join("elsewhere/SKILL.md"),
            "---\nname: linked\n---\nBody",
        );
        write(
            &project.join(".agents/skills/real/SKILL.md"),
            "---\nname: real\n---\nBody",
        );
        // Built part by part: mklink reads a `/` as the start of a switch.
        let link = project.join(".agents").join("skills").join("linked");
        #[cfg(unix)]
        std::os::unix::fs::symlink(data.join("elsewhere"), &link).unwrap();
        #[cfg(windows)]
        {
            // A junction needs no privilege, unlike a symbolic link.
            let made = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(data.join("elsewhere"))
                .output()
                .unwrap();
            assert!(made.status.success(), "{made:?}");
        }
        assert!(link.join("SKILL.md").is_file());
        let listed = list_in(&data, Some(&project)).unwrap();
        assert!(names(&listed).contains(&"real"));
        assert!(!names(&listed).contains(&"linked"));
    }

    #[test]
    fn a_skill_that_cannot_be_moved_away_keeps_its_switch() {
        let data = scratch("stuck");
        create_in(&data, "Keep", "", "Body").unwrap();
        set_enabled_in(&data, "Keep", false).unwrap();
        // The archive cannot be made where a file already is.
        std::fs::write(data.join("skills-archive"), "").unwrap();
        assert!(remove_in(&data, "Keep").is_err());
        let listed = list_in(&data, None).unwrap();
        assert!(listed.iter().any(|s| s.name == "Keep" && !s.enabled));
    }
}
