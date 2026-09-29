//! Stop tokens that reach everything below them.
//!
//! A `Cancel` goes down with a piece of work, and whatever that work blocks
//! on listens to it, so a stop is heard at once rather than when a sleep or a
//! timeout runs out. Tokens form a tree: `child` gives part of the work a
//! token of its own, which stops with its parent or on its own, so one stop
//! reaches everything below it and nothing beside it.
//!
//! Nothing here polls. `sleep` waits on a condvar that `set` wakes. A wait on
//! a channel registers a waker with `on_set` that closes the channel or sends
//! it a last value, because std has no `select` to wait on a channel and a
//! token at once.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

/// A stop signal that every clone shares.
#[derive(Clone, Default)]
pub struct Cancel(Arc<Node>);

#[derive(Default)]
struct Node {
    /// Kept outside the lock so a loop can check it on every step for free.
    stopped: AtomicBool,
    inner: Mutex<Inner>,
    woken: Condvar,
}

#[derive(Default)]
struct Inner {
    /// Numbers the wakers so a guard can take back its own.
    next: u64,
    wakers: Vec<(u64, Box<dyn FnOnce() + Send>)>,
    /// Weak, so a child's token goes away with the work that held it.
    children: Vec<Weak<Node>>,
}

impl Node {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        // Nothing that can panic runs under the lock, so a poisoned one is
        // still whole.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Takes its waker back when dropped, so a finished wait leaves nothing to run.
#[must_use = "dropping the guard takes the waker back at once"]
pub struct Waker {
    node: Weak<Node>,
    id: u64,
}

impl Drop for Waker {
    fn drop(&mut self) {
        let Some(node) = self.node.upgrade() else {
            return;
        };
        let mut inner = node.lock();
        let Some(at) = inner.wakers.iter().position(|(id, _)| *id == self.id) else {
            return;
        };
        let wake = inner.wakers.remove(at);
        // What the waker holds is dropped outside the lock, in case it holds
        // another guard on this token.
        drop(inner);
        drop(wake);
    }
}

impl Cancel {
    /// Whether the token has been set.
    pub fn is_set(&self) -> bool {
        self.0.stopped.load(Ordering::Acquire)
    }

    /// Stops the token: sleepers wake, wakers run, then children are set.
    pub fn set(&self) {
        let mut inner = self.0.lock();
        if self.0.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        let wakers = std::mem::take(&mut inner.wakers);
        let children = std::mem::take(&mut inner.children);
        // Wakers run outside the lock, so one may use this token itself.
        drop(inner);
        self.0.woken.notify_all();
        for (_, wake) in wakers {
            wake();
        }
        for child in children.iter().filter_map(Weak::upgrade) {
            Cancel(child).set();
        }
    }

    /// A token that stops with this one, or alone without stopping this one.
    pub fn child(&self) -> Cancel {
        let child = Cancel::default();
        let mut inner = self.0.lock();
        if self.is_set() {
            child.set();
            return child;
        }
        // A long-lived token hands out a child per call; dropping the dead
        // ones keeps the list from growing with every call.
        inner.children.retain(|c| c.strong_count() > 0);
        inner.children.push(Arc::downgrade(&child.0));
        child
    }

    /// Runs `wake` once when the token is set, or now if it already is.
    pub fn on_set(&self, wake: impl FnOnce() + Send + 'static) -> Waker {
        let mut inner = self.0.lock();
        if self.is_set() {
            drop(inner);
            wake();
            return Waker {
                node: Weak::new(),
                id: 0,
            };
        }
        inner.next += 1;
        let id = inner.next;
        inner.wakers.push((id, Box::new(wake)));
        Waker {
            node: Arc::downgrade(&self.0),
            id,
        }
    }

    /// Sleeps for `d` unless set first; false when the sleep was cut short.
    pub fn sleep(&self, d: Duration) -> bool {
        let started = Instant::now();
        let mut guard = self.0.lock();
        loop {
            if self.is_set() {
                return false;
            }
            let left = d.saturating_sub(started.elapsed());
            if left.is_zero() {
                return true;
            }
            guard = self
                .0
                .woken
                .wait_timeout(guard, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn counter(ran: &Arc<AtomicUsize>) -> impl FnOnce() + Send + 'static {
        let ran = ran.clone();
        move || {
            ran.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn a_cancelled_sleep_returns_early() {
        assert!(Cancel::default().sleep(Duration::from_millis(10)));
        let cancel = Cancel::default();
        let stop = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            stop.set();
        });
        let started = Instant::now();
        assert!(!cancel.sleep(Duration::from_secs(60)));
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "slept {:?}",
            started.elapsed()
        );
        // Once set, it does not sleep at all.
        assert!(!cancel.sleep(Duration::from_secs(60)));
    }

    #[test]
    fn stopping_a_parent_stops_its_children() {
        let parent = Cancel::default();
        let child = parent.child();
        let grandchild = child.child();
        let sibling = parent.child();
        sibling.set();
        assert!(
            !parent.is_set() && !child.is_set(),
            "a child stopped more than itself"
        );
        parent.set();
        assert!(child.is_set() && grandchild.is_set());
        assert!(parent.child().is_set(), "a child of a stopped token ran");
    }

    #[test]
    fn a_waker_added_after_stop_runs_at_once() {
        let ran = Arc::new(AtomicUsize::new(0));
        let cancel = Cancel::default();
        let _kept = cancel.on_set(counter(&ran));
        // Taken back before the stop, so it never runs.
        drop(cancel.on_set(counter(&ran)));
        cancel.set();
        assert_eq!(ran.load(Ordering::SeqCst), 1);
        let _late = cancel.on_set(counter(&ran));
        assert_eq!(ran.load(Ordering::SeqCst), 2);
    }
}
