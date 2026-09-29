//! One thread keeps every deadline in the process.
//!
//! Wake-ups, flush windows, time-to-live and heartbeats each need something
//! to happen later. A thread apiece would park a stack for every deadline,
//! and a loop that looks every so often wakes while nothing is due, so one
//! thread holds them all in deadline order and sleeps until the earliest.
//! Only a deadline sooner than the one it is waiting for rings it early.
//!
//! Callbacks run on that thread one after another, so they must be quick:
//! push onto a queue, set a flag or `try_send` on a channel, and nothing
//! more. Anything that blocks holds up every timer behind it, and a debug
//! build says so on stderr when a callback takes too long.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// How long a callback may take before a debug build reports it.
/// A push, a set or a `try_send` takes microseconds; 50 ms leaves room for a
/// busy machine and still catches one that waits on a disk, a network or a
/// lock.
const QUICK: Duration = Duration::from_millis(50);

/// A timer set with `at`, for taking it back with `cancel`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimerId(u64);

/// Run `fire` on the timer thread once `when` has come, unless it is cancelled first.
pub fn at(when: Instant, fire: Box<dyn FnOnce() + Send>) -> TimerId {
    timers().at(when, fire)
}

/// Keep a timer from firing; one that already fired or was cancelled is left alone.
pub fn cancel(id: TimerId) {
    timers().cancel(id);
}

/// The timers every caller in the process shares.
fn timers() -> &'static Timers {
    static TIMERS: OnceLock<Timers> = OnceLock::new();
    TIMERS.get_or_init(Timers::default)
}

/// Timers still to fire, and the thread that fires them.
#[derive(Default)]
struct Timers {
    queue: Mutex<Queue>,
    /// Rung when a timer becomes the earliest, so the thread waits for that
    /// one instead.
    changed: Condvar,
    /// How often the thread has woken, for a test to see it sleep while
    /// nothing is due.
    #[cfg(test)]
    wakes: std::sync::atomic::AtomicU64,
}

/// Every timer still to fire, under one lock so the deadlines and the
/// callbacks never disagree.
#[derive(Default)]
struct Queue {
    /// Deadlines with the earliest on top; the number keeps timers due at one
    /// instant in the order they were set.
    due: BinaryHeap<Reverse<(Instant, u64)>>,
    /// What each timer still to fire does, by its number.
    fire: HashMap<u64, Box<dyn FnOnce() + Send>>,
    /// The number the next timer gets.
    next: u64,
    /// Whether the thread is up; it starts with the first timer, so a process
    /// that sets none never has it.
    running: bool,
}

impl Timers {
    /// Queue `fire` for `when`, starting the thread if this is the first timer.
    fn at(&'static self, when: Instant, fire: Box<dyn FnOnce() + Send>) -> TimerId {
        let mut queue = self.lock();
        let id = queue.next;
        queue.next += 1;
        let earliest = queue
            .due
            .peek()
            .is_none_or(|Reverse((first, _))| when < *first);
        queue.due.push(Reverse((when, id)));
        queue.fire.insert(id, fire);
        if !queue.running {
            // When the thread cannot be started, the timer waits in the queue
            // and the next one set tries again.
            queue.running = std::thread::Builder::new()
                .name("timers".to_owned())
                .spawn(move || self.keep())
                .is_ok();
        }
        if earliest {
            self.changed.notify_one();
        }
        TimerId(id)
    }

    /// Drop a timer that has not fired yet.
    fn cancel(&self, TimerId(id): TimerId) {
        let mut queue = self.lock();
        let Some(fire) = queue.fire.remove(&id) else {
            return;
        };
        let earliest = queue
            .due
            .peek()
            .is_some_and(|Reverse((_, first))| *first == id);
        queue.due.retain(|Reverse((_, each))| *each != id);
        // The thread is waiting for this one: ring it now, while something is
        // happening anyway, rather than let it wake later for nothing.
        if earliest {
            self.changed.notify_one();
        }
        // Dropped unlocked, because something the callback holds may cancel or
        // set a timer as it goes, and the lock is not reentrant.
        drop(queue);
        drop(fire);
    }

