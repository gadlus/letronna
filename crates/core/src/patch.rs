//! `apply_patch`: several files added, changed, moved or deleted in one
//! call, in the patch format OpenAI trains its models to write.
//!
//! GPT, the o-series and codex models reach for a V4A patch when they change
//! code, and with edit_file alone they spend a call and an approval on each
//! file. Here a patch is parsed strictly, each hunk is found with edit_file's
//! matcher, and every file's new text is worked out before anything is
//! written: a hunk that does not apply leaves every file as it was. Each path
//! is held to write_file's rules and to the rules on write_file and
//! edit_file, and the whole patch is approved on one card. Writes go through
//! `write::atomic` like the other file tools'; one that fails part way puts
//! back what the patch had already written. A file the patch deletes goes
//! through `write::remove`, which keeps it for a rewind to bring back.

use crate::filestate::{Batch, Mark};
use crate::tools::{Tool, ToolContext};
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};

pub(crate) const APPLY_PATCH: &str = "apply_patch";

const BEGIN: &str = "*** Begin Patch";
const END: &str = "*** End Patch";
const ADD: &str = "*** Add File:";
const DELETE: &str = "*** Delete File:";
const UPDATE: &str = "*** Update File:";
const MOVE: &str = "*** Move to:";
const END_OF_FILE: &str = "*** End of File";

/// One file's part of a patch.
#[derive(Debug, PartialEq)]
enum Action {
    Add {
        path: String,
        text: String,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        to: Option<String>,
        hunks: Vec<Hunk>,
    },
}

/// One `@@` section of an update.
#[derive(Debug, PartialEq)]
struct Hunk {
    /// Where in the patch it starts, counted from 1, for the errors.
    line: usize,
    /// The `@@` lines' text, each a line of the file to find first.
    anchors: Vec<String>,
    /// Its lines with their ` `, `-` or `+` in front, in order.
    lines: Vec<String>,
    /// Marked `*** End of File`: lines it only adds go at the end.
    end: bool,
}

impl Hunk {
    /// The lines it expects, ` ` and `-`, each ending in a line break.
    fn expected(&self) -> String {
        self.side('+')
    }

    /// The lines it leaves, ` ` and `+`, each ending in a line break.
    fn left(&self) -> String {
        self.side('-')
    }

    fn side(&self, without: char) -> String {
        self.lines
            .iter()
            .filter(|line| !line.starts_with(without))
            .map(|line| format!("{}\n", &line[1..]))
            .collect()
    }
}

pub(crate) struct ApplyPatch;
impl Tool for ApplyPatch {
    fn mutates(&self) -> bool {
        true
    }
    fn name(&self) -> &'static str {
        APPLY_PATCH
    }
    fn description(&self) -> &'static str {
        "Add, delete, update and move files with one patch, approved as one change. The patch runs from `*** Begin Patch` to `*** End Patch`. Each file in it starts with `*** Add File: <path>` followed by its lines, each starting with +; `*** Delete File: <path>`; or `*** Update File: <path>`, optionally followed by `*** Move to: <new path>`, then its hunks. A hunk starts with an `@@` line, which may name the function or class it is in, then lines starting with a space (kept), - (removed) or + (added), with about three kept lines around each change. Paths are relative to the project folder. Update or delete only files you have read. If any hunk does not apply, no file is changed. A patch that applied need not be read back."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "patch": {"type": "string", "description": "The whole patch, from `*** Begin Patch` to `*** End Patch`."}
        }, "required": ["patch"]})
    }
    fn needs_approval(&self) -> bool {
        true
    }
    fn suits(&self, model: &str) -> bool {
        speaks_patches(model)
    }
    fn preview(&self, args: &Value, _: &ToolContext) -> String {
        let patch = patch_arg(args).unwrap_or_default();
        match parse(patch) {
            Ok(actions) => format!("{}\n{}", listing(&actions), diff(&actions)),
            Err(_) => patch.to_owned(),
        }
    }
    fn judge_input(&self, args: &Value, ctx: &ToolContext) -> String {
        format!("Apply this patch:\n{}", self.preview(args, ctx))
    }
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let patch = patch_arg(args).ok_or(
            "`patch` is required: the whole patch, from `*** Begin Patch` to `*** End Patch`.",
        )?;
        let actions = parse(patch)?;
        apply(&actions, ctx)
    }
}

/// Whether `model` is one of OpenAI's trained on this format -- GPT, the
/// o-series or codex -- by its name under any provider's prefix.
fn speaks_patches(model: &str) -> bool {
    let name = model.rsplit('/').next().unwrap_or(model);
    let name = name.trim().to_ascii_lowercase();
    let o_series = name.strip_prefix('o').is_some_and(|rest| {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        digits > 0 && (rest.len() == digits || rest[digits..].starts_with('-'))
    });
    name.starts_with("gpt-") || name.contains("codex") || o_series
}

/// The patch text of a call. `input` is the name OpenAI's own tool gives it,
/// which a model trained on that tool sends first.
fn patch_arg(args: &Value) -> Option<&str> {
    args["patch"].as_str().or(args["input"].as_str())
}

/// Every path the patch in `args` adds, changes, moves or deletes, as it
/// names them; `None` when it does not parse, and then nothing is written.
pub(crate) fn paths(args: &Value) -> Option<Vec<String>> {
    let actions = parse(patch_arg(args)?).ok()?;
    let mut paths = Vec::new();
    for action in &actions {
        match action {
            Action::Add { path, .. } | Action::Delete { path } => paths.push(path.clone()),
            Action::Update { path, to, .. } => {
                paths.push(path.clone());
                paths.extend(to.clone());
            }
        }
    }
    Some(paths)
}

