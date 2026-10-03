//! The project's files as the window's Files tab shows them: a tree read a
//! folder at a time, what a file looks like when it is previewed, and the
//! ways a file is handed on -- to the message, to an editor.
//!
//! It reads only when asked. There is no watcher: the window reads the
//! folders it has open again after a tool comes back and when it is brought
//! to the front, and a hidden tab reads nothing, so a project that is not
//! being looked at costs nothing. Each folder is read with `.gitignore`
//! honoured as `search` honours it; the ignored entries are read too only
//! when the user asks to see them, and are marked so they can be told apart.
//!
//! A preview goes through the checks a tool's read does: the path must
//! resolve inside the project, never into Letronna's own folder, and a
//! file holding keys is not opened here -- the system can open it, at the
//! user's own hand. A preview is for reading, so a file too big or not text
//! is handed to the system instead of being drawn badly.

use crate::chat::Message;
use ignore::WalkBuilder;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// The largest text file previewed. Past this a file is data or generated,
/// and its lines would cost more to lay out than they are worth reading here.
pub const PREVIEW_BYTES: u64 = 512 * 1024;
/// The largest picture previewed: a photo is often a few megabytes, and one
/// bigger than this is decoded into more memory than a glance deserves.
pub const PICTURE_BYTES: u64 = 10 * 1024 * 1024;
/// Folders a version control system keeps for itself, never shown: they
/// are not the project, and `.git` alone can hold thousands of entries.
const NEVER: &[&str] = &[".git", ".hg", ".svn"];
/// Pictures the window can draw.
const PICTURES: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];
/// Files shown rendered rather than as lines.
const MARKDOWN: &[&str] = &["md", "markdown", "mdx"];
/// Editors that open a file from their command, looked for on PATH in this
/// order. `$EDITOR` is not asked: it names a terminal editor more often
/// than not, and one started from a window has no terminal to draw in.
const EDITORS: &[&str] = &["code", "cursor", "zed", "subl", "windsurf"];

/// One file or folder in a folder's listing.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub dir: bool,
    /// Left out by a `.gitignore`, and shown only because ignored files are.
    pub ignored: bool,
}

/// What a folder or file is judged as in the checks every read makes.
fn reachable(root: &Path, path: &Path, data: &Path) -> Result<PathBuf, String> {
    let shown = label(root, path);
    // The tree holds canonical paths, which Windows spells `\\?\C:\...`, the
    // device form `unsafe_raw` refuses; it is judged as the plain path.
    let (plain_path, plain_root) = (plain(path), plain(root));
    if let Some(why) = crate::paths::unsafe_raw(&plain_path, Some(Path::new(&plain_root))) {
        return Err(format!("{shown} {why}."));
    }
    let root = std::fs::canonicalize(root)
        .map_err(|_| "The project folder is gone. Pick it again.".to_owned())?;
    let real = std::fs::canonicalize(path).map_err(|_| format!("{shown} is gone."))?;
    if !real.starts_with(&root) {
        return Err(format!("{shown} leads outside the project."));
    }
    if crate::tools::private(&real, data) {
        return Err(format!(
            "{shown} is in Letronna's own folder, with the keys and sign-in."
        ));
    }
    Ok(real)
}

/// `path` as text without the `\\?\` that Windows puts on a canonical path.
fn plain(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(share) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{share}");
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_owned(),
        _ => text.into_owned(),
    }
}

/// A path as the user knows it: from the project folder down.
fn label(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    match relative.as_os_str().is_empty() {
        true => "The project folder".to_owned(),
        false => format!("`{}`", slashed(relative)),
    }
}

/// A relative path with `/` between its names on every system.
fn slashed(relative: &Path) -> String {
    let names: Vec<String> = relative
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    names.join("/")
}

/// The folder `dir` of the project `root`, one level deep: folders first,
/// then files, each by name. With `ignored`, what `.gitignore` leaves out
/// is listed too, marked.
pub fn list(root: &Path, dir: &Path, ignored: bool) -> Result<Vec<Entry>, String> {
    list_in(root, dir, ignored, &crate::chat::data_dir())
}