    /// Fire each timer as its deadline comes, sleeping in between.
    fn keep(&self) {
        let mut queue = self.lock();
        loop {
            let Some(&Reverse((when, id))) = queue.due.peek() else {
                queue = self
                    .changed
                    .wait(queue)
                    .unwrap_or_else(PoisonError::into_inner);
                self.woke();
                continue;
            };
            let now = Instant::now();
            if when > now {
                (queue, _) = self
                    .changed
                    .wait_timeout(queue, when - now)
                    .unwrap_or_else(PoisonError::into_inner);
                self.woke();
                continue;
            }
            queue.due.pop();
            let Some(fire) = queue.fire.remove(&id) else {
                continue;
            };
            // Unlocked, so a callback can set its next timer.
            drop(queue);
            let started = Instant::now();
            // Caught, because a panic would end this thread and every timer
            // after it in the process.
            let _ = catch_unwind(AssertUnwindSafe(fire));
            // Said rather than asserted: a busy machine can hold up a quick
            // callback too, and a timer thread that stopped for it would leave
            // every later timer in the process silently unfired.
            if cfg!(debug_assertions) && started.elapsed() >= QUICK {
                eprintln!(
                    "A timer callback took {:?}; it should only push, set or try_send.",
                    started.elapsed()
                );
            }
            queue = self.lock();
        }
    }

    /// The queue, which is whole even after a panic because callbacks run
    /// without it.
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Count a wake-up of the thread, in tests.
    fn woke(&self) {
        #[cfg(test)]
        self.wakes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc;

    /// A callback that sends `label` down `sent`.
    fn say(sent: &mpsc::Sender<&'static str>, label: &'static str) -> Box<dyn FnOnce() + Send> {
        let sent = sent.clone();
        Box::new(move || {
            let _ = sent.send(label);
        })
    }

    /// Timers of the test's own, so no other test's deadline can wake their
    /// thread and hide one that failed to.
    fn own() -> &'static Timers {
        Box::leak(Box::default())
    }

    #[test]
    fn timers_fire_in_deadline_order() {
        let (sent, fired) = mpsc::channel();
        let start = Instant::now() + Duration::from_millis(100);
        // Set out of order, and the last two for the same instant.
        for (label, after) in [("third", 60), ("first", 0), ("second", 30), ("fourth", 60)] {
            at(start + Duration::from_millis(after), say(&sent, label));
        }
        let order: Vec<_> = (0..4)
            .map(|_| fired.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        assert_eq!(order, ["first", "second", "third", "fourth"]);
    }

    #[test]
    fn a_cancelled_timer_never_fires() {
        let (sent, fired) = mpsc::channel();
        let now = Instant::now();
        let cancelled = at(now + Duration::from_millis(300), say(&sent, "cancelled"));
        at(now + Duration::from_millis(600), say(&sent, "kept"));
        cancel(cancelled);
        assert_eq!(fired.recv_timeout(Duration::from_secs(5)), Ok("kept"));
    }

    #[test]
    fn cancelling_a_timer_drops_its_callback_outside_the_lock() {
        /// Cancels a timer when dropped, as the last handle to a session might.
        struct Guard(&'static Timers, TimerId);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.cancel(self.1);
            }
        }
        let timers = own();
        let later = Instant::now() + Duration::from_secs(3600);
        let guarded = timers.at(later, Box::new(|| {}));
        let guard = Guard(timers, guarded);
        let holder = timers.at(later, Box::new(move || drop(guard)));
        let (done, returned) = mpsc::channel();
        // On a thread of its own, so a cancel that locks twice fails the test
        // rather than hanging it.
        std::thread::spawn(move || {
            timers.cancel(holder);
            let _ = done.send(());
        });
        assert_eq!(returned.recv_timeout(Duration::from_secs(5)), Ok(()));
        assert!(timers.lock().fire.is_empty());
    }

    #[test]
    fn an_earlier_timer_set_while_waiting_fires_first() {
        let timers = own();
        let (sent, fired) = mpsc::channel();
        let now = Instant::now();
        timers.at(now + Duration::from_secs(3600), say(&sent, "late"));
        // Long enough for the thread to settle into waiting for the late one.
        std::thread::sleep(Duration::from_millis(100));
        let due = Instant::now() + Duration::from_millis(50);
        timers.at(due, say(&sent, "early"));
        assert_eq!(fired.recv_timeout(Duration::from_secs(5)), Ok("early"));
        assert!(Instant::now() >= due);
    }

    #[test]
    fn nothing_wakes_while_nothing_is_due() {
        let timers = own();
        let (sent, fired) = mpsc::channel();
        let now = Instant::now();
        timers.at(now + Duration::from_secs(3600), say(&sent, "late"));
        timers.at(now + Duration::from_millis(20), say(&sent, "soon"));
        assert_eq!(fired.recv_timeout(Duration::from_secs(5)), Ok("soon"));
        // The thread only counts a wake-up once a wait ends, so after the
        // callback it has none pending until the late one is due.
        let woken = timers.wakes.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_secs(1));
        assert_eq!(timers.wakes.load(Ordering::Relaxed), woken);
    }
}