/// The paths the patch in `args` deletes or moves away, as it names them;
/// none when it does not parse, and then nothing is written.
pub(crate) fn removes(args: &Value) -> Vec<String> {
    let Some(Ok(actions)) = patch_arg(args).map(parse) else {
        return Vec::new();
    };
    actions
        .into_iter()
        .filter_map(|action| match action {
            Action::Delete { path }
            | Action::Update {
                path, to: Some(_), ..
            } => Some(path),
            _ => None,
        })
        .collect()
}

/// The paths the patch in `args` leaves a file at, as it names them: what
/// it adds or updates, a moved file under its new name; `None` when it does
/// not parse.
pub(crate) fn written(args: &Value) -> Option<Vec<String>> {
    let actions = parse(patch_arg(args)?).ok()?;
    let left = actions.into_iter().filter_map(|action| match action {
        Action::Add { path, .. } => Some(path),
        Action::Update { path, to, .. } => Some(to.unwrap_or(path)),
        Action::Delete { .. } => None,
    });
    Some(left.collect())
}

/// The patch's files and hunks, or an error naming the line at fault.
fn parse(patch: &str) -> Result<Vec<Action>, String> {
    let lines: Vec<(usize, &str)> = patch
        .lines()
        .enumerate()
        .map(|(i, line)| (i + 1, line))
        .collect();
    let first = lines.iter().position(|(_, line)| !line.trim().is_empty());
    let last = lines.iter().rposition(|(_, line)| !line.trim().is_empty());
    let (Some(first), Some(last)) = (first, last) else {
        return Err(format!("The patch is empty. It starts with `{BEGIN}`."));
    };
    let (n, opening) = lines[first];
    if opening.trim() != BEGIN {
        return Err(format!(
            "Line {n}: a patch starts with `{BEGIN}`, not `{}`.",
            opening.trim()
        ));
    }
    if last == first || lines[last].1.trim() != END {
        return Err(format!(
            "The patch does not end with `{END}`, so it may have been cut off. Nothing was \
             changed. Send the whole patch again."
        ));
    }
    let body = &lines[first + 1..last];
    let mut actions = Vec::new();
    let mut i = 0;
    while i < body.len() {
        let (n, line) = body[i];
        i += 1;
        if let Some(path) = header(line, ADD, n)? {
            let mut text = String::new();
            while let Some(&(n, line)) = body.get(i).filter(|(_, line)| !starts_file(line)) {
                if blank_to_end(body, i) {
                    i += 1;
                    continue;
                }
                let Some(added) = line.strip_prefix('+') else {
                    return Err(format!(
                        "Line {n}: every line of an added file starts with `+`, and \
                         `{line}` does not."
                    ));
                };
                text.push_str(added);
                text.push('\n');
                i += 1;
            }
            actions.push(Action::Add { path, text });
        } else if let Some(path) = header(line, DELETE, n)? {
            actions.push(Action::Delete { path });
        } else if let Some(path) = header(line, UPDATE, n)? {
            let mut to = None;
            if let Some(&(n, line)) = body.get(i) {
                to = header(line, MOVE, n)?;
                if to.is_some() {
                    i += 1;
                }
            }
            let (hunks, next) = hunks(body, i)?;
            i = next;
            if hunks.is_empty() && to.is_none() {
                return Err(format!(
                    "Line {n}: `{UPDATE} {path}` has no hunks and no `{MOVE}`, so it \
                     changes nothing."
                ));
            }
            actions.push(Action::Update { path, to, hunks });
        } else if line.trim().is_empty() {
            continue;
        } else {
            return Err(format!(
                "Line {n}: expected `{ADD}`, `{DELETE}` or `{UPDATE}`, found `{line}`."
            ));
        }
    }
    if actions.is_empty() {
        return Err("The patch changes no files.".into());
    }
    Ok(actions)
}

/// The path after `marker` when `line` starts with it, or an error when it
/// names none.
fn header(line: &str, marker: &str, n: usize) -> Result<Option<String>, String> {
    let Some(path) = line.strip_prefix(marker) else {
        return Ok(None);
    };
    let path = path.trim();
    if path.is_empty() {
        return Err(format!("Line {n}: `{marker}` names no file."));
    }
    Ok(Some(path.to_owned()))
}

/// Whether `line` starts the next file of the patch.
fn starts_file(line: &str) -> bool {
    [ADD, DELETE, UPDATE]
        .iter()
        .any(|marker| line.starts_with(marker))
}

/// Whether `body[i]` and every line after it up to the next file are blank,
/// which a model leaves between files or before the end without meaning a
/// line of the file.
fn blank_to_end(body: &[(usize, &str)], i: usize) -> bool {
    body[i..]
        .iter()
        .take_while(|(_, line)| !starts_file(line))
        .all(|(_, line)| line.trim().is_empty())
}