fn list_in(root: &Path, dir: &Path, ignored: bool, data: &Path) -> Result<Vec<Entry>, String> {
    let dir = reachable(root, dir, data)?;
    let kept = unignored(&dir);
    let names: Vec<OsString> = match ignored {
        false => kept.iter().cloned().collect(),
        true => std::fs::read_dir(&dir)
            .map_err(|e| format!("{} could not be read: {e}", label(root, &dir)))?
            .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
            .collect(),
    };
    let mut entries: Vec<Entry> = names
        .into_iter()
        .filter(|name| !NEVER.iter().any(|never| name == *never))
        .map(|name| {
            let path = dir.join(&name);
            Entry {
                ignored: !kept.contains(&name),
                dir: path.is_dir(),
                name: name.to_string_lossy().into_owned(),
                path,
            }
        })
        .filter(|entry| !crate::tools::private(&entry.path, data))
        .collect();
    entries.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(entries)
}

/// The names in `dir` that no `.gitignore` or `.ignore` leaves out, read as
/// `search` reads them: inside a repository its rules count from its root
/// down, outside one only the folder's own.
fn unignored(dir: &Path) -> HashSet<OsString> {
    let repo = dir.ancestors().any(|up| up.join(".git").exists());
    let mut walk = WalkBuilder::new(dir);
    walk.hidden(false)
        .require_git(repo)
        .parents(repo)
        .max_depth(Some(1));
    walk.build()
        .filter_map(Result::ok)
        .filter(|entry| entry.depth() == 1)
        .map(|entry| entry.file_name().to_owned())
        .collect()
}

/// What a file looks like in the preview.
#[derive(Clone, Debug, PartialEq)]
pub enum Preview {
    Markdown(String),
    Text(String),
    Picture(PathBuf),
    /// Not drawn here, and why; the system's own program opens it instead.
    Elsewhere(String),
}

/// How `path`, a file in the project `root`, is previewed.
pub fn preview(root: &Path, path: &Path) -> Result<Preview, String> {
    preview_in(root, path, &crate::chat::data_dir())
}

