//! A session's shared task list: tasks its agents add, claim and finish, with
//! owners and blockers, which the session's idle kept helpers are offered.
//!
//! The list is `tasklists/<root session>.jsonl`, one whole task per line as it
//! is after each change, folded on load so the latest line of each task wins.
//! Changes are appended, so a writer that dies part way tears one line and
//! loses one change, never the list; past COMPACT_AT lines the list is
//! written afresh and renamed over the old, keeping every task not finished
//! and the latest finished ones -- the highest id among them, so no id is
//! reused. The window and the terminal can both run the session's agents, so
//! every change holds this process's turn and an OS lock on
//! `<root session>.lock` beside the list, and is made against the list as it
//! is on disk then. The OS lets a dead holder's lock go, so none is ever taken
//! over, and a change that cannot get it in time is refused, never made
//! without it. That makes a claim a check-and-set: it fails when the task was
//! taken, or has changed at all since the claimer read it. Owners are helper
//! names, and only this process's helpers can be reached, so an owner not
//! running here is shown as such and holds nothing against the main agent.
//! Whoever a change concerns hears of it -- the main agent only of a task
//! finished, held or given back -- and the session's idle kept helpers are
//! rung with each task that can be taken, after every change and whenever one
//! of them goes idle.

use crate::agents;
use crate::inbox::{self, Notice, When};
use crate::tools::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// What the session's own agent is called, as a helper reaches it.
const MAIN: &str = "main";
/// Where every notice of a change comes from, so the notices waiting merge.
const FROM: &str = "tasks";
/// Far more than a session's helpers get through at once, so only a runaway
/// loop of adds is refused; finished and dropped tasks do not count.
const MAX_TASKS: usize = 200;
/// A title is one line of the list.
const MAX_TITLE: usize = 200;
/// A notice's 4,000 characters less the title and the words around it, so a
/// task handed to a helper arrives whole.
const MAX_DETAIL: usize = 3_500;
/// How much of each task's detail the list shows; a claim shows it all.
const LISTED_DETAIL: usize = 200;
/// Lines past which the list is written afresh. A change appends a whole
/// task, at most ~4 KB, so the file stays under ~4 MB.
const COMPACT_AT: usize = 1_000;
/// Finished tasks the list keeps when written afresh, so list still shows
/// what was done lately. With MAX_TASKS unfinished that is 250 lines at most,
/// leaving 750 changes before the next time.
const KEEP_FINISHED: usize = 50;
/// Times a task may be given back before it is held for the main agent: one
/// that helpers keep giving up on needs a decision, not another helper.
const MAX_GIVEN_BACK: u32 = 3;
/// How long a change waits for another to finish; a change of even a 40 MB
/// list takes tens of milliseconds, so waiting longer means something hangs.
const WAIT: Duration = Duration::from_secs(3);
/// How long a change sleeps between tries at the lock.
const RETRY: Duration = Duration::from_millis(10);
/// What a change that could not get the lock in time says.
const BUSY: &str = "The task list is busy; try again.";

/// This process's changes, one at a time, so a thread never spends its wait
/// on a lock another thread here holds.
static TURN: Mutex<()> = Mutex::new(());

/// Where a task has got to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Nobody works on it.
    Open,
    /// Its owner works on it.
    Claimed,
    Done,
    /// Held until someone unblocks it, whatever its blockers say.
    Blocked,
    /// Given up by the main agent: never offered, and no longer waited for.
    Dropped,
}

impl Status {
    fn word(self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::Claimed => "claimed",
            Status::Done => "done",
            Status::Blocked => "blocked",
            Status::Dropped => "dropped",
        }
    }

    /// Whether nothing more is to be done on it: done or dropped.
    fn closed(self) -> bool {
        matches!(self, Status::Done | Status::Dropped)
    }
}

/// One task on the list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: u64,
    /// One more with every change, which a claim checks what it read against.
    pub rev: u64,
    pub title: String,
    #[serde(default)]
    pub detail: String,
    pub status: Status,
    /// Who works on it: "main" for the session's own agent, or a helper's name.
    #[serde(default)]
    pub owner: Option<String>,
    /// The tasks that must be done before this one can be taken.
    #[serde(default)]
    pub blocked_by: Vec<u64>,
    /// Who added it: "main" or a helper's name.
    #[serde(default)]
    pub by: String,
    /// How many times a helper has given it back or ended holding it.
    #[serde(default)]
    pub given_back: u32,
}

/// A task as a change left it, with how it was before, if it was.
struct Changed {
    task: Task,
    before: Option<Task>,
}

/// Where session `root`'s list is kept.
fn path(root: u64) -> PathBuf {
    crate::chat::data_dir()
        .join("tasklists")
        .join(format!("{root}.jsonl"))
}

/// The file whose OS lock every change to session `root`'s list holds; it is
/// never removed while the session is there, so all its writers lock one file.
fn lock_path(root: u64) -> PathBuf {
    path(root).with_extension("lock")
}

/// Session `root`'s tasks as they are now, by id.
pub fn load(root: u64) -> Vec<Task> {
    fold(&std::fs::read_to_string(path(root)).unwrap_or_default())
}

/// The tasks `lines` leave, the latest line of each winning. A line that
/// does not read, such as one a dying writer tore, is skipped.
fn fold(lines: &str) -> Vec<Task> {
    let mut tasks = BTreeMap::new();
    for task in lines
        .lines()
        .filter_map(|line| serde_json::from_str::<Task>(line).ok())
    {
        tasks.insert(task.id, task);
    }
    tasks.into_values().collect()
}

/// This process's turn and the OS lock on a list, held for one change. The
/// file is let go first, so whoever takes the turn next finds it free.
struct Held {
    _file: File,
    _turn: MutexGuard<'static, ()>,
}

/// Holds session `root`'s list for one change, or says it is busy when
/// another holder has not let it go within WAIT. Nothing is taken over: the
/// OS lets the lock of a holder that died go by itself.
fn hold(root: u64) -> Result<Held, String> {
    let turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path(root))
        .map_err(|e| format!("Could not open the task list's lock: {e}"))?;
    let start = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => {
                return Ok(Held {
                    _file: file,
                    _turn: turn,
                });
            }
            Err(TryLockError::WouldBlock) if start.elapsed() < WAIT => std::thread::sleep(RETRY),
            Err(TryLockError::WouldBlock) => return Err(BUSY.into()),
            Err(TryLockError::Error(e)) => {
                return Err(format!("Could not lock the task list: {e}"));
            }
        }
    }
}

/// Makes one change to session `root`'s list under its lock: `edit` is handed
/// the list as it is on disk now and gives back each task it changes, as it
/// is to be, and those are written one revision on.
fn change(
    root: u64,
    edit: impl FnOnce(&[Task]) -> Result<Vec<Task>, String>,
) -> Result<Vec<Changed>, String> {
    let path = path(root);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("Could not make the task list's folder: {e}"))?;
    }
    let _held = hold(root)?;
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let tasks = fold(&text);
    let mut changed = Vec::new();
    for mut task in edit(&tasks)? {
        let before = tasks.iter().find(|old| old.id == task.id).cloned();
        task.rev = before.as_ref().map_or(1, |old| old.rev + 1);
        changed.push(Changed { task, before });
    }
    if changed.is_empty() {
        return Ok(changed);
    }
    if text.lines().count() + changed.len() <= COMPACT_AT {
        append(&path, &text, &changed)?;
        return Ok(changed);
    }
    let mut all: BTreeMap<u64, Task> = (tasks.into_iter()).map(|task| (task.id, task)).collect();
    all.extend((changed.iter()).map(|changed| (changed.task.id, changed.task.clone())));
    rewrite(&path, &compacted(all.into_values().collect()))?;
    Ok(changed)
}