/// The hunks of an update starting at `body[i]`, and where the next file
/// starts.
fn hunks(body: &[(usize, &str)], mut i: usize) -> Result<(Vec<Hunk>, usize), String> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut open: Option<Hunk> = None;
    while let Some(&(n, line)) = body.get(i).filter(|(_, line)| !starts_file(line)) {
        if blank_to_end(body, i) {
            i += 1;
            continue;
        }
        i += 1;
        if let Some(anchor) = line.strip_prefix("@@") {
            // Two `@@` lines in a row narrow one place down, class then method.
            let mut hunk = match open.take() {
                Some(hunk) if hunk.lines.is_empty() => hunk,
                done => {
                    hunks.extend(done);
                    Hunk {
                        line: n,
                        anchors: Vec::new(),
                        lines: Vec::new(),
                        end: false,
                    }
                }
            };
            let anchor = anchor.trim().trim_end_matches("@@").trim();
            // A unified diff's `@@ -12,3 +12,4 @@` counts lines rather than
            // naming one; the hunk's own lines find its place.
            let counts = anchor.starts_with('-') && anchor.contains('+');
            if !anchor.is_empty() && !counts {
                hunk.anchors.push(anchor.to_owned());
            }
            open = Some(hunk);
            continue;
        }
        if line.trim_end() == END_OF_FILE {
            let Some(mut hunk) = open.take().filter(|hunk| !hunk.lines.is_empty()) else {
                return Err(format!("Line {n}: `{END_OF_FILE}` ends no hunk."));
            };
            hunk.end = true;
            hunks.push(hunk);
            continue;
        }
        // A kept blank line often loses its space on the way.
        let line = if line.is_empty() { " " } else { line };
        if !line.starts_with([' ', '-', '+']) {
            return Err(format!(
                "Line {n}: a hunk's lines start with a space, `-` or `+`, or are an `@@` \
                 line, and `{line}` is none of those."
            ));
        }
        open.get_or_insert_with(|| Hunk {
            line: n,
            anchors: Vec::new(),
            lines: Vec::new(),
            end: false,
        })
        .lines
        .push(line.to_owned());
    }
    if let Some(hunk) = open {
        if hunk.lines.is_empty() {
            return Err(format!("Line {}: the `@@` hunk has no lines.", hunk.line));
        }
        hunks.push(hunk);
    }
    Ok((hunks, i))
}

/// A line for each file saying what the patch does to it, which the card
/// shows first: the terminal shows only a card's first lines, and a delete
/// at the end of a long diff would go by unseen.
fn listing(actions: &[Action]) -> String {
    actions
        .iter()
        .map(|action| match action {
            Action::Add { path, .. } => format!("Add {path}\n"),
            Action::Delete { path } => format!("Delete {path}\n"),
            Action::Update {
                path, to: Some(to), ..
            } => format!("Move {path} to {to}\n"),
            Action::Update { path, .. } => format!("Update {path}\n"),
        })
        .collect()
}

/// The patch as the card shows it: each file's diff, one after another.
fn diff(actions: &[Action]) -> String {
    let mut out = String::new();
    for action in actions {
        match action {
            Action::Add { path, text } => {
                out.push_str(&format!("--- /dev/null\n+++ {path}\n"));
                for line in text.lines() {
                    out.push_str(&format!("+{line}\n"));
                }
            }
            Action::Delete { path } => out.push_str(&format!("--- {path}\n+++ /dev/null\n")),
            Action::Update { path, to, hunks } => {
                let to = to.as_deref().unwrap_or(path);
                out.push_str(&format!("--- {path}\n+++ {to}\n"));
                for hunk in hunks {
                    out.push_str(&format!("@@ {}\n", hunk.anchors.join(" ")));
                    for line in &hunk.lines {
                        out.push_str(line);
                        out.push('\n');
                    }
                }
            }
        }
    }
    out
}

/// What the patch leaves at one path, worked out before anything is written.
enum Step {
    /// `after` goes to `path`, which held `before` or nothing.
    Write {
        path: PathBuf,
        raw: String,
        before: Option<String>,
        after: String,
        mark: Mark,
    },
    /// `path`, which held `before`, goes.
    Remove {
        path: PathBuf,
        raw: String,
        before: String,
        mark: Mark,
    },
}