fn preview_in(root: &Path, path: &Path, data: &Path) -> Result<Preview, String> {
    let real = reachable(root, path, data)?;
    let meta = std::fs::metadata(&real)
        .map_err(|e| format!("{} could not be read: {e}", label(root, path)))?;
    if meta.is_dir() {
        return Err(format!("{} is a folder.", label(root, path)));
    }
    // Opening a named pipe waits for a writer that may never come.
    if !meta.is_file() {
        return Ok(Preview::Elsewhere("It is not a regular file.".into()));
    }
    if crate::tools::secret(&real) {
        return Ok(Preview::Elsewhere(
            "It holds keys, passwords or tokens, so it is not shown here.".into(),
        ));
    }
    let extension = real
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if PICTURES.contains(&extension.as_str()) {
        if meta.len() > PICTURE_BYTES {
            return Ok(Preview::Elsewhere(format!(
                "It is over {} MB, too big to preview.",
                PICTURE_BYTES / 1024 / 1024
            )));
        }
        return Ok(Preview::Picture(real));
    }
    if meta.len() > PREVIEW_BYTES {
        return Ok(Preview::Elsewhere(format!(
            "It is over {} KB, too big to preview.",
            PREVIEW_BYTES / 1024
        )));
    }
    // Read no more than the limit even if the file grew since it was measured.
    let mut bytes = Vec::new();
    std::fs::File::open(&real)
        .and_then(|file| file.take(PREVIEW_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|e| format!("{} could not be read: {e}", label(root, path)))?;
    if bytes.len() as u64 > PREVIEW_BYTES {
        return Ok(Preview::Elsewhere(format!(
            "It is over {} KB, too big to preview.",
            PREVIEW_BYTES / 1024
        )));
    }
    let Some(text) = text_of(bytes) else {
        return Ok(Preview::Elsewhere("It is not text.".into()));
    };
    Ok(match MARKDOWN.contains(&extension.as_str()) {
        true => Preview::Markdown(text),
        false => Preview::Text(text),
    })
}

/// The text in `bytes`, or None when they are not text: a NUL is never in
/// text a person reads, and anything else not UTF-8 would show as boxes.
fn text_of(bytes: Vec<u8>) -> Option<String> {
    if bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// How a message names `path`: `@` and the path from the project folder,
/// in quotes when it has a space the model would read as its end.
pub fn reference(root: &Path, path: &Path) -> String {
    let relative = slashed(path.strip_prefix(root).unwrap_or(path));
    match relative.contains(char::is_whitespace) {
        true => format!("@\"{relative}\""),
        false => format!("@{relative}"),
    }
}

/// The files a session's tools wrote, by their real paths.
pub fn touched(messages: &[Message]) -> HashSet<PathBuf> {
    messages
        .iter()
        .flat_map(|message| &message.files)
        .map(|change| change.path.clone())
        .collect()
}

/// Open `path` in the first editor on PATH that takes a file from its
/// command: Ok(false) when there is none, for the system to open it instead.
pub fn open_in_editor(path: &Path) -> Result<bool, String> {
    let Some(editor) = EDITORS.iter().find_map(|name| on_path(name)) else {
        return Ok(false);
    };
    let login = crate::login_env::vars();
    std::process::Command::new(&editor)
        .arg(path)
        .envs(login.iter().map(|(name, value)| (name, value)))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| true)
        .map_err(|e| format!("{} would not start: {e}", editor.display()))
}

/// Where the program `name` is on PATH, as the login shell sets it.
pub(crate) fn on_path(name: &str) -> Option<PathBuf> {
    let login = crate::login_env::vars();
    let path = login
        .iter()
        .find(|(key, _)| key == "PATH")
        .map(|(_, value)| value.clone())
        .or_else(|| std::env::var_os("PATH"))?;
    // On Windows an editor's command is a script beside its program.
    let endings: &[&str] = if cfg!(windows) {
        &[".cmd", ".exe"]
    } else {
        &[""]
    };
    std::env::split_paths(&path)
        .flat_map(|dir| {
            endings
                .iter()
                .map(move |end| dir.join(format!("{name}{end}")))
        })
        .find(|candidate| candidate.is_file())
}

/// A tree of the project's folders, the open ones read, and the row picked
/// out; the window draws its rows and hands it the keys.
#[derive(Default)]
pub struct Tree {
    root: Option<PathBuf>,
    /// Whether what `.gitignore` leaves out is listed too.
    ignored: bool,
    listing: HashMap<PathBuf, Vec<Entry>>,
    open: HashSet<PathBuf>,
    selected: Option<PathBuf>,
}

/// One row of the tree as it is drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub entry: Entry,
    pub depth: usize,
    pub open: bool,
}

/// What a folder read found: the folders asked for that could be read.
pub struct Listing {
    root: PathBuf,
    ignored: bool,
    dirs: HashMap<PathBuf, Vec<Entry>>,
}

impl Tree {
    /// The project folder shown, canonical.
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// Show the project `root`, or nothing; a different folder starts over.
    pub fn show(&mut self, root: Option<&Path>) {
        let root = root.map(|root| std::fs::canonicalize(root).unwrap_or(root.to_path_buf()));
        if root != self.root {
            *self = Tree {
                root,
                ignored: self.ignored,
                ..Tree::default()
            };
        }
    }

    pub fn ignored(&self) -> bool {
        self.ignored
    }

    /// Show or hide what `.gitignore` leaves out; the folders need reading again.
    pub fn show_ignored(&mut self, ignored: bool) {
        self.ignored = ignored;
    }

    pub fn selected(&self) -> Option<&Path> {
        self.selected.as_deref()
    }

    pub fn select(&mut self, path: &Path) {
        self.selected = Some(path.to_path_buf());
    }

    /// What a read is to cover: the project folder, the open folders, and
    /// whether ignored entries are wanted.
    pub fn wanted(&self) -> Option<(PathBuf, Vec<PathBuf>, bool)> {
        let root = self.root.clone()?;
        let mut dirs = vec![root.clone()];
        dirs.extend(self.open.iter().cloned());
        Some((root, dirs, self.ignored))
    }

    /// Whether an open folder has not been read yet.
    pub fn unread(&self) -> bool {
        self.open.iter().any(|dir| !self.listing.contains_key(dir))
    }