/// `tasks` as lines of the list.
fn lines<'a>(tasks: impl Iterator<Item = &'a Task>) -> Result<String, String> {
    let mut lines = String::new();
    for task in tasks {
        lines.push_str(&serde_json::to_string(task).map_err(|e| e.to_string())?);
        lines.push('\n');
    }
    Ok(lines)
}

/// Adds `changed` to the list at `path`, which reads `text`, in one write, so
/// a change of several tasks lands whole or tears one line.
fn append(path: &Path, text: &str, changed: &[Changed]) -> Result<(), String> {
    let mut out = String::new();
    // A line torn part way would swallow the first one written after it.
    if !text.is_empty() && !text.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&lines(changed.iter().map(|changed| &changed.task))?);
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(out.as_bytes()))
        .map_err(|e| format!("Could not write the task list: {e}"))
}

/// Writes `tasks` as the whole list at `path`: beside it first, then renamed
/// over it, so a reader sees the old list or the new one, whole.
fn rewrite(path: &Path, tasks: &[Task]) -> Result<(), String> {
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, lines(tasks.iter())?)
        .and_then(|()| std::fs::rename(&temp, path))
        .map_err(|e| format!("Could not write the task list afresh: {e}"))
}

/// `tasks`, in id order, less all but the KEEP_FINISHED latest finished ones.
/// The highest id stays among them, so the next task is never given an id
/// that was used.
fn compacted(tasks: Vec<Task>) -> Vec<Task> {
    let finished = tasks.iter().filter(|task| task.status.closed()).count();
    let mut surplus = finished.saturating_sub(KEEP_FINISHED);
    (tasks.into_iter())
        .filter(|task| {
            if !task.status.closed() || surplus == 0 {
                return true;
            }
            surplus -= 1;
            false
        })
        .collect()
}

/// Removes session `root`'s list, as deleting the session does, so a session
/// given its id later starts with none.
pub fn forget(root: u64) {
    // Held so no change lands part way; a busy list goes all the same.
    let held = hold(root);
    let _ = std::fs::remove_file(path(root));
    let _ = std::fs::remove_file(lock_path(root));
    drop(held);
}

/// Task `id` of `tasks`, or why there is none.
fn find(tasks: &[Task], id: u64) -> Result<&Task, String> {
    (tasks.iter().find(|task| task.id == id))
        .ok_or_else(|| format!("There is no task #{id}; list shows the tasks there are."))
}

/// The tasks `task` waits for that are neither done nor dropped yet.
fn waiting(task: &Task, tasks: &[Task]) -> Vec<u64> {
    (task.blocked_by.iter().copied())
        .filter(|id| tasks.iter().any(|t| t.id == *id && !t.status.closed()))
        .collect()
}

/// Whether `task` can be taken: open, with every task it waits for done.
fn claimable(task: &Task, tasks: &[Task]) -> bool {
    task.status == Status::Open && task.owner.is_none() && waiting(task, tasks).is_empty()
}

/// Whether `task` was given back so often that it waits for the main agent.
fn needs_decision(task: &Task) -> bool {
    task.status == Status::Blocked && task.given_back > MAX_GIVEN_BACK
}

/// `ids` as the list writes them: "#1, #2".
fn numbers(ids: &[u64]) -> String {
    let ids: Vec<String> = ids.iter().map(|id| format!("#{id}")).collect();
    ids.join(", ")
}

/// Whether `owner` can be reached here: the main agent, or one of `here`,
/// the session's helpers that run or wait idle in this process.
fn running(owner: &str, here: &[String]) -> bool {
    owner == MAIN || here.iter().any(|name| name == owner)
}

/// `owner` as the list shows it, saying so when it is not running here.
fn holder(owner: &str, here: &[String]) -> String {
    if running(owner, here) {
        owner.to_owned()
    } else {
        format!("{owner} (not running)")
    }
}

/// Refuses a task `by` may not change: one someone else owns, unless `by`
/// is the session's own agent.
fn may_change(task: &Task, by: &str) -> Result<(), String> {
    match &task.owner {
        Some(owner) if by != MAIN && owner != by => Err(format!(
            "Task #{} is {owner}'s; only {owner} or the main agent may change it.",
            task.id
        )),
        _ => Ok(()),
    }
}

/// Who `owner` names when `by` gives it: None leaves the task to nobody, and
/// "me" is `by`. Only the session's own agent gives a task to someone else,
/// and only to one of `here`, since no other would hear of it.
fn owner_of(owner: &str, by: &str, here: &[String]) -> Result<Option<String>, String> {
    let owner = match owner.trim() {
        "" => return Ok(None),
        "me" => by,
        name => name,
    };
    if owner == by {
        return Ok(Some(owner.to_owned()));
    }
    if by != MAIN {
        return Err(
            "Only the main agent gives a task to someone else: claim it for \
                    yourself, or leave the owner empty to give it up."
                .into(),
        );
    }
    if !running(owner, here) {
        return Err(format!(
            "No helper of this session called \"{owner}\" runs here; the agents tool's \
             list shows the ones that do."
        ));
    }
    Ok(Some(owner.to_owned()))
}

/// `task` with `owner` working on it, or left to nobody, its status moved
/// between open and claimed to match.
fn owned(task: Task, owner: Option<String>) -> Task {
    let status = match (task.status, owner.is_some()) {
        (Status::Open, true) => Status::Claimed,
        (Status::Claimed, false) => Status::Open,
        (status, _) => status,
    };
    Task {
        owner,
        status,
        ..task
    }
}

/// `task` given back by the helper that held it: left to nobody, and held
/// for the main agent once it has been given back more than MAX_GIVEN_BACK
/// times, rather than offered round the helpers again.
fn given_back(task: Task) -> Task {
    let task = owned(task, None);
    let given_back = task.given_back + 1;
    let status = match task.status {
        Status::Open if given_back > MAX_GIVEN_BACK => Status::Blocked,
        status => status,
    };
    Task {
        status,
        given_back,
        ..task
    }
}

/// Refuses making task `id` wait for `ids`: each must be another task that
/// does not already wait, however indirectly, for `id`.
fn check_blockers(tasks: &[Task], id: u64, ids: &[u64]) -> Result<(), String> {
    for &blocker in ids {
        if blocker == id {
            return Err(format!("Task #{id} cannot wait for itself."));
        }
        find(tasks, blocker)?;
        if waits_for(tasks, blocker, id) {
            return Err(format!(
                "Task #{blocker} already waits for #{id}, so #{id} cannot wait for it."
            ));
        }
    }
    Ok(())
}

/// Whether task `from` waits for task `on`, directly or through others.
fn waits_for(tasks: &[Task], from: u64, on: u64) -> bool {
    let mut seen = Vec::new();
    let mut next = vec![from];
    while let Some(id) = next.pop() {
        if id == on {
            return true;
        }
        if seen.contains(&id) {
            continue;
        }
        seen.push(id);
        if let Some(task) = tasks.iter().find(|task| task.id == id) {
            next.extend(&task.blocked_by);
        }
    }
    false
}

