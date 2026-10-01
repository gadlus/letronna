//! Letronna kept alive through Ctrl-C while a program it started has the
//! terminal, as system(3) keeps its caller alive. The terminal is cooked
//! while it is lent out, so Ctrl-C is no longer read as a key: it goes to
//! every process on the terminal, Letronna included, whose default is to
//! end there.
//!
//! A handler that does nothing is set rather than ignoring the signal. An
//! ignored signal stays ignored in a program started meanwhile, while a
//! handled one goes back to its default there, so the program itself still
//! stops on Ctrl-C. On Windows a console handler that says the event was
//! handled does the same, and handlers are not inherited either.

/// Ctrl-C, and Ctrl-\ on Unix, pass Letronna by while this is held, and are
/// handled as before once it is dropped.
pub struct Held {
    #[cfg(unix)]
    was: [libc::sigaction; 2],
}

#[cfg(unix)]
const SIGNALS: [libc::c_int; 2] = [libc::SIGINT, libc::SIGQUIT];

#[cfg(unix)]
extern "C" fn nothing(_: libc::c_int) {}

#[cfg(windows)]
unsafe extern "system" fn handled(_: u32) -> windows_sys::Win32::Foundation::BOOL {
    1
}

impl Held {
    /// Pass Ctrl-C by until this is dropped.
    #[cfg(unix)]
    pub fn take() -> Self {
        // SAFETY: sigaction is given valid structs; a zeroed sigaction is a
        // valid one, and the handler touches nothing.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = nothing as extern "C" fn(libc::c_int) as libc::sighandler_t;
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            let mut was: [libc::sigaction; 2] = std::mem::zeroed();
            for (signal, was) in SIGNALS.into_iter().zip(was.iter_mut()) {
                libc::sigaction(signal, &action, was);
            }
            Self { was }
        }
    }

    /// Pass Ctrl-C by until this is dropped.
    #[cfg(windows)]
    pub fn take() -> Self {
        // SAFETY: the handler is a plain function that lives for the program.
        unsafe {
            windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(handled), 1);
        }
        Self {}
    }
}

impl Drop for Held {
    #[cfg(unix)]
    fn drop(&mut self) {
        for (signal, was) in SIGNALS.into_iter().zip(self.was.iter()) {
            // SAFETY: `was` is what sigaction filled in when this was taken.
            unsafe { libc::sigaction(signal, was, std::ptr::null_mut()) };
        }
    }

    #[cfg(windows)]
    fn drop(&mut self) {
        // SAFETY: removes the handler `take` added.
        unsafe {
            windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(handled), 0);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn ctrl_c_passes_by_while_held_and_is_handled_as_before_after() {
        let before = unsafe { libc::signal(libc::SIGQUIT, libc::SIG_DFL) };
        {
            let _held = Held::take();
            // A process that raises SIGQUIT on itself would end without the
            // handler; with it, the test goes on.
            unsafe { libc::raise(libc::SIGQUIT) };
        }
        let after = unsafe { libc::signal(libc::SIGQUIT, before) };
        assert_eq!(after, libc::SIG_DFL);
    }
}