    /// Take what a read found, unless the tree moved on to another folder
    /// or setting since it began. Open folders that are gone close, and a
    /// row that is gone gives the pick to its folder.
    pub fn take(&mut self, read: Listing) {
        if Some(&read.root) != self.root.as_ref() || read.ignored != self.ignored {
            return;
        }
        self.listing = read.dirs;
        let listing = &self.listing;
        self.open.retain(|dir| listing.contains_key(dir));
        let shown: HashSet<PathBuf> = self.rows().into_iter().map(|row| row.entry.path).collect();
        if let Some(selected) = self.selected.clone() {
            self.selected = selected
                .ancestors()
                .find(|up| shown.contains(*up))
                .map(Path::to_path_buf);
        }
    }

    /// The rows drawn: each open folder's entries under it.
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        if let Some(root) = &self.root {
            self.add_rows(root, 0, &mut rows);
        }
        rows
    }

    fn add_rows(&self, dir: &Path, depth: usize, rows: &mut Vec<Row>) {
        for entry in self.listing.get(dir).into_iter().flatten() {
            let open = entry.dir && self.open.contains(&entry.path);
            rows.push(Row {
                entry: entry.clone(),
                depth,
                open,
            });
            if open {
                self.add_rows(&entry.path, depth + 1, rows);
            }
        }
    }

    /// Where the picked row is among `rows`.
    pub fn place(&self, rows: &[Row]) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        rows.iter().position(|row| &row.entry.path == selected)
    }

    /// Pick the row `by` rows down (or up, below zero), stopping at the
    /// ends; with none picked, the first.
    pub fn step(&mut self, by: isize) {
        let rows = self.rows();
        let Some(last) = rows.len().checked_sub(1) else {
            return;
        };
        let to = match self.place(&rows) {
            Some(at) => at.saturating_add_signed(by).min(last),
            None => 0,
        };
        self.selected = Some(rows[to].entry.path.clone());
    }

    /// Right: open the picked folder, or step into it once it is open.
    /// True when a folder opened, and wants reading.
    pub fn right(&mut self) -> bool {
        let rows = self.rows();
        let Some(at) = self.place(&rows) else {
            return false;
        };
        let row = &rows[at];
        if !row.entry.dir {
            return false;
        }
        if !row.open {
            self.open.insert(row.entry.path.clone());
            return true;
        }
        if let Some(first) = rows.get(at + 1).filter(|next| next.depth > row.depth) {
            self.selected = Some(first.entry.path.clone());
        }
        false
    }

    /// Left: close the picked folder, or pick the folder the row is in.
    pub fn left(&mut self) {
        let Some(selected) = self.selected.clone() else {
            return;
        };
        if self.open.remove(&selected) {
            return;
        }
        let parent = selected.parent().map(Path::to_path_buf);
        if parent.as_ref() != self.root.as_ref() {
            self.selected = parent;
        }
    }

    /// Open or close a folder, picking it. True when it opened.
    pub fn toggle(&mut self, dir: &Path) -> bool {
        self.selected = Some(dir.to_path_buf());
        if self.open.remove(dir) {
            return false;
        }
        self.open.insert(dir.to_path_buf());
        true
    }
}

/// Read the folders `dirs` of the project `root`: an error when the
/// project folder itself cannot be read, and any other that cannot left out.
pub fn read(root: &Path, dirs: &[PathBuf], ignored: bool) -> Result<Listing, String> {
    read_in(root, dirs, ignored, &crate::chat::data_dir())
}

