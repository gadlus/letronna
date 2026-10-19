//! The paths a request names that are not there as its run begins.
//!
//! What a request names that is there as the run begins is input. What is
//! not is the run's to make, what a program it writes makes when run, or an
//! example. A run nobody watches that ends on its own with one of them still
//! not there has more often stopped early than finished, and `contract`
//! only looks for the kinds of file it reads, under `out/` or asked for in
//! so many words. So the paths are noted as the run begins, and each still
//! not there at the end is named once in the check that sends it back, to
//! weigh against the request.
//!
//! Since inputs are there, what a sentence does with a path is not read;
//! the words are only used to leave out what names no file to make:
//! placeholders, URLs, flags, a line or port after a path, code, a diff,
//! examples, what a command the request shows writes, and files it says are
//! optional, not to be made or for later. Only a path with its folder
//! counts: a name alone is as often a library's or a module's, or one meant
//! wherever it is found.

use crate::contract::{self, Contract};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

/// Paths of a request looked up as the run begins: more than any list of
/// files to make, and each is a look at the disk.
const LOOKED: usize = 64;

/// The paths a request names that were not there as its run began.
#[derive(Debug, Default)]
pub(crate) struct Missing {
    workspace: PathBuf,
    /// Each as the request first writes it.
    names: Vec<String>,
}

/// The paths `text` names that are not in `workspace` now, leaving the files
/// `contract` asks for to it.
pub(crate) fn from_request(text: &str, workspace: &Path, contract: &Contract) -> Missing {
    let built = built(text, workspace);
    let mut seen = HashSet::new();
    let mut names = Vec::new();
    for word in contract::named_words(text) {
        // Opening a path on Windows can sign in to another computer, and
        // this one comes from the request's text.
        if !names_a_file(word) || crate::paths::unsafe_raw(word, Some(workspace)).is_some() {
            continue;
        }
        let path = workspace.join(word);
        if !seen.insert(path.clone()) {
            continue;
        }
        if seen.len() > LOOKED {
            break;
        }
        if !contract.covers(word, workspace) && !built.contains(&path) && absent(&path) {
            names.push(word.to_owned());
        }
    }
    Missing {
        workspace: workspace.to_owned(),
        names,
    }
}

/// A line for each path `missing` noted that is still not there, naming it
/// as the request does.
pub(crate) fn unmade(missing: &Missing) -> Vec<String> {
    let workspace = &missing.workspace;
    let left = missing.names.iter().filter(|name| {
        let path = workspace.join(name);
        // One outside the workspace counts once its folder is there, so a
        // path from another computer, as in a traceback, never does.
        let here = path.starts_with(workspace) || path.parent().is_some_and(Path::is_dir);
        here && absent(&path)
    });
    let line = |name: &String| format!("`{name}` does not exist, though the request names it.");
    left.map(line).collect()
}

/// Whether `word` reads as the path of one file, with its folder: from the
/// root in two parts or more, as `POST /items` names a route as often, or
/// from a folder that is not a host's, ending in an extension, as `and/or`
/// and `src/lib` do not.
fn names_a_file(word: &str) -> bool {
    // A placeholder, an address, a URL, a query, a flag, a path in a home
    // folder, which is the user's, or a folder, which is often one to read.
    let marks = ['<', '>', '{', '}', '$', '*', '?', '%', '@', '=', '#'];
    if word.contains("://")
        || word.contains(marks)
        || word.starts_with(['-', '+', '~'])
        || word.ends_with(['/', '\\'])
        || word
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(holds_a_place)
    {
        return false;
    }
    // Outside Windows, a backslash is a string's escape, as in `a\nb`.
    if !cfg!(windows) && word.contains('\\') {
        return false;
    }
    // A line or a port after a path, as in `src/app.js:42:13` or
    // `localhost:8080/data.json`; on Windows a path may begin with a drive's.
    let past_drive = match word.as_bytes() {
        [letter, b':', ..] if cfg!(windows) && letter.is_ascii_alphabetic() => &word[2..],
        _ => word,
    };
    if past_drive.contains(':') {
        return false;
    }
    let path = Path::new(word);
    if path.is_absolute() {
        let parts = path.components();
        let parts = parts.filter(|part| matches!(part, Component::Normal(_)));
        return parts.count() >= 2;
    }
    let mut parts = word.split(['/', '\\']);
    let first = parts.next().unwrap_or_default();
    // A host, as in `github.com/a/b.git`, though not `..` or `.github`.
    let host = first.trim_start_matches('.').contains('.');
    !host && parts.next_back().is_some_and(has_extension)
}