/// Applies `actions` in `ctx`'s project: every hunk is found and every file
/// worked out first, and only then is anything written.
fn apply(actions: &[Action], ctx: &ToolContext) -> Result<String, String> {
    let mut batch = crate::filestate::batch(ctx.conversation);
    let mut steps = Vec::new();
    let mut claimed = Vec::new();
    let mut said = Vec::new();
    let mut notes = String::new();
    // Only these are the session's own: a file moved is still the user's.
    let mut added = Vec::new();
    for action in actions {
        match action {
            Action::Add { path: raw, text } => {
                let path = target(raw, ctx, &mut claimed)?;
                if path.symlink_metadata().is_ok() {
                    return Err(format!(
                        "`{raw}` already exists, so the patch cannot add it; update it \
                         instead. Nothing was changed."
                    ));
                }
                crate::write::check_syntax(&path, None, text)?;
                notes.push_str(&crate::write::csv_note(Path::new(raw), None, text));
                notes.push_str(&crate::write::syntax_note(Path::new(raw), None, text));
                added.push(path.clone());
                steps.push(Step::Write {
                    path,
                    raw: raw.clone(),
                    before: None,
                    after: text.clone(),
                    mark: batch.fresh(),
                });
                said.push(format!("added {raw}"));
            }
            Action::Delete { path: raw } => {
                let path = target(raw, ctx, &mut claimed)?;
                writable(&path, raw)?;
                let (before, mark) = batch.open(&path, raw).map_err(|e| missing(&path, raw, e))?;
                steps.push(Step::Remove {
                    path,
                    raw: raw.clone(),
                    before,
                    mark,
                });
                said.push(format!("deleted {raw}"));
            }
            Action::Update {
                path: raw,
                to,
                hunks,
            } => {
                let path = target(raw, ctx, &mut claimed)?;
                writable(&path, raw)?;
                let (before, mark) = batch.open(&path, raw).map_err(|e| missing(&path, raw, e))?;
                let after = patched(&before, hunks, raw)?;
                let Some(to) = to else {
                    crate::write::check_syntax(&path, Some(before.as_bytes()), &after)?;
                    notes.push_str(&crate::write::csv_note(
                        Path::new(raw),
                        Some(&before),
                        &after,
                    ));
                    notes.push_str(&crate::write::syntax_note(
                        Path::new(raw),
                        Some(&before),
                        &after,
                    ));
                    steps.push(Step::Write {
                        path,
                        raw: raw.clone(),
                        before: Some(before),
                        after,
                        mark,
                    });
                    said.push(format!("updated {raw}"));
                    continue;
                };
                let moved = target(to, ctx, &mut claimed)?;
                if moved.symlink_metadata().is_ok() {
                    return Err(format!(
                        "`{to}` already exists, so the patch cannot move `{raw}` there. \
                         Nothing was changed."
                    ));
                }
                crate::write::check_syntax(&moved, Some(before.as_bytes()), &after)?;
                notes.push_str(&crate::write::csv_note(
                    Path::new(to),
                    Some(&before),
                    &after,
                ));
                notes.push_str(&crate::write::syntax_note(
                    Path::new(to),
                    Some(&before),
                    &after,
                ));
                steps.push(Step::Write {
                    path: moved,
                    raw: to.clone(),
                    before: None,
                    after,
                    mark,
                });
                steps.push(Step::Remove {
                    path,
                    raw: raw.clone(),
                    before,
                    mark,
                });
                said.push(format!("moved {raw} to {to}"));
            }
        }
    }
    let mut done: Vec<&Step> = Vec::new();
    for step in &steps {
        let result = match step {
            Step::Write {
                path,
                raw,
                after,
                mark,
                ..
            } => path
                .parent()
                .map_or(Ok(()), |folder| crate::taint::create_folders(ctx, folder))
                .map_err(|e| format!("Could not make the folder for `{raw}`: {e}."))
                .and_then(|()| batch.write(path, after.as_bytes(), *mark, &ctx.files)),
            Step::Remove { path, raw, .. } => batch.remove(path, raw, &ctx.files),
        };
        if let Err(e) = result {
            return Err(format!("{e}{}", undo(&done, &mut batch)));
        }
        done.push(step);
    }
    for path in &added {
        crate::taint::made(ctx, path);
    }
    Ok(format!("Applied the patch: {}.{notes}", said.join(", ")))
}

/// Where `raw` may be written, as write_file resolves it, refused when the
/// patch already names that file.
fn target(raw: &str, ctx: &ToolContext, claimed: &mut Vec<PathBuf>) -> Result<PathBuf, String> {
    let path = crate::tools::resolve_write(raw, ctx)?;
    // `./a.rs` and `a.rs` are one file; so are two spellings of one that exists.
    let same: PathBuf = path
        .components()
        .filter(|part| *part != Component::CurDir)
        .collect();
    let same = std::fs::canonicalize(&same).unwrap_or(same);
    if claimed.contains(&same) {
        return Err(format!(
            "The patch names `{raw}` more than once; put all its changes under one \
             `{UPDATE}`. Nothing was changed."
        ));
    }
    claimed.push(same);
    Ok(path)
}

/// Refuses a read-only file, which write_file and edit_file leave alone too,
/// before anything is written: deleting one only needs its folder writable.
fn writable(path: &Path, raw: &str) -> Result<(), String> {
    if std::fs::metadata(path).is_ok_and(|meta| meta.permissions().readonly()) {
        return Err(format!(
            "`{raw}` is read-only, so the patch cannot change, move or delete it. \
             Nothing was changed."
        ));
    }
    Ok(())
}

/// Says a file the patch updates or deletes is not there, before the error
/// `why` from opening it, which reads as the system's.
fn missing(path: &Path, raw: &str, why: String) -> String {
    if path.symlink_metadata().is_err() {
        return format!(
            "`{raw}` does not exist, so the patch cannot change it. Nothing was changed."
        );
    }
    why
}

/// `file` with the hunks made in order, each found after the one before, or
/// an error naming the first that does not apply.
fn patched(file: &str, hunks: &[Hunk], raw: &str) -> Result<String, String> {
    // The mark stays out of the search, where the first line would never
    // start a line, and goes back on afterwards.
    let (bom, body) = match file.strip_prefix('\u{FEFF}') {
        Some(body) => ("\u{FEFF}", body),
        None => ("", file),
    };
    let ending = crate::edit::line_ending(body).unwrap_or("\n");
    // Every line of a hunk ends in a break, so a last line without one would
    // never match; it has one while the hunks are made, and loses it after.
    let open = !body.is_empty() && !body.ends_with('\n');
    let mut text = match open {
        true => format!("{body}{ending}"),
        false => body.to_owned(),
    };
    let breaks = text.matches('\n').count();
    let mut from = 0;
    for (i, hunk) in hunks.iter().enumerate() {
        let fail = |why: &str| {
            // A hint on the lines below stays last, so that nothing changed
            // is read with the error, not after a block of the file.
            let (why, hint) = why.split_at(why.find('\n').unwrap_or(why.len()));
            format!(
                "Hunk {} of {} for `{raw}` (line {} of the patch) does not apply: {why} \
                 Nothing was changed.{hint}",
                i + 1,
                hunks.len(),
                hunk.line
            )
        };
        for anchor in &hunk.anchors {
            from = after_line(&text, from, anchor).ok_or_else(|| {
                fail(&format!(
                    "no line `{anchor}` follows the hunks before it, as its `@@` line says."
                ))
            })?;
        }
        let (old, new) = (hunk.expected(), hunk.left());
        if old.is_empty() {
            // Lines only added go after the `@@` line they name, or at the end.
            let at = match hunk.end || hunk.anchors.is_empty() {
                true => text.len(),
                false => from,
            };
            let new = new.replace('\n', ending);
            text.insert_str(at, &new);
            from = at + new.len();
            continue;
        }
        (text, from) =
            crate::edit::replace_lines(&text, from, breaks, &old, &new).map_err(|e| fail(&e))?;
    }
    if open && let Some(closed) = text.strip_suffix(ending) {
        text.truncate(closed.len());
    }
    Ok(format!("{bom}{text}"))
}