/// The title `args` gives, if any, as one line of the list.
fn title(args: &Value) -> Result<Option<String>, String> {
    let Some(title) = args["title"].as_str().map(str::trim) else {
        return Ok(None);
    };
    // A carriage return or a line separator would forge a row of the list.
    let breaks = (title.chars()).any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'));
    if title.is_empty() || breaks || title.chars().count() > MAX_TITLE {
        return Err(format!(
            "A title is one line of up to {MAX_TITLE} characters with no control \
             characters; the rest goes in `detail`."
        ));
    }
    Ok(Some(title.to_owned()))
}

/// The detail `args` gives, if any.
fn detail(args: &Value) -> Result<Option<String>, String> {
    let Some(detail) = args["detail"].as_str().map(str::trim) else {
        return Ok(None);
    };
    if detail.chars().count() > MAX_DETAIL {
        return Err(format!(
            "A task's detail is at most {MAX_DETAIL} characters; point to a file for more."
        ));
    }
    Ok(Some(detail.to_owned()))
}

/// The task numbers `args` gives in `blocked_by`, each once, as 5, "5" or
/// "#5"; anything else is refused rather than skipped, since a task that
/// silently waits for nothing would be offered before its prerequisite.
fn blockers(args: &Value) -> Result<Vec<u64>, String> {
    let given = match &args["blocked_by"] {
        Value::Null => Vec::new(),
        Value::Array(given) => given.iter().collect(),
        one => vec![one],
    };
    let mut ids = Vec::new();
    for entry in given {
        let id = match entry {
            Value::String(text) => text.trim().trim_start_matches('#').parse().ok(),
            number => number.as_u64(),
        };
        let Some(id) = id else {
            return Err(format!(
                "{entry} in `blocked_by` is not a task number; give the numbers list shows."
            ));
        };
        ids.push(id);
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

/// A new task from `args`, numbered one past the highest ever written.
fn added(tasks: &[Task], by: &str, here: &[String], args: &Value) -> Result<Task, String> {
    let unfinished = tasks.iter().filter(|task| !task.status.closed()).count();
    if unfinished >= MAX_TASKS {
        return Err(format!(
            "The task list holds {MAX_TASKS} tasks not finished, the most it keeps; \
             finish or drop some first."
        ));
    }
    let Some(title) = title(args)? else {
        return Err("`title` is required: what the task is, in a line.".into());
    };
    // The fold keeps tasks in id order.
    let id = match tasks.last() {
        None => 1,
        Some(last) => (last.id.checked_add(1)).ok_or("The task list has run out of numbers.")?,
    };
    let blocked_by = blockers(args)?;
    check_blockers(tasks, id, &blocked_by)?;
    let owner = owner_of(args["owner"].as_str().unwrap_or_default(), by, here)?;
    let task = Task {
        id,
        rev: 0,
        title,
        detail: detail(args)?.unwrap_or_default(),
        status: Status::Open,
        owner: None,
        blocked_by,
        by: by.to_owned(),
        given_back: 0,
    };
    Ok(owned(task, owner))
}

/// Task `id` taken by `who`, if it can be: open, with every task it waits
/// for done, and, when `seen` is given, at the revision the claimer read. An
/// owner not running here keeps it from helpers but not from the main agent.
fn claimed(
    tasks: &[Task],
    id: u64,
    who: &str,
    seen: Option<u64>,
    here: &[String],
) -> Result<Task, String> {
    let task = find(tasks, id)?;
    if task.status.closed() {
        return Err(format!("Task #{id} is {} already.", task.status.word()));
    }
    if let Some(owner) = &task.owner
        && (who != MAIN || running(owner, here))
    {
        let owner = if owner == who {
            "you".to_owned()
        } else {
            holder(owner, here)
        };
        return Err(format!("Task #{id} is already claimed by {owner}."));
    }
    if needs_decision(task) && who != MAIN {
        return Err(format!(
            "Task #{id} was given back too often and waits for the main agent to decide."
        ));
    }
    if task.status == Status::Blocked {
        return Err(format!("Task #{id} is blocked until someone unblocks it."));
    }
    let waiting = waiting(task, tasks);
    if !waiting.is_empty() {
        return Err(format!("Task #{id} is blocked by {}.", numbers(&waiting)));
    }
    if let Some(seen) = seen.filter(|seen| *seen != task.rev) {
        return Err(format!(
            "Task #{id} has changed since you read it at rev {seen}; it is at rev {} now. \
             List it again before you claim it.",
            task.rev
        ));
    }
    Ok(owned(task.clone(), Some(who.to_owned())))
}

/// Task `id` with the title, detail or owner `args` gives. Taking a task
/// for oneself is a claim, with all its checks, however it is asked for.
fn updated(
    tasks: &[Task],
    id: u64,
    by: &str,
    here: &[String],
    args: &Value,
) -> Result<Task, String> {
    let task = find(tasks, id)?;
    may_change(task, by)?;
    let mut task = match args["owner"].as_str() {
        None => task.clone(),
        Some(_) if task.status.closed() => {
            return Err(format!(
                "Task #{id} is {}; reopen it before you give it to anyone.",
                task.status.word()
            ));
        }
        Some(owner) => match owner_of(owner, by, here)? {
            Some(owner) if owner == by => claimed(tasks, id, by, args["rev"].as_u64(), here)?,
            Some(owner) => owned(task.clone(), Some(owner)),
            None if by == MAIN => owned(task.clone(), None),
            None if task.owner.as_deref() == Some(by) => given_back(task.clone()),
            None => return Err(format!("Task #{id} is not yours to give up.")),
        },
    };
    let mut any = args["owner"].is_string();
    if let Some(title) = title(args)? {
        (task.title, any) = (title, true);
    }
    if let Some(detail) = detail(args)? {
        (task.detail, any) = (detail, true);
    }
    if !any {
        return Err("Nothing to change: give a title, a detail or an owner.".into());
    }
    Ok(task)
}

/// Task `id` marked done: by the helper that holds it, or by the main agent,
/// and only once every task it waits for is done.
fn finished(tasks: &[Task], id: u64, by: &str) -> Result<Task, String> {
    let task = find(tasks, id)?;
    may_change(task, by)?;
    if task.status.closed() {
        return Err(format!("Task #{id} is {} already.", task.status.word()));
    }
    if by != MAIN && task.owner.is_none() {
        return Err(format!(
            "Task #{id} is nobody's; claim it before you mark it done."
        ));
    }
    let waiting = waiting(task, tasks);
    if !waiting.is_empty() {
        return Err(format!(
            "Task #{id} still waits for {}, which must be done first.",
            numbers(&waiting)
        ));
    }
    Ok(Task {
        status: Status::Done,
        ..task.clone()
    })
}

/// Task `id` open again for anyone to take: one done, dropped or held. The
/// main agent may reopen any; a helper only one it finished or holds, and
/// one it holds counts as given back.
fn reopened(tasks: &[Task], id: u64, by: &str) -> Result<Task, String> {
    let task = find(tasks, id)?;
    may_change(task, by)?;
    let helper = by != MAIN;
    match task.status {
        Status::Open => return Err(format!("Task #{id} is open already.")),
        Status::Blocked => return Err(format!("Task #{id} is blocked; unblock it instead.")),
        Status::Dropped if helper => {
            return Err("Only the main agent reopens a dropped task.".into());
        }
        _ if helper && task.owner.is_none() => {
            return Err(format!("Task #{id} is not yours to reopen."));
        }
        Status::Claimed if helper => return Ok(given_back(task.clone())),
        _ => {}
    }
    Ok(Task {
        status: Status::Open,
        owner: None,
        ..task.clone()
    })
}

/// Task `id` dropped by the main agent: never offered again, and no longer
/// waited for by the tasks that waited for it.
fn dropped(tasks: &[Task], id: u64, by: &str) -> Result<Task, String> {
    if by != MAIN {
        return Err("Only the main agent drops a task; tell main why it should go.".into());
    }
    let task = find(tasks, id)?;
    if task.status.closed() {
        return Err(format!("Task #{id} is {} already.", task.status.word()));
    }
    Ok(Task {
        status: Status::Dropped,
        ..task.clone()
    })
}

/// Task `id` held until unblocked, or, given `ids`, waiting for those tasks too.
fn blocked(tasks: &[Task], id: u64, by: &str, ids: &[u64]) -> Result<Task, String> {
    let task = find(tasks, id)?;
    may_change(task, by)?;
    if task.status.closed() {
        return Err(format!(
            "Task #{id} is {}; there is nothing to block.",
            task.status.word()
        ));
    }
    let mut task = task.clone();
    if ids.is_empty() {
        if task.status == Status::Blocked {
            return Err(format!("Task #{id} is blocked already."));
        }
        task.status = Status::Blocked;
        return Ok(task);
    }
    check_blockers(tasks, id, ids)?;
    let new: Vec<u64> = (ids.iter().copied())
        .filter(|id| !task.blocked_by.contains(id))
        .collect();
    if new.is_empty() {
        return Err(format!("Task #{id} waits for {} already.", numbers(ids)));
    }
    task.blocked_by.extend(new);
    Ok(task)
}

/// Task `id` no longer waiting for `ids`, or, given none, no longer held or
/// waiting for anything; one given back too often only by the main agent.
fn unblocked(tasks: &[Task], id: u64, by: &str, ids: &[u64]) -> Result<Task, String> {
    let task = find(tasks, id)?;
    may_change(task, by)?;
    if needs_decision(task) && by != MAIN {
        return Err(format!(
            "Task #{id} was given back too often and waits for the main agent to decide."
        ));
    }
    let mut task = task.clone();
    let before = task.blocked_by.len();
    if ids.is_empty() {
        if task.status != Status::Blocked && before == 0 {
            return Err(format!("Task #{id} is not blocked."));
        }
        task.blocked_by.clear();
    } else {
        task.blocked_by.retain(|blocker| !ids.contains(blocker));
        if task.blocked_by.len() == before {
            return Err(format!("Task #{id} does not wait for {}.", numbers(ids)));
        }
    }
    if ids.is_empty() && task.status == Status::Blocked {
        let owner = task.owner.take();
        task = owned(
            Task {
                status: Status::Open,
                ..task
            },
            owner,
        );
    }
    Ok(task)
}

/// One line of the list for `task`: its number, status, owner, what it
/// waits for, who added it and its revision, then its title.
fn row(task: &Task, tasks: &[Task], here: &[String]) -> String {
    let mut row = format!("#{} {}", task.id, task.status.word());
    if needs_decision(task) {
        row.push_str(", needs a decision");
    }
    if let Some(owner) = &task.owner {
        // Whoever finished or held a closed task need not run any more.
        let owner = match task.status.closed() {
            true => owner.clone(),
            false => holder(owner, here),
        };
        row.push_str(&format!(", owner {owner}"));
    }
    let waiting = waiting(task, tasks);
    if !waiting.is_empty() && !task.status.closed() {
        row.push_str(&format!(", blocked by {}", numbers(&waiting)));
    }
    if !task.by.is_empty() {
        row.push_str(&format!(", added by {}", task.by));
    }
    format!("{row}, rev {}: {}", task.rev, task.title)
}

/// The list as the tool shows it: a row per task, with the start of the
/// detail of each not finished yet.
fn listed(tasks: &[Task], here: &[String]) -> String {
    if tasks.is_empty() {
        return "The task list is empty.".into();
    }
    let mut out = Vec::new();
    for task in tasks {
        out.push(row(task, tasks, here));
        if task.status.closed() || task.detail.is_empty() {
            continue;
        }
        let mut detail: String = task.detail.chars().take(LISTED_DETAIL).collect();
        if detail.len() < task.detail.len() {
            detail.push('…');
        }
        out.push(format!("    {}", detail.replace('\n', " ")));
    }
    out.join("\n")
}

/// What a helper that takes `task` is handed to work on.
fn handed(task: &Task) -> Notice {
    let id = task.id;
    let by = match task.by.as_str() {
        "" => String::new(),
        by => format!(", added by {by},"),
    };
    let mut text = format!(
        "Task #{id} on the session's task list{by} is yours now: {}",
        task.title
    );
    if !task.detail.is_empty() {
        text.push_str(&format!("\n{}", task.detail));
    }
    text.push_str(&format!(
        "\nWhen it is finished, mark it with board {{\"action\": \"done\", \"id\": {id}}}; \
         if it cannot go on, block it and tell main why."
    ));
    Notice {
        from: FROM.into(),
        when: When::Next,
        text,
    }
}

/// Whether the main agent hears of `changed`, a helper's change: only of a
/// task finished, held or made to wait, or given back. Claims and edits it
/// can list, and a notice of each would wake it for nothing.
fn concerns_main(changed: &Changed) -> bool {
    let (task, Some(before)) = (&changed.task, &changed.before) else {
        return false;
    };
    let finished = task.status == Status::Done && before.status != Status::Done;
    let held = task.status == Status::Blocked && before.status != Status::Blocked;
    let waits = task.blocked_by.len() > before.blocked_by.len();
    let given_back = task.owner.is_none() && before.owner.is_some();
    finished || held || waits || given_back
}

/// Tells whoever `changed` concerns that `by` `verb` it: the session, when a
/// helper made a change it hears of, and each helper that owns the task or
/// owned it before, unless that helper made it.
fn tell(root: u64, by: &str, verb: &str, changed: &Changed, tasks: &[Task], here: &[String]) {
    let task = &changed.task;
    let who = if by == MAIN {
        MAIN.to_owned()
    } else {
        format!("helper {by}")
    };
    let notice = |text: String| Notice {
        from: FROM.into(),
        when: When::Next,
        text,
    };
    let mut text = format!(
        "{who} {verb} task #{}:\n{}",
        task.id,
        row(task, tasks, here)
    );
    if by != MAIN && concerns_main(changed) {
        let mut to_main = text.clone();
        if needs_decision(task) && !changed.before.as_ref().is_some_and(needs_decision) {
            to_main.push_str(&format!(
                "\nIt has been given back {} times, so it is held for you: give it to \
                 a helper, unblock it or drop it.",
                task.given_back
            ));
        }
        inbox::push(root, notice(to_main));
    }
    let before = changed.before.as_ref().and_then(|b| b.owner.as_deref());
    let mut owners: Vec<&str> = [before, task.owner.as_deref()]
        .into_iter()
        .flatten()
        .filter(|owner| *owner != by && *owner != MAIN)
        .collect();
    owners.dedup();
    if !task.detail.is_empty() {
        text.push_str(&format!("\n{}", task.detail));
    }
    for owner in owners {
        // One that has ended holds nothing; its end gave its tasks back.
        agents::notify(root, owner, notice(text.clone()));
    }
}

/// Makes the change `edit` gives for `by` to session `root`'s list, tells
/// whoever it concerns and offers what can be taken now to the idle helpers,
/// and says what it did and who took what.
fn apply(
    root: u64,
    by: &str,
    here: &[String],
    verb: &str,
    edit: impl FnOnce(&[Task]) -> Result<Task, String>,
) -> Result<String, String> {
    let changed = change(root, |tasks| Ok(vec![edit(tasks)?]))?;
    let tasks = load(root);
    let mut out = Vec::new();
    for changed in &changed {
        tell(root, by, verb, changed, &tasks, here);
        let task = &changed.task;
        out.push(format!(
            "You {verb} task #{}:\n{}",
            task.id,
            row(task, &tasks, here)
        ));
        // Whoever holds it now has what it is to do.
        let yours = task.owner.as_deref() == Some(by) && !task.status.closed();
        if yours && !task.detail.is_empty() {
            out.push(task.detail.clone());
        }
    }
    out.extend(offer(root));
    Ok(out.join("\n"))
}

/// Offers each task of session `root` that can be taken to its idle kept
/// helpers, in order, and says who took which; the helper hears what it took,
/// and the session can list it.
fn offer(root: u64) -> Vec<String> {
    let tasks = load(root);
    let mut took = Vec::new();
    for task in tasks.iter().filter(|task| claimable(task, &tasks)) {
        let taker = agents::ring(root, |name| {
            // An open task has no owner, so no helper list is needed.
            let claim = |now: &[Task]| Ok(vec![claimed(now, task.id, name, Some(task.rev), &[])?]);
            let changed = change(root, claim).ok()?.pop()?;
            Some(handed(&changed.task))
        });
        // Nobody idle took it, so nobody takes the next; a change that came
        // between the list and the claim offers again itself.
        let Some(name) = taker else {
            break;
        };
        took.push(format!("helper {name} took task #{}.", task.id));
    }
    took
}

/// Gives back every task helper `name` of session `root` held and had not
/// finished, since its run has ended: the session hears of each, and they
/// are offered to the idle helpers left. Runs for every helper whose run
/// ends without going idle, kept or not.
fn released(root: u64, name: &str) {
    if !path(root).exists() {
        return;
    }
    let changed = change(root, |tasks| {
        Ok((tasks.iter())
            .filter(|task| task.owner.as_deref() == Some(name) && !task.status.closed())
            .map(|task| given_back(task.clone()))
            .collect())
    });
    // A list that cannot be written keeps them claimed; they show as not
    // running, and the main agent can still give them to someone else.
    let Ok(changed) = changed else {
        return;
    };
    if changed.is_empty() {
        return;
    }
    let (tasks, here) = (load(root), agents::here(root));
    for changed in &changed {
        tell(root, name, "ended before finishing", changed, &tasks, &here);
    }
    offer(root);
}

/// Offers what waits on session `root`'s list as one of its kept helpers
/// goes idle, since no change may come to ring it.
fn pulled(root: u64, _name: &str) {
    if path(root).exists() {
        offer(root);
    }
}

/// The `board` tool: the session's shared task list, for its own agent and
/// every helper, which asks for no approval since it changes only the list.
pub struct Board;

impl Tool for Board {
    fn name(&self) -> &'static str {
        "board"
    }
    fn description(&self) -> &'static str {
        "The session's task list, shared with every helper of this session; your own step-by-step checklist stays in todo. add: a task with a one-line title, a detail, and blocked_by, the tasks to finish first. list: every task with its status, owner, blockers, who added it and its rev. claim: take an open task, giving the rev you listed; it fails if the task changed since. update: a task's title, detail or owner; \"me\" takes it as claim does, and only the main agent names a helper. done: finish a task you hold, once its blockers are done. reopen: open a done task again, or give back one you hold. drop: cancel a task (main agent only). block: hold a task, or with blocked_by make it wait for others; unblock undoes either. Idle helpers are offered each open task whose blockers are done; one given back again and again is held for the main agent."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "action": {"type": "string", "enum": ["add", "list", "claim", "update", "done", "reopen", "drop", "block", "unblock"]},
            "id": {"type": "integer", "description": "The task's number, as list shows it."},
            "rev": {"type": "integer", "description": "For claim, or update taking a task: the revision list showed."},
            "title": {"type": "string"},
            "detail": {"type": "string"},
            "owner": {"type": "string", "description": "\"me\", a helper's name, or \"\" for nobody."},
            "blocked_by": {"type": "array", "items": {"type": "integer"}, "description": "The numbers of the tasks it waits for."}
        }, "required": ["action"]})
    }
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let root = ctx.caller.root;
        if root == 0 {
            return Err("The task list belongs to a session, and this call is in none.".into());
        }
        let by = match ctx.caller.agent {
            None => MAIN.to_owned(),
            Some(id) => agents::name_of(id)
                .ok_or("This helper is not known here, so it cannot use the task list.")?,
        };
        let by = by.as_str();
        // So a helper whose run ends gives back what it held, and one that
        // goes idle takes what waits.
        agents::on_end(released);
        agents::on_idle(pulled);
        let here = agents::here(root);
        let action = args["action"].as_str().unwrap_or_default();
        if action == "list" {
            return Ok(listed(&load(root), &here));
        }
        if action == "add" {
            return apply(root, by, &here, "added", |tasks| {
                added(tasks, by, &here, args)
            });
        }
        let Some(id) = args["id"].as_u64() else {
            return Err("`id` is required: the task's number, as list shows it.".into());
        };
        let ids = blockers(args)?;
        let gives_up = args["owner"]
            .as_str()
            .is_some_and(|owner| owner.trim().is_empty());
        let update = if gives_up { "gave up" } else { "changed" };
        match action {
            "claim" => apply(root, by, &here, "claimed", |tasks| {
                claimed(tasks, id, by, args["rev"].as_u64(), &here)
            }),
            "update" => apply(root, by, &here, update, |tasks| {
                updated(tasks, id, by, &here, args)
            }),
            "done" => apply(root, by, &here, "finished", |tasks| finished(tasks, id, by)),
            "reopen" => apply(root, by, &here, "reopened", |tasks| reopened(tasks, id, by)),
            "drop" => apply(root, by, &here, "dropped", |tasks| dropped(tasks, id, by)),
            "block" => apply(root, by, &here, "blocked", |tasks| {
                blocked(tasks, id, by, &ids)
            }),
            "unblock" => apply(root, by, &here, "unblocked", |tasks| {
                unblocked(tasks, id, by, &ids)
            }),
            other => Err(format!(
                "No action \"{other}\": use add, list, claim, update, done, reopen, drop, block \
                 or unblock."
            )),
        }
    }
    fn machine(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::Scratch;

    /// Points Letronna's data at a fresh folder, kept from other tests until
    /// the guard goes and removed with it.
    fn data(name: &str) -> (MutexGuard<'static, ()>, Scratch) {
        let turn = (crate::store::tests::DATA_DIR.lock()).unwrap_or_else(|e| e.into_inner());
        let dir = Scratch::new(&format!("tasklist-{name}"));
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        (turn, dir)
    }

    /// The helpers the tests treat as running here.
    fn crew() -> Vec<String> {
        ["tester", "reviewer"].map(str::to_owned).to_vec()
    }

    /// Makes a change as the tool does, with the crew running.
    fn act(
        root: u64,
        by: &str,
        verb: &str,
        edit: impl FnOnce(&[Task]) -> Result<Task, String>,
    ) -> Result<String, String> {
        apply(root, by, &crew(), verb, edit)
    }

    fn add(root: u64, by: &str, args: Value) -> Result<String, String> {
        act(root, by, "added", |tasks| added(tasks, by, &crew(), &args))
    }

    fn claim(root: u64, id: u64, who: &str, seen: Option<u64>) -> Result<String, String> {
        act(root, who, "claimed", |tasks| {
            claimed(tasks, id, who, seen, &crew())
        })
    }

    /// Changes task `id` as `by` with the update `args`.
    fn update(root: u64, id: u64, by: &str, args: Value) -> Result<String, String> {
        act(root, by, "changed", |tasks| {
            updated(tasks, id, by, &crew(), &args)
        })
    }

    /// The text of every notice waiting for `to`.
    fn heard(to: u64) -> Vec<String> {
        let notices = inbox::take(to, true);
        notices.into_iter().map(|notice| notice.text).collect()
    }

    /// The numbers of session `root`'s tasks an idle helper would be offered.
    fn offered(root: u64) -> Vec<u64> {
        let tasks = load(root);
        let open = tasks.iter().filter(|task| claimable(task, &tasks));
        open.map(|task| task.id).collect()
    }

    fn line(id: u64, rev: u64, status: Status, owner: Option<&str>) -> String {
        let task = Task {
            id,
            rev,
            title: format!("task {id}"),
            detail: String::new(),
            status,
            owner: owner.map(str::to_owned),
            blocked_by: Vec::new(),
            by: String::new(),
            given_back: 0,
        };
        serde_json::to_string(&task).unwrap()
    }

    /// Makes `lines` session `root`'s whole list.
    fn write(root: u64, lines: &[String]) {
        std::fs::create_dir_all(path(root).parent().unwrap()).unwrap();
        std::fs::write(path(root), lines.join("\n")).unwrap();
    }

    #[test]
    fn the_list_is_folded_from_its_lines_the_latest_of_each_task_winning() {
        let _data = data("fold");
        let torn = r#"{"id":3,"rev":1,"title":"tor"#;
        write(
            7_001,
            &[
                line(1, 1, Status::Open, None),
                line(2, 1, Status::Open, None),
                line(1, 2, Status::Claimed, Some("tester")),
                torn.to_owned(),
            ],
        );

        let loaded = load(7_001);
        let added = add(7_001, MAIN, json!({"title": "after the tear"}));
        let after = load(7_001);

        let brief = |tasks: &[Task]| -> Vec<(u64, u64, Status)> {
            tasks.iter().map(|t| (t.id, t.rev, t.status)).collect()
        };
        assert_eq!(
            brief(&loaded),
            [(1, 2, Status::Claimed), (2, 1, Status::Open)]
        );
        assert_eq!(loaded[0].owner.as_deref(), Some("tester"));
        // Numbered past the highest that reads, and not swallowed by the tear.
        assert_eq!(
            added,
            Ok("You added task #3:\n#3 open, added by main, rev 1: after the tear".into())
        );
        assert_eq!(after.len(), 3);
        assert_eq!(after[2].title, "after the tear");
    }

    #[test]
    fn writers_on_many_threads_lose_no_change_and_share_no_id() {
        let _data = data("threads");
        let writers: Vec<_> = (0..8)
            .map(|writer| {
                std::thread::spawn(move || {
                    for n in 0..10 {
                        let title = format!("task {writer}.{n}");
                        add(7_002, MAIN, json!({ "title": title })).unwrap();
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }

        let tasks = load(7_002);
        let ids: Vec<u64> = tasks.iter().map(|task| task.id).collect();
        let mut titles: Vec<&str> = tasks.iter().map(|task| task.title.as_str()).collect();
        titles.sort_unstable();
        titles.dedup();
        assert_eq!(ids, (1..=80).collect::<Vec<u64>>());
        assert_eq!(titles.len(), 80);
        // Every writer locked the one file, which stays.
        assert!(lock_path(7_002).exists());
    }

    #[test]
    fn a_change_waits_while_another_handle_holds_the_lock_and_never_writes_without_it() {
        let _data = data("lock");
        let root = 7_007;
        add(root, MAIN, json!({"title": "Only once"})).unwrap();
        // Another process's hold, as the OS sees it: a second handle's lock.
        let other = File::options().write(true).open(lock_path(root)).unwrap();
        other.lock().unwrap();
        let claims: Vec<_> = ["tester", "reviewer"]
            .into_iter()
            .map(|who| std::thread::spawn(move || claim(root, 1, who, Some(1))))
            .collect();
        std::thread::sleep(Duration::from_millis(300));
        let waited = claims.iter().all(|claim| !claim.is_finished());
        other.unlock().unwrap();
        let results: Vec<_> = (claims.into_iter())
            .map(|claim| claim.join().unwrap())
            .collect();
        let owner = load(root)[0].owner.clone().unwrap();
        other.lock().unwrap();
        let busy = add(root, MAIN, json!({"title": "Never written"}));
        drop(other);

        assert!(waited);
        let won: Vec<_> = results.iter().filter(|result| result.is_ok()).collect();
        assert_eq!(won.len(), 1, "{results:?}");
        let lost = results.iter().find_map(|result| result.clone().err());
        assert_eq!(
            lost,
            Some(format!("Task #1 is already claimed by {owner}."))
        );
        assert_eq!(busy, Err(BUSY.into()));
        assert_eq!(load(root).len(), 1);
    }

    #[test]
    fn a_claim_on_a_stale_read_or_of_a_taken_task_is_refused() {
        let _data = data("stale");
        add(7_003, MAIN, json!({"title": "Write the tests"})).unwrap();
        let read = load(7_003)[0].rev;
        update(7_003, 1, MAIN, json!({"detail": "In tasklist.rs."})).unwrap();

        let stale = claim(7_003, 1, "tester", Some(read));
        let fresh = claim(7_003, 1, "tester", Some(read + 1));
        let taken = claim(7_003, 1, "reviewer", None);
        let twice = claim(7_003, 1, "tester", None);
        heard(7_003);

        assert_eq!(
            stale,
            Err(
                "Task #1 has changed since you read it at rev 1; it is at rev 2 now. \
                 List it again before you claim it."
                    .into()
            )
        );
        assert_eq!(
            fresh,
            Ok(
                "You claimed task #1:\n#1 claimed, owner tester, added by main, rev 3: \
                 Write the tests\nIn tasklist.rs."
                    .into()
            )
        );
        assert_eq!(taken, Err("Task #1 is already claimed by tester.".into()));
        assert_eq!(twice, Err("Task #1 is already claimed by you.".into()));
    }

    #[test]
    fn taking_a_task_by_update_passes_every_check_a_claim_makes() {
        let _data = data("take");
        let root = 7_008;
        add(root, MAIN, json!({"title": "Build"})).unwrap();
        add(root, MAIN, json!({"title": "Test", "blocked_by": [1]})).unwrap();
        add(root, MAIN, json!({"title": "Ship"})).unwrap();
        act(root, MAIN, "blocked", |tasks| blocked(tasks, 3, MAIN, &[])).unwrap();
        let take = |id: u64, rev: Option<u64>| {
            update(root, id, "tester", json!({"owner": "me", "rev": rev}))
        };

        let waiting = take(2, None);
        let held = take(3, None);
        let stale = take(1, Some(0));
        let fresh = take(1, Some(1));
        let taken = update(root, 1, "reviewer", json!({"owner": "me"}));
        heard(root);

        assert_eq!(waiting, Err("Task #2 is blocked by #1.".into()));
        assert_eq!(
            held,
            Err("Task #3 is blocked until someone unblocks it.".into())
        );
        assert!(
            stale
                .as_ref()
                .is_err_and(|e| e.contains("changed since you read it at rev 0")),
            "{stale:?}"
        );
        assert_eq!(
            fresh,
            Ok(
                "You changed task #1:\n#1 claimed, owner tester, added by main, rev 2: Build"
                    .into()
            )
        );
        assert_eq!(
            taken,
            Err("Task #1 is tester's; only tester or the main agent may change it.".into())
        );
    }

    #[test]
    fn blockers_keep_a_task_from_being_offered_or_claimed_until_they_are_done() {
        let _data = data("blockers");
        let root = 7_004;
        add(root, MAIN, json!({"title": "Build"})).unwrap();
        add(root, MAIN, json!({"title": "Test", "blocked_by": [1]})).unwrap();

        let cycle = act(root, MAIN, "blocked", |tasks| blocked(tasks, 1, MAIN, &[2]));
        let early = claim(root, 2, "tester", None);
        let (listed_early, offered_early) = (listed(&load(root), &crew()), offered(root));
        act(root, MAIN, "finished", |tasks| finished(tasks, 1, MAIN)).unwrap();
        let offered_after = offered(root);
        act(root, MAIN, "blocked", |tasks| blocked(tasks, 2, MAIN, &[])).unwrap();
        let held = claim(root, 2, "tester", None);
        let offered_held = offered(root);
        act(root, MAIN, "unblocked", |tasks| {
            unblocked(tasks, 2, MAIN, &[])
        })
        .unwrap();
        let late = claim(root, 2, "tester", None);
        heard(root);

        assert_eq!(
            cycle,
            Err("Task #2 already waits for #1, so #1 cannot wait for it.".into())
        );
        assert_eq!(early, Err("Task #2 is blocked by #1.".into()));
        assert!(
            listed_early.contains("#2 open, blocked by #1, added by main, rev 1: Test"),
            "{listed_early}"
        );
        assert_eq!(offered_early, [1]);
        assert_eq!(offered_after, [2]);
        assert_eq!(
            held,
            Err("Task #2 is blocked until someone unblocks it.".into())
        );
        assert!(offered_held.is_empty());
        assert!(late.is_ok(), "{late:?}");
    }

    #[test]
    fn only_its_holder_finishes_a_task_once_its_blockers_are_done_and_can_reopen_it() {
        let _data = data("done");
        let root = 7_009;
        add(root, MAIN, json!({"title": "Build"})).unwrap();
        add(root, MAIN, json!({"title": "Test", "blocked_by": [1]})).unwrap();
        let done =
            |id: u64, who: &str| act(root, who, "finished", |tasks| finished(tasks, id, who));
        let reopen =
            |id: u64, who: &str| act(root, who, "reopened", |tasks| reopened(tasks, id, who));

        let nobodys = done(1, "tester");
        claim(root, 1, "tester", None).unwrap();
        let others = done(1, "reviewer");
        // The main agent gives out a task whatever it waits for.
        update(root, 2, MAIN, json!({"owner": "tester"})).unwrap();
        let early = done(2, "tester");
        done(1, "tester").unwrap();
        let again = done(1, "tester");
        let not_theirs = reopen(1, "reviewer");
        let reopened = reopen(1, "tester");
        let task = load(root)[0].clone();
        heard(root);

        assert_eq!(
            nobodys,
            Err("Task #1 is nobody's; claim it before you mark it done.".into())
        );
        assert_eq!(
            others,
            Err("Task #1 is tester's; only tester or the main agent may change it.".into())
        );
        assert_eq!(
            early,
            Err("Task #2 still waits for #1, which must be done first.".into())
        );
        assert_eq!(again, Err("Task #1 is done already.".into()));
        assert_eq!(
            not_theirs,
            Err("Task #1 is tester's; only tester or the main agent may change it.".into())
        );
        assert!(reopened.is_ok(), "{reopened:?}");
        assert_eq!(
            (task.status, task.owner, task.given_back),
            (Status::Open, None, 0)
        );
    }

    #[test]
    fn a_dropped_task_is_never_offered_and_only_the_main_agent_drops_or_reopens_it() {
        let _data = data("dropped");
        let root = 7_010;
        add(root, MAIN, json!({"title": "Spike"})).unwrap();
        add(root, MAIN, json!({"title": "Follow up", "blocked_by": [1]})).unwrap();

        let by_helper = act(root, "tester", "dropped", |tasks| {
            dropped(tasks, 1, "tester")
        });
        act(root, MAIN, "dropped", |tasks| dropped(tasks, 1, MAIN)).unwrap();
        let offered_dropped = offered(root);
        let claim_dropped = claim(root, 1, "tester", None);
        let reopen_helper = act(root, "tester", "reopened", |tasks| {
            reopened(tasks, 1, "tester")
        });
        act(root, MAIN, "reopened", |tasks| reopened(tasks, 1, MAIN)).unwrap();
        let offered_reopened = offered(root);
        heard(root);

        assert_eq!(
            by_helper,
            Err("Only the main agent drops a task; tell main why it should go.".into())
        );
        // No longer waited for, so what waited for it can go ahead.
        assert_eq!(offered_dropped, [2]);
        assert_eq!(claim_dropped, Err("Task #1 is dropped already.".into()));
        assert_eq!(
            reopen_helper,
            Err("Only the main agent reopens a dropped task.".into())
        );
        assert_eq!(offered_reopened, [1]);
    }

    #[test]
    fn a_helper_that_ends_gives_back_the_tasks_it_had_not_finished() {
        let _data = data("released");
        let root = 7_005;
        add(root, MAIN, json!({"title": "Lint"})).unwrap();
        add(root, MAIN, json!({"title": "Format"})).unwrap();
        claim(root, 1, "gone", None).unwrap();
        claim(root, 2, "gone", None).unwrap();
        act(root, "gone", "finished", |tasks| finished(tasks, 2, "gone")).unwrap();
        heard(root);

        released(root, "gone");
        let tasks = load(root);

        assert_eq!(
            (
                tasks[0].status,
                tasks[0].owner.as_deref(),
                tasks[0].given_back
            ),
            (Status::Open, None, 1)
        );
        assert_eq!(
            (tasks[1].status, tasks[1].owner.as_deref()),
            (Status::Done, Some("gone"))
        );
        assert_eq!(
            heard(root),
            ["helper gone ended before finishing task #1:\n#1 open, added by main, rev 3: Lint"]
        );
    }

    #[test]
    fn the_session_hears_only_what_a_helper_finishes_holds_or_gives_back() {
        let _data = data("notices");
        let root = 7_006;
        add(root, MAIN, json!({"title": "Plan"})).unwrap();
        add(root, MAIN, json!({"title": "Check"})).unwrap();
        let by_main = heard(root);
        add(root, "tester", json!({"title": "Mine", "owner": "me"})).unwrap();
        claim(root, 1, "tester", None).unwrap();
        update(root, 1, "tester", json!({"detail": "All of it."})).unwrap();
        act(root, "tester", "unblocked", |tasks| {
            unblocked(tasks, 1, "tester", &[])
        })
        .unwrap_err();
        let quiet = heard(root);
        act(root, "tester", "blocked", |tasks| {
            blocked(tasks, 1, "tester", &[])
        })
        .unwrap();
        let held = heard(root);
        act(root, "tester", "unblocked", |tasks| {
            unblocked(tasks, 1, "tester", &[])
        })
        .unwrap();
        act(root, "tester", "finished", |tasks| {
            finished(tasks, 1, "tester")
        })
        .unwrap();
        let done = heard(root);
        let gave_up = act(root, "tester", "gave up", |tasks| {
            updated(tasks, 3, "tester", &crew(), &json!({"owner": ""}))
        });
        let given_back = heard(root);
        claim(root, 2, "tester", None).unwrap();
        let given = update(root, 2, "tester", json!({"owner": "reviewer"}));
        let taken = act(root, "reviewer", "finished", |tasks| {
            finished(tasks, 2, "reviewer")
        });

        assert!(by_main.is_empty(), "{by_main:?}");
        assert!(quiet.is_empty(), "{quiet:?}");
        assert_eq!(
            held,
            [
                "helper tester blocked task #1:\n#1 blocked, owner tester, added by main, rev 4: Plan"
            ]
        );
        assert_eq!(
            done,
            ["helper tester finished task #1:\n#1 done, owner tester, added by main, rev 6: Plan"]
        );
        assert!(gave_up.is_ok(), "{gave_up:?}");
        assert_eq!(
            given_back,
            ["helper tester gave up task #3:\n#3 open, added by tester, rev 2: Mine"]
        );
        assert_eq!(
            given,
            Err(
                "Only the main agent gives a task to someone else: claim it for \
                 yourself, or leave the owner empty to give it up."
                    .into()
            )
        );
        assert_eq!(
            taken,
            Err("Task #2 is tester's; only tester or the main agent may change it.".into())
        );
    }

    #[test]
    fn the_cap_counts_only_unfinished_tasks_and_a_long_list_is_written_afresh() {
        let _data = data("cap");
        let root = 7_011;
        let open: Vec<String> = (1..=200)
            .map(|id| line(id, 1, Status::Open, None))
            .collect();
        let done: Vec<String> = (1..=200)
            .map(|id| line(id, 2, Status::Done, None))
            .collect();
        write(root, &open);
        let full = add(root, MAIN, json!({"title": "One more"}));
        write(root, &[open, done].concat());
        let room = add(root, MAIN, json!({"title": "One more"}));
        // A list at the limit, which the next change passes.
        let many: Vec<String> = (1..=COMPACT_AT as u64)
            .map(|id| line(id, 1, Status::Dropped, None))
            .collect();
        write(root, &many);
        add(root, MAIN, json!({"title": "After"})).unwrap();
        let text = std::fs::read_to_string(path(root)).unwrap();
        let ids: Vec<u64> = load(root).iter().map(|task| task.id).collect();

        assert_eq!(
            full,
            Err(
                "The task list holds 200 tasks not finished, the most it keeps; \
                 finish or drop some first."
                    .into()
            )
        );
        assert!(room.is_ok_and(|room| room.starts_with("You added task #201:")));
        assert_eq!(text.lines().count(), KEEP_FINISHED + 1);
        assert_eq!(ids.first(), Some(&951));
        assert_eq!(ids.last(), Some(&1_001));
    }

    #[test]
    fn a_task_given_back_too_often_is_held_for_the_main_agent() {
        let _data = data("given-back");
        let root = 7_012;
        add(root, MAIN, json!({"title": "Flaky"})).unwrap();
        let mut told = Vec::new();
        for _ in 0..=MAX_GIVEN_BACK {
            claim(root, 1, "tester", None).unwrap();
            update(root, 1, "tester", json!({"owner": ""})).unwrap();
            told.push(heard(root));
        }
        let task = load(root)[0].clone();
        let taken = claim(root, 1, "reviewer", None);
        let unblocked_by_helper = act(root, "tester", "unblocked", |tasks| {
            unblocked(tasks, 1, "tester", &[])
        });
        act(root, MAIN, "unblocked", |tasks| {
            unblocked(tasks, 1, MAIN, &[])
        })
        .unwrap();
        let after = load(root)[0].clone();

        assert_eq!((task.status, task.given_back), (Status::Blocked, 4));
        assert_eq!(
            told[2],
            ["helper tester changed task #1:\n#1 open, added by main, rev 7: Flaky"]
        );
        assert_eq!(
            told[3],
            [
                "helper tester changed task #1:\n#1 blocked, needs a decision, added by main, \
              rev 9: Flaky\nIt has been given back 4 times, so it is held for you: give it \
              to a helper, unblock it or drop it."
            ]
        );
        let decide = "Task #1 was given back too often and waits for the main agent to decide.";
        assert_eq!(taken, Err(decide.into()));
        assert_eq!(unblocked_by_helper, Err(decide.into()));
        assert_eq!((after.status, after.owner), (Status::Open, None));
    }

    #[test]
    fn an_owner_not_running_here_is_shown_so_and_holds_nothing_against_the_main_agent() {
        let _data = data("ghost");
        let root = 7_013;
        write(
            root,
            &[
                line(1, 1, Status::Claimed, Some("ghost")),
                line(2, 1, Status::Open, None),
            ],
        );

        let listed = listed(&load(root), &crew());
        let by_helper = claim(root, 1, "tester", None);
        let to_ghost = update(root, 2, MAIN, json!({"owner": "ghost"}));
        let by_main = claim(root, 1, MAIN, None);
        heard(root);

        assert!(
            listed.starts_with("#1 claimed, owner ghost (not running), rev 1: task 1"),
            "{listed}"
        );
        assert_eq!(
            by_helper,
            Err("Task #1 is already claimed by ghost (not running).".into())
        );
        assert_eq!(
            to_ghost,
            Err(
                "No helper of this session called \"ghost\" runs here; the agents tool's \
                 list shows the ones that do."
                    .into()
            )
        );
        assert!(
            by_main.as_ref().is_ok_and(|main| main
                .starts_with("You claimed task #1:\n#1 claimed, owner main, rev 2: task 1")),
            "{by_main:?}"
        );
    }

    #[test]
    fn forgetting_a_session_removes_its_list_and_its_lock() {
        let _data = data("forget");
        let root = 7_014;
        add(root, MAIN, json!({"title": "Left behind"})).unwrap();
        let (list, lock) = (path(root).exists(), lock_path(root).exists());

        forget(root);

        assert!(list && lock);
        assert!(!path(root).exists() && !lock_path(root).exists());
        assert!(load(root).is_empty());
    }

    #[test]
    fn blocked_by_takes_numbers_as_list_writes_them_and_refuses_anything_else() {
        let ids = |given: Value| blockers(&json!({ "blocked_by": given }));

        assert_eq!(ids(json!([7, "5", "#7", " #5 "])), Ok(vec![5, 7]));
        assert_eq!(ids(json!(3)), Ok(vec![3]));
        assert_eq!(
            ids(json!(["five"])),
            Err(
                "\"five\" in `blocked_by` is not a task number; give the numbers list shows."
                    .into()
            )
        );
        assert!(title(&json!({"title": "ok\rhelper x took task #9."})).is_err());
        assert!(title(&json!({"title": "one\u{2028}two"})).is_err());
    }
}