/// Whether `part` of a path holds the place of what will be there, in
/// capitals, as `YYYY` and `NNN` do in `notes/YYYY-MM-DD.md` and
/// `frames/NNN.png`: a file a scheduled run is asked for each time would
/// otherwise send it back each time.
fn holds_a_place(part: &str) -> bool {
    let first = part.bytes().next().filter(u8::is_ascii_uppercase);
    part.len() > 1 && first.is_some_and(|first| part.bytes().all(|b| b == first))
}

/// Whether file name `name` ends in an extension as a file's does: a letter,
/// then letters and digits, after a name.
fn has_extension(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(stem, extension)| {
        let letter = extension.starts_with(|c: char| c.is_ascii_alphabetic());
        !stem.is_empty() && letter && extension.chars().all(|c| c.is_ascii_alphanumeric())
    })
}

/// The paths a command line the request shows writes to: the one after an
/// output flag, as in `gcc main.c -o build/app`, or a redirection, as in
/// `./run > results/out.txt` or `2>>logs/err.txt`. A program makes these
/// when run, and a run told to remove what it made only to try may well
/// have removed them.
fn built(text: &str, workspace: &Path) -> HashSet<PathBuf> {
    let mut built = HashSet::new();
    for line in text.lines() {
        let mut before = "";
        for word in contract::words(line) {
            if let Some(output) = written_to(before, word) {
                built.insert(workspace.join(output));
            }
            before = word;
        }
    }
    built
}

/// The path a command line writes to in `word`, after `before`: after an
/// output flag, as in `-o app`, or a redirection, as in `> out.txt` or
/// `2>>err.log`.
fn written_to<'a>(before: &str, word: &'a str) -> Option<&'a str> {
    if ["-o", "--output"].contains(&before) || past_sign(before) == Some("") {
        return Some(word);
    }
    (past_sign(word).filter(|path| !path.is_empty())).or_else(|| word.strip_prefix("--output="))
}

/// What follows the sign of a redirection `word` opens with, as `out.txt`
/// does in `2>>out.txt`.
fn past_sign(word: &str) -> Option<&str> {
    let sign = word.trim_start_matches(['1', '2', '&']).strip_prefix('>')?;
    Some(sign.trim_start_matches('>'))
}

