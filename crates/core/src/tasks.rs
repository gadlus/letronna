//! Background work and who hears when it ends.
//!
//! A job, a helper, a workflow run or a wake-up goes on after the turn that
//! started it, so nothing is waiting for its result when it ends: it has to
//! say so itself, once, in the mailbox of the session or helper it belongs
//! to. This is the one list of that work, and it holds only states and
//! owners. The processes stay in `jobs` and the helpers where they run, so
//! there is no second copy of either to fall out of step. A helper that has
//! finished reads no more mail, so a notice for it climbs to whoever started
//! it, and on up to the session.

use crate::inbox::{self, Notice};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

/// A piece of background work, by the id it already has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Key {
    /// A background command, by its job number.
    Job(u64),
    /// A helper agent, by its agent id.
    Agent(u64),
    /// A workflow run, by its number.
    Workflow(u64),
    /// A wake-up the model asked for, by its number.
    Wake(u64),
}

/// Where a piece of work has got to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Running,
    /// A helper kept for more work, with nothing to do for now.
    Idle,
    /// Ended by itself, with a few words on how.
    Done(String),
    /// Ended in failure, with a few words on what went wrong.
    Failed(String),
    /// Ended because someone asked it to, which needs telling to nobody.
    Stopped,
}

/// A piece of background work and whom it answers to.
#[derive(Clone, Debug)]
pub struct Task {
    pub key: Key,
    /// The session or helper that hears when it ends; 0 for nobody.
    pub owner: u64,
    /// Who started it, if not its owner; once it has finished, notices meant
    /// for it go there instead of to its owner.
    pub parent: Option<u64>,
    pub label: String,
    pub state: State,
    /// Set by its first finish, so its owner is told once and only once.
    notified: bool,
}

impl Task {
    /// Work that has just started, with nobody told of it yet.
    pub fn new(key: Key, owner: u64, parent: Option<u64>, label: &str) -> Self {
        Self {
            key,
            owner,
            parent,
            label: label.to_owned(),
            state: State::Running,
            notified: false,
        }
    }
}

fn registry() -> &'static Mutex<BTreeMap<Key, Task>> {
    static TASKS: OnceLock<Mutex<BTreeMap<Key, Task>>> = OnceLock::new();
    TASKS.get_or_init(Mutex::default)
}

/// Keep track of a piece of work, in place of any earlier one with its key.
pub fn register(task: Task) {
    if let Ok(mut tasks) = registry().lock() {
        tasks.insert(task.key, task);
    }
}

/// Record how a piece of work ended and tell its owner, exactly once: a
/// second finish, such as the exit that follows a stop, changes nothing.
pub fn finish(key: &Key, state: State, notice: Option<Notice>) {
    let to = {
        let Ok(mut tasks) = registry().lock() else {
            return;
        };
        let Some(task) = tasks.get_mut(key) else {
            return;
        };
        if task.notified {
            return;
        }
        task.state = state;
        task.notified = true;
        let owner = task.owner;
        climb(&tasks, owner, |state| {
            !matches!(state, State::Running | State::Idle)
        })
    };
    let Some(notice) = notice else {
        return;
    };
    if to != 0 {
        inbox::push(to, notice);
    }
}

/// Pass on news from work that is still going to whoever hears from it now;
/// once it has finished, or been stopped, there is nothing more to tell.
pub fn tell(key: &Key, notice: Notice) {
    let to = {
        let Ok(tasks) = registry().lock() else {
            return;
        };
        let Some(task) = tasks.get(key) else {
            return;
        };
        if task.notified {
            return;
        }
        climb(&tasks, task.owner, |state| {
            !matches!(state, State::Running | State::Idle)
        })
    };
    if to != 0 {
        inbox::push(to, notice);
    }
}

/// Every piece of work a session or helper started, its helpers' work
/// included.
pub fn list(by: u64) -> Vec<Task> {
    let Ok(tasks) = registry().lock() else {
        return Vec::new();
    };
    tasks
        .values()
        .filter(|task| started_under(&tasks, task.owner, by))
        .cloned()
        .collect()
}