fn read_in(root: &Path, dirs: &[PathBuf], ignored: bool, data: &Path) -> Result<Listing, String> {
    let mut read = HashMap::new();
    for dir in dirs {
        match list_in(root, dir, ignored, data) {
            Ok(entries) => {
                read.insert(dir.clone(), entries);
            }
            Err(why) if dir == root => return Err(why),
            Err(_) => {}
        }
    }
    Ok(Listing {
        root: root.to_path_buf(),
        ignored,
        dirs: read,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh project holding `files` (a name ending `/` is a folder), and
    /// a data folder of its own beside it, both canonical.
    fn project(name: &str, files: &[(&str, &[u8])]) -> (PathBuf, PathBuf) {
        let base =
            std::env::temp_dir().join(format!("letronna-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (root, data) = (base.join("project"), base.join("data"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        for (path, bytes) in files {
            let path = root.join(path);
            if path.to_string_lossy().ends_with('/') {
                std::fs::create_dir_all(&path).unwrap();
                continue;
            }
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        (
            std::fs::canonicalize(root).unwrap(),
            std::fs::canonicalize(data).unwrap(),
        )
    }

    fn names(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| match (entry.dir, entry.ignored) {
                (true, _) => format!("{}/", entry.name),
                (false, true) => format!("({})", entry.name),
                (false, false) => entry.name.clone(),
            })
            .collect()
    }

    fn tree_of(root: &Path, data: &Path) -> Tree {
        let mut tree = Tree::default();
        tree.show(Some(root));
        refresh(&mut tree, data);
        tree
    }

    fn refresh(tree: &mut Tree, data: &Path) {
        let (root, dirs, ignored) = tree.wanted().unwrap();
        tree.take(read_in(&root, &dirs, ignored, data).unwrap());
    }

    fn drawn(tree: &Tree) -> Vec<String> {
        tree.rows()
            .iter()
            .map(|row| format!("{}{}", "  ".repeat(row.depth), row.entry.name))
            .collect()
    }

    #[test]
    fn a_folder_lists_folders_first_and_hides_what_gitignore_leaves_out_until_asked() {
        let (root, data) = project(
            "list",
            &[
                (".gitignore", b"*.log\n"),
                ("b.txt", b"b"),
                ("A.md", b"a"),
                ("run.log", b"x"),
                ("src/main.rs", b"fn main() {}"),
                (".git/HEAD", b"ref"),
            ],
        );
        let shown = list_in(&root, &root, false, &data).unwrap();
        assert_eq!(names(&shown), ["src/", ".gitignore", "A.md", "b.txt"]);
        let all = list_in(&root, &root, true, &data).unwrap();
        assert_eq!(
            names(&all),
            ["src/", ".gitignore", "A.md", "b.txt", "(run.log)"]
        );
    }

    #[test]
    fn nothing_outside_the_project_or_in_letronnas_own_folder_is_listed() {
        let (root, _) = project("outside", &[("a.txt", b"a")]);
        let outside = root.parent().unwrap().to_path_buf();
        assert!(list_in(&root, &outside, false, &root.join("none")).is_err());
        // A project that holds the data folder never shows it.
        let data = root.join("keys");
        std::fs::create_dir_all(&data).unwrap();
        let listed = list_in(&root, &root, true, &data).unwrap();
        assert_eq!(names(&listed), ["a.txt"]);
        assert!(preview_in(&root, &root.join("keys"), &data).is_err());
    }

    #[test]
    fn a_preview_is_markdown_text_a_picture_or_handed_to_the_system() {
        let big = vec![b'a'; PREVIEW_BYTES as usize + 1];
        let (root, data) = project(
            "preview",
            &[
                ("README.md", b"# Hi"),
                ("main.rs", b"fn main() {}"),
                ("logo.png", b"\x89PNG"),
                ("blob.bin", b"a\0b"),
                ("latin.txt", b"caf\xe9"),
                ("big.txt", &big),
                (".env", b"KEY=1"),
            ],
        );
        let at = |name: &str| preview_in(&root, &root.join(name), &data).unwrap();
        assert_eq!(at("README.md"), Preview::Markdown("# Hi".into()));
        assert_eq!(at("main.rs"), Preview::Text("fn main() {}".into()));
        assert_eq!(at("logo.png"), Preview::Picture(root.join("logo.png")));
        for name in ["blob.bin", "latin.txt", "big.txt", ".env"] {
            assert!(matches!(at(name), Preview::Elsewhere(_)), "{name}");
        }
        assert!(preview_in(&root, &root, &data).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_named_pipe_is_handed_to_the_system_not_waited_on() {
        let (root, data) = project("pipe", &[("a.txt", b"a")]);
        let pipe = root.join("jobs");
        let made = std::process::Command::new("mkfifo").arg(&pipe).status();
        assert!(made.is_ok_and(|status| status.success()));
        let Ok(Preview::Elsewhere(why)) = preview_in(&root, &pipe, &data) else {
            panic!("a pipe should be handed on");
        };
        assert_eq!(why, "It is not a regular file.");
    }

    #[test]
    fn a_canonical_windows_path_loses_its_verbatim_prefix() {
        assert_eq!(plain(Path::new(r"\\?\C:\work\app")), r"C:\work\app");
        assert_eq!(plain(Path::new(r"\\?\UNC\nas\p\app")), r"\\nas\p\app");
        assert_eq!(plain(Path::new(r"\\?\GLOBALROOT\x")), r"\\?\GLOBALROOT\x");
        assert_eq!(plain(Path::new("/work/app")), "/work/app");
    }

    #[test]
    fn a_reference_names_the_file_from_the_project_folder() {
        let root = Path::new("/work/app");
        assert_eq!(
            reference(root, &root.join("src").join("main.rs")),
            "@src/main.rs"
        );
        assert_eq!(
            reference(root, &root.join("my notes.md")),
            "@\"my notes.md\""
        );
    }

    #[test]
    fn right_opens_a_folder_then_steps_in_and_left_closes_it_then_steps_out() {
        let (root, data) = project(
            "keys",
            &[("src/a.rs", b""), ("src/b.rs", b""), ("z.txt", b"")],
        );
        let mut tree = tree_of(&root, &data);
        assert_eq!(drawn(&tree), ["src", "z.txt"]);
        tree.step(1);
        assert_eq!(tree.selected(), Some(root.join("src").as_path()));
        assert!(tree.right());
        assert!(tree.unread());
        refresh(&mut tree, &data);
        assert_eq!(drawn(&tree), ["src", "  a.rs", "  b.rs", "z.txt"]);
        assert!(!tree.right());
        assert_eq!(tree.selected(), Some(root.join("src/a.rs").as_path()));
        tree.step(10);
        assert_eq!(tree.selected(), Some(root.join("z.txt").as_path()));
        tree.step(-2);
        tree.left();
        assert_eq!(tree.selected(), Some(root.join("src").as_path()));
        tree.left();
        assert_eq!(drawn(&tree), ["src", "z.txt"]);
        // At the top the pick stays put rather than leaving the tree.
        tree.left();
        assert_eq!(tree.selected(), Some(root.join("src").as_path()));
    }

    #[test]
    fn a_read_again_keeps_the_pick_or_hands_it_to_the_folder_of_what_went() {
        let (root, data) = project("again", &[("src/a.rs", b""), ("b.txt", b"")]);
        let mut tree = tree_of(&root, &data);
        tree.toggle(&root.join("src"));
        refresh(&mut tree, &data);
        tree.select(&root.join("src/a.rs"));
        std::fs::remove_file(root.join("src/a.rs")).unwrap();
        refresh(&mut tree, &data);
        assert_eq!(tree.selected(), Some(root.join("src").as_path()));
        std::fs::remove_dir_all(root.join("src")).unwrap();
        refresh(&mut tree, &data);
        assert_eq!(drawn(&tree), ["b.txt"]);
        assert_eq!(tree.selected(), None);
    }

    #[test]
    fn a_read_for_another_folder_or_setting_is_dropped() {
        let (root, data) = project("late", &[("a.txt", b"")]);
        let mut tree = tree_of(&root, &data);
        let late = read_in(&root, std::slice::from_ref(&root), false, &data).unwrap();
        tree.show_ignored(true);
        tree.take(Listing {
            dirs: HashMap::new(),
            ..late
        });
        assert_eq!(drawn(&tree), ["a.txt"]);
    }

    #[test]
    fn the_files_a_session_wrote_are_the_ones_it_touched() {
        let wrote = |paths: &[&str]| Message {
            files: paths
                .iter()
                .map(|path| crate::history::FileChange {
                    path: PathBuf::from(path),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let messages = [
            wrote(&["/p/a.rs", "/p/b.rs"]),
            wrote(&[]),
            wrote(&["/p/a.rs"]),
        ];
        let touched = touched(&messages);
        assert_eq!(touched.len(), 2);
        assert!(touched.contains(Path::new("/p/b.rs")));
    }
}
