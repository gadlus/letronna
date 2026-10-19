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

/// Set `cancel` at the first Ctrl-C, SIGTERM or hangup instead of ending the
/// process, so a run can end what it started before it goes; a second Ctrl-C
/// or SIGTERM ends the process at once, as the first did before.
#[cfg(unix)]
pub fn on_interrupt(cancel: Cancel) {
    use std::io::Read;
    use std::os::fd::IntoRawFd;
    static HEARD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
    // A signal handler may not lock or allocate, so it only wakes the thread
    // below, which sets the token.
    extern "C" fn heard(_: libc::c_int) {
        // SAFETY: write is async-signal-safe, the byte is on this stack, and
        // the pipe stays open for the life of the process.
        unsafe { libc::write(HEARD.load(Ordering::Relaxed), [1u8].as_ptr().cast(), 1) };
    }
    // Close-on-exec, so no command Letronna starts holds the pipe.
    let Ok((mut reader, writer)) = std::io::pipe() else {
        return;
    };
    HEARD.store(writer.into_raw_fd(), Ordering::Relaxed);
    // Whether a second of each ends the process: a second Ctrl-C or SIGTERM
    // means now, but a closing terminal sends two hangups, one from its
    // shell and one from the kernel as the shell exits, and the second
    // would end the stop the first began.
    for (signal, once) in [
        (libc::SIGINT, true),
        (libc::SIGTERM, true),
        (libc::SIGHUP, false),
    ] {
        // SAFETY: all zeroes is a valid sigaction, for the current one to be
        // read into and for the new one, whose mask is emptied below.
        let (mut before, mut action): (libc::sigaction, libc::sigaction) =
            unsafe { (std::mem::zeroed(), std::mem::zeroed()) };
        // SAFETY: this only reads the current action.
        unsafe { libc::sigaction(signal, std::ptr::null(), &mut before) };
        // Ignored on purpose, as a shell does with Ctrl-C for what it runs in
        // the background of a script, where it is meant for the script alone,
        // or nohup with a hangup.
        if before.sa_sigaction == libc::SIG_IGN {
            continue;
        }
        action.sa_sigaction = heard as *const () as libc::sighandler_t;
        // With `once`, heard once and then back to the default, which ends
        // the process; and a call another thread was in carries on rather
        // than fail.
        action.sa_flags = libc::SA_RESTART | if once { libc::SA_RESETHAND } else { 0 };
        // SAFETY: the mask is the action's own, and the action is whole when
        // set.
        unsafe {
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
    std::thread::spawn(move || {
        // Read on after the first, so hangups that keep coming never fill
        // the pipe and block the handler.
        while reader.read(&mut [0]).is_ok_and(|read| read == 1) {
            cancel.set();
        }
    });
}

/// Set `cancel` at the first Ctrl-C instead of ending the process, so a run
/// can end what it started before it goes; a second Ctrl-C ends the process
/// at once, as the first did before.
#[cfg(windows)]
pub fn on_interrupt(cancel: Cancel) {
    use windows_sys::Win32::Foundation::{BOOL, FALSE, TRUE};
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler,
    };
    static HEARD: std::sync::OnceLock<Cancel> = std::sync::OnceLock::new();
    static ONCE: AtomicBool = AtomicBool::new(false);
    // Windows calls this on a thread of its own, where setting a token is
    // safe. FALSE passes the event on to the default, which ends the process.
    unsafe extern "system" fn heard(event: u32) -> BOOL {
        if !matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT) || ONCE.swap(true, Ordering::Relaxed) {
            return FALSE;
        }
        if let Some(cancel) = HEARD.get() {
            cancel.set();
        }
        TRUE
    }
    if HEARD.set(cancel).is_err() {
        return;
    }
    // SAFETY: the handler is a plain function that lives as long as the process.
    unsafe { SetConsoleCtrlHandler(Some(heard), TRUE) };
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Held by every test that changes what a signal does to this process:
    /// that is one setting for the whole process, and tests run side by side.
    #[cfg(unix)]
    pub(crate) static SIGNALS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// What `run` gives with Ctrl-C and Ctrl-\ ignored by this process, as a
    /// script's `&` starts a program.
    #[cfg(unix)]
    pub(crate) fn ignoring_interrupts<T>(run: impl FnOnce() -> T) -> T {
        /// What the two signals did before, set back when this is dropped,
        /// so a `run` that panics leaves the other tests as they were.
        struct Was([(libc::c_int, libc::sighandler_t); 2]);
        impl Drop for Was {
            fn drop(&mut self) {
                for (signal, was) in self.0 {
                    // SAFETY: as where it was taken.
                    unsafe { libc::signal(signal, was) };
                }
            }
        }
        let _signals = SIGNALS.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: these only change what the two signals do to this test
        // process, and every other test that does so waits for the lock.
        let _was = Was([libc::SIGINT, libc::SIGQUIT]
            .map(|signal| (signal, unsafe { libc::signal(signal, libc::SIG_IGN) })));
        run()
    }

    /// A shell line that sends itself Ctrl-\ and then Ctrl-C, which prints
    /// `quit` and ends at the Ctrl-C where both are at their defaults, and
    /// prints `survived` where they are ignored. A shell cannot trap a signal
    /// it started with ignored, and bash ignores Ctrl-\ itself, so that one
    /// is caught to show it was not.
    #[cfg(unix)]
    pub(crate) const INTERRUPTS_ITSELF: &str =
        "trap 'echo quit' QUIT; kill -QUIT $$; kill -INT $$; echo survived";

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

    /// What `signal` does to this process now.
    #[cfg(unix)]
    fn action(signal: libc::c_int) -> libc::sighandler_t {
        // SAFETY: all zeroes is a valid sigaction to read the current one into.
        let mut now: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: this only reads the current action.
        unsafe { libc::sigaction(signal, std::ptr::null(), &mut now) };
        now.sa_sigaction
    }

    // One test, since what a signal does is the whole process's.
    #[cfg(unix)]
    #[test]
    fn a_ctrl_c_sigterm_or_hangup_sets_the_token_unless_it_was_ignored() {
        let _signals = SIGNALS.lock().unwrap_or_else(|e| e.into_inner());
        let signals = [
            ("Ctrl-C", libc::SIGINT, true),
            ("SIGTERM", libc::SIGTERM, true),
            ("hangup", libc::SIGHUP, false),
        ];
        for (name, signal, second_ends) in signals {
            let ignored = Cancel::default();
            // SAFETY: these only change what `signal` does to this test
            // process.
            unsafe { libc::signal(signal, libc::SIG_IGN) };
            on_interrupt(ignored.clone());
            assert_eq!(action(signal), libc::SIG_IGN, "an ignored {name} was taken");
            // SAFETY: as above.
            unsafe { libc::signal(signal, libc::SIG_DFL) };

            let cancel = Cancel::default();
            on_interrupt(cancel.clone());
            assert_ne!(action(signal), libc::SIG_DFL, "the {name} was not taken");
            // SAFETY: `signal` now goes to the handler just set.
            unsafe { libc::raise(signal) };
            assert!(
                !cancel.sleep(Duration::from_secs(10)),
                "the {name} was not heard"
            );
            assert!(!ignored.is_set());
            // A second Ctrl-C or SIGTERM ends the process, as the first
            // always did, while a second hangup is heard and passed by.
            assert_eq!(action(signal) == libc::SIG_DFL, second_ends, "{name}");
        }
        // SAFETY: the hangup still goes to the handler, as just checked.
        unsafe { libc::raise(libc::SIGHUP) };
        for (_, signal, _) in signals {
            // SAFETY: this leaves each at its default for the tests after.
            unsafe { libc::signal(signal, libc::SIG_DFL) };
        }
    }
}