/// Whether work owned by `owner` is `by`'s, or that of a helper it started.
fn started_under(tasks: &BTreeMap<Key, Task>, mut owner: u64, by: u64) -> bool {
    // One step per task at most, as in `climb`.
    for _ in 0..=tasks.len() {
        if owner == by {
            return true;
        }
        let Some(helper) = tasks.get(&Key::Agent(owner)) else {
            return false;
        };
        owner = helper.parent.unwrap_or(helper.owner);
    }
    false
}

/// Hand the running work of a helper that is finishing to whoever it
/// answered to, and say which work that was.
pub fn reown(from: u64, to: u64) -> Vec<Key> {
    let Ok(mut tasks) = registry().lock() else {
        return Vec::new();
    };
    let mut handed = Vec::new();
    for task in tasks.values_mut() {
        if task.owner == from && !task.notified {
            task.owner = to;
            handed.push(task.key);
        }
    }
    handed
}

/// Where mail for `to` ends up: past every helper whose state `past` accepts,
/// to whoever started it.
fn climb(tasks: &BTreeMap<Key, Task>, mut to: u64, past: impl Fn(&State) -> bool) -> u64 {
    // One step per task at most, so owners that name each other cannot hang
    // the caller.
    for _ in 0..tasks.len() {
        match tasks.get(&Key::Agent(to)) {
            Some(helper) if past(&helper.state) => to = helper.parent.unwrap_or(helper.owner),
            _ => break,
        }
    }
    to
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox::When;

    fn exited(job: u64) -> Option<Notice> {
        Some(Notice {
            from: format!("job {job}"),
            when: When::Next,
            text: format!("job #{job} exited with code 1"),
        })
    }

    #[test]
    fn a_finished_owner_hands_the_notice_up_its_chain() {
        let _turn = inbox::tests::DOORBELL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Session 7201 started helper 7202, which started 7203, which started job 7204.
        register(Task::new(Key::Agent(7_202), 7_201, None, "explore"));
        register(Task::new(Key::Agent(7_203), 7_202, None, "explore-2"));
        register(Task::new(Key::Job(7_204), 7_203, None, "npm test"));
        assert_eq!(list(7_201).len(), 3);
        for helper in [7_203, 7_202] {
            finish(&Key::Agent(helper), State::Done("reported".into()), None);
        }
        let failed = State::Failed("exited with code 1".into());
        finish(&Key::Job(7_204), failed, exited(7_204));
        assert!(inbox::take(7_203, true).is_empty());
        assert!(inbox::take(7_202, true).is_empty());
        let heard = inbox::take(7_201, true);
        assert_eq!(heard.len(), 1);
        assert_eq!(heard[0].text, "job #7204 exited with code 1");
    }

    #[test]
    fn a_helper_lists_what_it_and_its_own_helpers_started() {
        // Session 7501 started helper 7502, which started 7503; each started a job.
        register(Task::new(Key::Agent(7_502), 7_501, None, "explore"));
        register(Task::new(Key::Agent(7_503), 7_502, None, "explore-2"));
        for (job, owner) in [(7_504, 7_501), (7_505, 7_502), (7_506, 7_503)] {
            register(Task::new(Key::Job(job), owner, None, "npm test"));
        }
        let keys = |by| list(by).iter().map(|t| t.key).collect::<Vec<_>>();
        assert_eq!(keys(7_501).len(), 5);
        let second = [Key::Job(7_505), Key::Job(7_506), Key::Agent(7_503)];
        assert_eq!(keys(7_502), second);
        assert_eq!(keys(7_503), [Key::Job(7_506)]);
    }

    #[test]
    fn reowned_work_reports_to_its_new_owner() {
        let _turn = inbox::tests::DOORBELL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        register(Task::new(Key::Job(7_301), 7_302, None, "npm run dev"));
        register(Task::new(Key::Job(7_303), 7_302, None, "npm test"));
        finish(&Key::Job(7_303), State::Done("exited".into()), None);
        reown(7_302, 7_304);
        // What had already ended stays where it ended.
        let keys = |session| list(session).iter().map(|t| t.key).collect::<Vec<_>>();
        assert_eq!(keys(7_304), [Key::Job(7_301)]);
        assert_eq!(keys(7_302), [Key::Job(7_303)]);
        let failed = State::Failed("exited with code 1".into());
        finish(&Key::Job(7_301), failed, exited(7_301));
        assert!(inbox::take(7_302, true).is_empty());
        assert_eq!(inbox::take(7_304, true).len(), 1);
    }
}