/// Where the line after the first one from byte `from` of `text` that is
/// `anchor` starts; failing that, the first that holds it.
fn after_line(text: &str, from: usize, anchor: &str) -> Option<usize> {
    let anchor = anchor.trim();
    let lines = || {
        text[from..].split_inclusive('\n').scan(from, |at, line| {
            *at += line.len();
            Some((*at, line))
        })
    };
    lines()
        .find(|(_, line)| line.trim() == anchor)
        .or_else(|| lines().find(|(_, line)| line.contains(anchor)))
        .map(|(end, _)| end)
}

/// Puts back what the steps in `done` changed, after a later one failed,
/// each file as the conversation had seen it, and says how that went, to
/// follow the failure.
fn undo(done: &[&Step], batch: &mut Batch) -> String {
    let quiet = crate::history::Log::default();
    let mut left = Vec::new();
    for step in done.iter().rev() {
        let (raw, result) = match step {
            Step::Write {
                path,
                raw,
                before: Some(before),
                mark,
                ..
            }
            | Step::Remove {
                path,
                raw,
                before,
                mark,
            } => (raw, batch.write(path, before.as_bytes(), *mark, &quiet)),
            Step::Write { path, raw, .. } => (raw, batch.remove(path, raw, &quiet)),
        };
        if let Err(e) = result {
            left.push(format!("`{raw}` ({e})"));
        }
    }
    match (done.len(), left.is_empty()) {
        (0, _) => " Nothing was changed.".into(),
        (_, true) => " The files the patch had already changed were put back.".into(),
        (_, false) => format!(
            " The files the patch had already changed were put back, except {}.",
            left.join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::run;

    /// A fresh project folder of its own for one test, and a context in it.
    fn project(name: &str) -> (PathBuf, ToolContext) {
        let dir =
            std::env::temp_dir().join(format!("letronna-patch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        (dir, ctx)
    }

    fn patch(ctx: &ToolContext, text: &str) -> Result<String, String> {
        run(APPLY_PATCH, &json!({ "patch": text }).to_string(), ctx)
    }

    fn read(dir: &Path, name: &str) -> String {
        std::fs::read_to_string(dir.join(name)).unwrap()
    }

    #[test]
    fn a_new_file_is_added_with_its_folders() {
        let (dir, ctx) = project("add");
        let done = patch(
            &ctx,
            "*** Begin Patch\n*** Add File: src/hello.txt\n+hello\n+world\n*** End Patch\n",
        )
        .unwrap();
        assert!(done.contains("added src/hello.txt"), "{done}");
        assert_eq!(read(&dir, "src/hello.txt"), "hello\nworld\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_update_finds_each_hunk_by_its_context_and_keeps_the_rest() {
        let (dir, ctx) = project("update");
        let file = "fn one() {\n    let a = 1;\n}\n\nfn two() {\n    let a = 1;\n}\n";
        std::fs::write(dir.join("lib.rs"), file).unwrap();
        let text = "*** Begin Patch
*** Update File: lib.rs
@@ fn two() {
-    let a = 1;
+    let a = 2;
 }
*** End Patch";
        patch(&ctx, text).unwrap();
        assert_eq!(
            read(&dir, "lib.rs"),
            "fn one() {\n    let a = 1;\n}\n\nfn two() {\n    let a = 2;\n}\n"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn several_hunks_apply_in_order_and_a_last_line_without_a_break_keeps_none() {
        let (dir, ctx) = project("hunks");
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\nfour\nfive").unwrap();
        let text = "*** Begin Patch
*** Update File: a.txt
@@
 one
-two
+2
@@
 four
-five
+5
*** End Patch";
        patch(&ctx, text).unwrap();
        assert_eq!(read(&dir, "a.txt"), "one\n2\nthree\nfour\n5");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_crlf_file_with_a_byte_order_mark_keeps_both() {
        let (dir, ctx) = project("crlf");
        std::fs::write(dir.join("a.txt"), "\u{FEFF}first\r\nsecond\r\nthird\r\n").unwrap();
        let text = "*** Begin Patch
*** Update File: a.txt
@@
 first
-second
+middle
+more
 third
*** End Patch";
        patch(&ctx, text).unwrap();
        assert_eq!(
            read(&dir, "a.txt"),
            "\u{FEFF}first\r\nmiddle\r\nmore\r\nthird\r\n"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_file_is_deleted_and_another_moved_with_its_changes() {
        let (dir, ctx) = project("move");
        std::fs::write(dir.join("gone.txt"), "bye\n").unwrap();
        std::fs::write(dir.join("old.txt"), "keep\nchange\n").unwrap();
        let text = "*** Begin Patch
*** Delete File: gone.txt
*** Update File: old.txt
*** Move to: moved/new.txt
@@
 keep
-change
+changed
*** End Patch";
        let done = patch(&ctx, text).unwrap();
        assert!(done.contains("deleted gone.txt"), "{done}");
        assert!(done.contains("moved old.txt to moved/new.txt"), "{done}");
        assert!(!dir.join("gone.txt").exists());
        assert!(!dir.join("old.txt").exists());
        assert_eq!(read(&dir, "moved/new.txt"), "keep\nchanged\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_csv_the_patch_leaves_uneven_gets_a_note() {
        let (dir, ctx) = project("csv");
        std::fs::write(dir.join("b.csv"), "id,field\n1,\"amount, currency\"\n").unwrap();
        let text = "*** Begin Patch
*** Add File: a.csv
+id,field
+1,\"amount, currency\"
*** Update File: b.csv
@@
 id,field
-1,\"amount, currency\"
+1,amount, currency
*** End Patch";
        assert_eq!(
            patch(&ctx, text).unwrap(),
            "Applied the patch: added a.csv, updated b.csv.\nNote: `b.csv` line 2 has 3 \
             fields where the header has 2; a field that holds a comma needs double quotes \
             around it."
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_hunk_that_does_not_apply_leaves_every_file_untouched() {
        let (dir, ctx) = project("atomic");
        std::fs::write(dir.join("a.txt"), "alpha\n").unwrap();
        std::fs::write(dir.join("b.txt"), "beta\n").unwrap();
        std::fs::write(dir.join("c.txt"), "gamma\n").unwrap();
        let text = "*** Begin Patch
*** Update File: a.txt
-alpha
+ALPHA
*** Add File: new.txt
+new
*** Delete File: c.txt
*** Update File: b.txt
@@
-not in the file
+x
*** End Patch";
        let err = patch(&ctx, text).unwrap_err();
        assert!(
            err.contains("Hunk 1 of 1 for `b.txt` (line 9 of the patch)"),
            "{err}"
        );
        assert!(err.contains("Nothing was changed"), "{err}");
        assert_eq!(read(&dir, "a.txt"), "alpha\n");
        assert_eq!(read(&dir, "b.txt"), "beta\n");
        assert_eq!(read(&dir, "c.txt"), "gamma\n");
        assert!(!dir.join("new.txt").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_apply_patch_hunk_gets_the_same_hint() {
        let (dir, ctx) = project("near-miss");
        let file = "fn one() {}\n\nfn two() {\n    let total = foo_bar(1);\n}\n";
        std::fs::write(dir.join("a.rs"), file).unwrap();
        let text = "*** Begin Patch\n*** Update File: a.rs\n@@\n+// One.\n+// Two.\n fn one() {}\n\
                    @@\n fn two() {\n-    let total = foo-bar(1);\n+    let total = 2;\n*** End Patch";
        let err = patch(&ctx, text).unwrap_err();
        // The first hunk added two lines above; the numbers are the file's.
        assert!(
            err.ends_with(
                "are not in the file after the hunks before it. Nothing was changed.\n\
                 Closest text, lines 3-4 (97% alike):\n3|fn two() {\n4|    let total = foo_bar(1);\n\
                 Differs at 4:20: the file has `foo_bar(1);`, `old` has `foo-bar(1);`."
            ),
            "{err}"
        );
        assert_eq!(read(&dir, "a.rs"), file);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_write_that_fails_part_way_puts_every_file_back_as_it_was_read() {
        let (dir, mut ctx) = project("undo");
        ctx.conversation = 9_102;
        crate::filestate::asked(ctx.conversation, 1, 0, 0);
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        std::fs::write(dir.join("gone.txt"), "bye\n").unwrap();
        // A file where the last step needs a folder: found only when written.
        std::fs::write(dir.join("blocker"), "a file\n").unwrap();
        for name in ["a.txt", "gone.txt"] {
            run("read_file", &json!({ "path": name }).to_string(), &ctx).unwrap();
        }
        crate::filestate::asked(ctx.conversation, 2, 0, 0);
        let text = "*** Begin Patch\n*** Update File: a.txt\n@@\n-a\n+b\n\
                    *** Delete File: gone.txt\n*** Add File: blocker/x.txt\n+x\n*** End Patch";
        let err = patch(&ctx, text).unwrap_err();
        assert!(err.contains("`blocker/x.txt`"), "{err}");
        assert!(err.ends_with("were put back."), "{err}");
        assert_eq!(read(&dir, "a.txt"), "a\n");
        assert_eq!(read(&dir, "gone.txt"), "bye\n");
        // What the model read still stands, so it goes on from there.
        for name in ["a.txt", "gone.txt"] {
            let edit = json!({ "path": name, "old": "\n", "new": "!\n" });
            run("edit_file", &edit.to_string(), &ctx).unwrap();
        }
        crate::filestate::forget(ctx.conversation);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_read_only_file_is_neither_deleted_nor_moved() {
        let (dir, ctx) = project("read-only");
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        std::fs::write(dir.join("locked.txt"), "keep\n").unwrap();
        let locked = dir.join("locked.txt");
        let mut perms = std::fs::metadata(&locked).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&locked, perms.clone()).unwrap();
        for body in [
            "*** Delete File: locked.txt",
            "*** Update File: locked.txt\n*** Move to: free.txt",
            "*** Update File: locked.txt\n@@\n-keep\n+changed",
        ] {
            let text = format!(
                "*** Begin Patch\n*** Update File: a.txt\n@@\n-a\n+b\n{body}\n*** End Patch"
            );
            let err = patch(&ctx, &text).unwrap_err();
            assert!(err.contains("`locked.txt` is read-only"), "{err}");
            assert_eq!(read(&dir, "locked.txt"), "keep\n");
            assert_eq!(read(&dir, "a.txt"), "a\n");
        }
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&locked, perms).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_hunk_matching_in_two_places_or_only_mid_line_is_refused() {
        let (dir, ctx) = project("ambiguous");
        std::fs::write(dir.join("a.txt"), "x = 1\nx = 1\nmax = 2\n").unwrap();
        let twice = "*** Begin Patch\n*** Update File: a.txt\n-x = 1\n+x = 3\n*** End Patch";
        let err = patch(&ctx, twice).unwrap_err();
        assert!(err.contains("2 places"), "{err}");
        let inside = "*** Begin Patch\n*** Update File: a.txt\n-x = 2\n+x = 3\n*** End Patch";
        let err = patch(&ctx, inside).unwrap_err();
        assert!(err.contains("are not in the file"), "{err}");
        assert_eq!(read(&dir, "a.txt"), "x = 1\nx = 1\nmax = 2\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_path_outside_the_project_is_refused_and_nothing_is_written() {
        let (dir, ctx) = project("outside");
        let text = "*** Begin Patch
*** Add File: inside.txt
+fine
*** Add File: ../escaped.txt
+no
*** End Patch";
        let err = patch(&ctx, text).unwrap_err();
        assert!(err.contains("`../escaped.txt`"), "{err}");
        assert!(!dir.join("inside.txt").exists());
        assert!(!dir.parent().unwrap().join("escaped.txt").exists());
        let absolute = std::env::temp_dir().join("letronna-patch-absolute.txt");
        let text = format!(
            "*** Begin Patch\n*** Add File: {}\n+no\n*** End Patch",
            absolute.display()
        );
        let err = patch(&ctx, &text).unwrap_err();
        assert!(err.contains("outside the project folder"), "{err}");
        assert!(!absolute.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bad_patch_syntax_is_refused_naming_the_line() {
        let cases = [
            (
                "*** Add File: a.txt\n+x\n*** End Patch",
                "Line 1: a patch starts with",
            ),
            (
                "*** Begin Patch\n*** Add File: a.txt\n+x",
                "does not end with `*** End Patch`",
            ),
            (
                "*** Begin Patch\n*** Add File: a.txt\nx\n*** End Patch",
                "Line 3: every line of an added file",
            ),
            (
                "*** Begin Patch\n*** Update File: a.txt\n@@\n*x\n*** End Patch",
                "Line 4: a hunk's lines start with",
            ),
            (
                "*** Begin Patch\n*** Change File: a.txt\n*** End Patch",
                "Line 2: expected `*** Add File:`",
            ),
            (
                "*** Begin Patch\n*** Update File: a.txt\n*** End Patch",
                "Line 2: `*** Update File: a.txt` has no hunks",
            ),
            (
                "*** Begin Patch\n*** Delete File:\n*** End Patch",
                "Line 2: `*** Delete File:` names no file",
            ),
            ("*** Begin Patch\n*** End Patch", "changes no files"),
        ];
        for (text, said) in cases {
            let err = parse(text).unwrap_err();
            assert!(err.contains(said), "{text:?}: {err}");
        }
        let (dir, ctx) = project("syntax");
        let err = patch(
            &ctx,
            "*** Begin Patch\n*** Add File: a.txt\nx\n*** End Patch",
        )
        .unwrap_err();
        assert!(err.contains("Line 3"), "{err}");
        assert!(!dir.join("a.txt").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(feature = "syntax")]
    #[test]
    fn a_patch_that_breaks_code_says_so_and_still_writes_it() {
        let (dir, ctx) = project("broken-code");
        std::fs::write(dir.join("lib.rs"), "fn one() {\n    let a = 1;\n}\n").unwrap();
        let text = "*** Begin Patch
*** Add File: src/app.py
+def f(x):
+    return x +
*** Update File: lib.rs
@@ fn one() {
-    let a = 1;
+    let a = (1;
*** End Patch";
        let said = patch(&ctx, text).unwrap();
        assert!(
            said.starts_with(
                "Applied the patch: added src/app.py, updated lib.rs.\nNote: `src/app.py` line \
                 2 does not parse as Python: `    return x +`."
            ),
            "{said}"
        );
        let rust =
            "\nNote: `lib.rs` line 2 no longer parses as Rust, missing `)`: `    let a = (1;`.";
        assert!(said.contains(rust), "{said}");
        assert_eq!(read(&dir, "lib.rs"), "fn one() {\n    let a = (1;\n}\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_file_named_twice_or_added_over_one_that_exists_is_refused() {
        let (dir, ctx) = project("twice");
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        let over = "*** Begin Patch\n*** Add File: a.txt\n+b\n*** End Patch";
        assert!(patch(&ctx, over).unwrap_err().contains("already exists"));
        let twice = "*** Begin Patch\n*** Delete File: a.txt\n*** Update File: ./a.txt\n-a\n+b\n*** End Patch";
        assert!(patch(&ctx, twice).unwrap_err().contains("more than once"));
        let gone = "*** Begin Patch\n*** Delete File: none.txt\n*** End Patch";
        assert!(patch(&ctx, gone).unwrap_err().contains("does not exist"));
        assert_eq!(read(&dir, "a.txt"), "a\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lines_only_added_go_after_their_anchor_or_at_the_end() {
        let (dir, ctx) = project("insert");
        std::fs::write(
            dir.join("a.py"),
            "class A:\n    pass\n\nclass B:\n    pass\n",
        )
        .unwrap();
        let text = "*** Begin Patch
*** Update File: a.py
@@ class A:
+    x = 1
@@
+
+class C:
+    pass
*** End of File
*** End Patch";
        patch(&ctx, text).unwrap();
        assert_eq!(
            read(&dir, "a.py"),
            "class A:\n    x = 1\n    pass\n\nclass B:\n    pass\n\nclass C:\n    pass\n"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_file_the_conversation_has_not_read_is_not_changed() {
        let (dir, mut ctx) = project("unread");
        ctx.conversation = 9_101;
        crate::filestate::asked(ctx.conversation, 1, 0, 0);
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        let text = "*** Begin Patch\n*** Update File: a.txt\n-a\n+b\n*** End Patch";
        let err = patch(&ctx, text).unwrap_err();
        assert!(err.contains("You have not read `a.txt`"), "{err}");
        assert_eq!(read(&dir, "a.txt"), "a\n");
        crate::filestate::forget(ctx.conversation);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_card_names_every_file_and_what_becomes_of_it_before_the_diff() {
        let args = json!({"patch": "*** Begin Patch
*** Add File: new.txt
+hi
*** Delete File: old.txt
*** Update File: a.rs
*** Move to: b.rs
@@ fn main
-a
+b
*** End Patch"});
        assert_eq!(
            ApplyPatch.preview(&args, &ToolContext::default()),
            "Add new.txt\nDelete old.txt\nMove a.rs to b.rs\n\n\
             --- /dev/null\n+++ new.txt\n+hi\n--- old.txt\n+++ /dev/null\n\
             --- a.rs\n+++ b.rs\n@@ fn main\n-a\n+b\n"
        );
        assert_eq!(
            paths(&args).unwrap(),
            ["new.txt", "old.txt", "a.rs", "b.rs"]
        );
        assert_eq!(paths(&json!({"input": "nonsense"})), None);
    }

    #[test]
    fn a_delete_at_the_end_of_a_long_patch_is_among_the_cards_first_lines() {
        let updates =
            ["a", "b", "c"].map(|name| format!("*** Update File: {name}.rs\n@@\n-a\n+b\n"));
        let text = format!(
            "*** Begin Patch\n{}*** Delete File: important.rs\n*** End Patch",
            updates.concat()
        );
        let card = ApplyPatch.preview(&json!({ "patch": text }), &ToolContext::default());
        // The terminal shows a card's first 12 lines.
        let first: Vec<&str> = card.lines().take(12).collect();
        assert!(first.contains(&"Delete important.rs"), "{first:?}");
    }

    #[test]
    fn deny_and_ask_rules_on_writing_files_reach_every_path_of_a_patch() {
        use crate::permissions::{Effect, Rule, check};
        let (dir, ctx) = project("rules");
        let rule = |tool: &str, pattern: &str, effect| Rule {
            tool: tool.into(),
            pattern: pattern.into(),
            effect,
            project: None,
        };
        let args = json!({"patch": "*** Begin Patch
*** Add File: src/a.rs
+a
*** Update File: src/b.rs
*** Move to: locked/b.rs
-b
+c
*** End Patch"});
        let denied = [rule("edit_file", "locked/**", Effect::Deny)];
        assert!(check(&denied, &ApplyPatch, &args, &ctx).deny.is_some());
        let asked = [rule("write_file", "*", Effect::Ask)];
        assert!(check(&asked, &ApplyPatch, &args, &ctx).ask.is_some());
        let elsewhere = [rule("write_file", "docs/**", Effect::Deny)];
        assert!(check(&elsewhere, &ApplyPatch, &args, &ctx).deny.is_none());

        let theirs = [rule("write_file", "*", Effect::Allow)];
        assert!(!check(&theirs, &ApplyPatch, &args, &ctx).allow);
        let part = [rule(APPLY_PATCH, "src/**", Effect::Allow)];
        assert!(!check(&part, &ApplyPatch, &args, &ctx).allow);
        let all = [
            rule(APPLY_PATCH, "src/**", Effect::Allow),
            rule(APPLY_PATCH, "locked/**", Effect::Allow),
        ];
        assert!(check(&all, &ApplyPatch, &args, &ctx).allow);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn only_openai_models_are_offered_the_tool() {
        let tools: [std::sync::Arc<dyn Tool>; 1] = [std::sync::Arc::new(ApplyPatch)];
        let offered = |model: &str| {
            let connection = crate::chat::Connection {
                model: model.into(),
                ..Default::default()
            };
            !crate::tools::specs_of(&tools, &connection, Default::default()).is_empty()
        };
        assert!(offered("gpt-4.1"));
        assert!(!offered("claude-sonnet-4-5"));
        for model in [
            "gpt-4.1",
            "gpt-5-mini",
            "openai/gpt-4o",
            "o1",
            "o3-mini",
            "o4-mini-high",
            "codex-mini-latest",
            "gpt-5-codex",
        ] {
            assert!(speaks_patches(model), "{model}");
        }
        for model in [
            "claude-sonnet-4-5",
            "anthropic/claude-opus-4",
            "gemini-2.5-pro",
            "llama-3.3-70b",
            "olmo-2",
            "o",
            "",
        ] {
            assert!(!speaks_patches(model), "{model}");
        }
    }
}