/// Whether nothing is at `path`, as against something that cannot be looked at.
fn absent(path: &Path) -> bool {
    std::fs::metadata(path).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::Scratch;

    /// A workspace holding each of `files`, by its path from the workspace,
    /// and each of `folders`.
    fn workspace(name: &str, files: &[&str], folders: &[&str]) -> Scratch {
        let dir = Scratch::new(&format!("missing-{name}"));
        for file in files {
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "given").unwrap();
        }
        for folder in folders {
            std::fs::create_dir_all(dir.join(folder)).unwrap();
        }
        dir
    }

    /// The paths `text` names that are not in `dir`, with nothing asked for
    /// under `out/`.
    fn noted(text: &str, dir: &Path) -> Vec<String> {
        from_request(text, dir, &Contract::default()).names
    }

    /// `path` with forward slashes, as a request would write it.
    fn written(path: &Path) -> String {
        path.display().to_string().replace('\\', "/")
    }

    #[test]
    fn a_path_the_request_names_that_is_not_there_is_noted_whatever_its_kind() {
        let dir = workspace("noted", &["firmware/image.bin", "given.txt"], &["work"]);
        let elsewhere = Scratch::new("missing-noted-elsewhere");
        let (ws, other) = (written(&dir), written(&elsewhere));
        let request = format!(
            "I have put a firmware image at {ws}/firmware/image.bin. Write an emulator \
             for it in {ws}/emu/run.js, so that `node emu/run.js` boots it.\n\
             Your task is to read the jobs and produce a plan in work/out/plan.jsonl.\n\
             Build {other}/bin/tool from given.txt, and write the parser as main.scm."
        );
        // A name alone, as main.scm is, is as often meant wherever it is.
        assert_eq!(
            noted(&request, &dir),
            [
                format!("{ws}/emu/run.js"),
                "work/out/plan.jsonl".to_owned(),
                format!("{other}/bin/tool"),
            ]
        );
    }

    #[test]
    fn words_that_name_no_file_are_not_noted() {
        let dir = workspace("not-files", &[], &[]);
        let ws = written(&dir);
        let request = format!(
            "Use re.sub on np.float64 values, and mail team@example.org \
             with read/write and/or https://x.org/data/a.json.\n\
             Mail goes to /srv/inbox/<user> through f\"/srv/inbox/{{name}}\"; link with \
             -L/usr/lib/x.so, serve POST /items, and source ~/.config/app.toml.\n\
             Look in src/, run ./tool and clone github.com/a/b.git.\n\
             It failed at Object.<anonymous> ({ws}/src/app.js:42:13) and with \
             {ws}/src/main.c:12:5: error, after curl localhost:8080/api/data.json.\n\
             Each night, save the digest as digests/YYYY-MM-DD.md. \
             Then write notes/summary.txt."
        );
        assert_eq!(noted(&request, &dir), ["notes/summary.txt"]);
        // Outside Windows, where it would part folders, a backslash is a
        // string's escape.
        #[cfg(not(windows))]
        assert_eq!(
            noted("Check that `printf 'a\\nb/c.txt'` prints two lines.", &dir),
            Vec::<String>::new()
        );
    }

    #[test]
    fn what_a_command_the_request_shows_writes_is_not_noted() {
        let dir = workspace("built", &[], &[]);
        let ws = written(&dir);
        let request = format!(
            "Write src/main.c so that `gcc src/main.c -o {ws}/bin/app && {ws}/bin/app` runs.\n\
             Then `python3 tools/solve.py > results/answer.txt 2>>logs/err.txt` should \
             leave logs/err.txt empty, and `pandoc notes.md --output=build/notes.pdf` make \
             build/notes.pdf.\n\
             Build {ws}/bin/tool from the sources."
        );
        // With no flag or sign before it, a program is the run's to build.
        assert_eq!(
            noted(&request, &dir),
            [
                "src/main.c".to_owned(),
                "tools/solve.py".to_owned(),
                format!("{ws}/bin/tool"),
            ]
        );
    }

    #[test]
    fn files_the_contract_asks_for_are_left_to_it() {
        let dir = workspace("asked", &["notes.txt"], &[]);
        let request = format!(
            "Read notes.txt and write what it says to out/summary.json. \
             The next step reads {}/out/summary.json.",
            written(&dir)
        );
        let contract = crate::contract::from_request(&request, &dir);
        let missing = from_request(&request, &dir, &contract);
        assert_eq!(missing.names, Vec::<String>::new());
    }

    #[test]
    fn a_file_there_as_the_run_began_is_not_noted_even_once_removed() {
        let dir = workspace("removed", &["logs/old.log"], &[]);
        let request = "Compress logs/old.log; the original can go once that works.";
        let missing = from_request(request, &dir, &Contract::default());
        assert_eq!(missing.names, Vec::<String>::new());
        std::fs::remove_file(dir.join("logs/old.log")).unwrap();
        assert_eq!(unmade(&missing), Vec::<String>::new());
    }

    #[test]
    fn unmade_names_each_noted_file_still_not_there_once() {
        let dir = workspace("unmade", &[], &[]);
        let elsewhere = Scratch::new("missing-unmade-elsewhere");
        let gone = format!("{}/gone/c.txt", written(&elsewhere));
        let request = format!(
            "Write docs/a.txt, docs/b.txt and {gone}, then check {}/docs/b.txt.",
            written(&dir)
        );
        let missing = from_request(&request, &dir, &Contract::default());
        assert_eq!(missing.names, ["docs/a.txt", "docs/b.txt", gone.as_str()]);

        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(dir.join("docs/a.txt"), "made").unwrap();
        // Not the path from another computer whose folder is not here.
        assert_eq!(
            unmade(&missing),
            ["`docs/b.txt` does not exist, though the request names it."]
        );

        std::fs::create_dir_all(elsewhere.join("gone")).unwrap();
        assert_eq!(
            unmade(&missing),
            [
                "`docs/b.txt` does not exist, though the request names it.".to_owned(),
                format!("`{gone}` does not exist, though the request names it."),
            ]
        );
    }
}
