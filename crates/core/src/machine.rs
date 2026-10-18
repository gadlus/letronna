//! The CPUs commands here can use, where that is fewer than the machine has.
//!
//! A container held to fewer CPUs than its host -- Docker's `--cpus`, a
//! Kubernetes limit, a CI runner -- still shows every CPU to `nproc` and
//! `os.cpu_count()`, so `make -j`, `xargs -P` and the thread pools of OpenMP
//! and BLAS start a worker for each. On the quota those workers take turns,
//! and OpenMP's spin while they wait for each other: a step that took a
//! second and a half alone took over a minute. The standard library's count
//! honours the quota and the affinity mask, and the online count is what
//! programs see, so where the two differ the system prompt says so and
//! commands start with their thread pools sized to the quota.

use std::ffi::OsString;
#[cfg(not(test))]
use std::sync::OnceLock;

/// Thread-pool sizes a command starts with where it gets fewer CPUs than
/// the machine has: OpenMP's, and those of OpenBLAS and MKL, which read
/// their own before OpenMP's.
pub(crate) const THREAD_VARS: [&str; 4] = [
    "OMP_NUM_THREADS",
    "OMP_THREAD_LIMIT",
    "OPENBLAS_NUM_THREADS",
    "MKL_NUM_THREADS",
];
/// Sizes of other pools the four would cap or stand in for, so a user who
/// set one has chosen too.
const ALSO_CHOSEN: [&str; 3] = [
    "GOTO_NUM_THREADS",
    "BLIS_NUM_THREADS",
    "NUMEXPR_NUM_THREADS",
];

/// The CPUs commands get, and the CPUs the machine has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cpus {
    /// What the quota and the affinity mask leave commands.
    pub(crate) usable: usize,
    /// The machine's CPUs as programs count them. With glibc these are the
    /// online ones; musl counts the affinity mask, which programs keep to
    /// already, so there only a quota tells the two apart.
    pub(crate) online: usize,
    /// Whether commands start with `THREAD_VARS` set to `usable`: not when
    /// the user chose a size already.
    pub(crate) pools: bool,
}

/// The CPUs, when commands here get fewer than the machine has. Measured
/// once, so the system prompt says the same for the whole run and its
/// cached prefix holds, and commands start as it says.
#[cfg(not(test))]
pub(crate) fn cpus() -> Option<Cpus> {
    static CPUS: OnceLock<Option<Cpus>> = OnceLock::new();
    *CPUS.get_or_init(measure)
}

/// Count the CPUs, and log what was found, so a run's log shows whether
/// its prompt and commands were told of a limit.
fn measure() -> Option<Cpus> {
    let usable = std::thread::available_parallelism().ok().map(|n| n.get());
    let online = online();
    // The login shell's variables only where there is a limit, as reading
    // them can take a second or two.
    let cpus = fewer(usable, online).map(|cpus| Cpus {
        pools: !chosen(|name| std::env::var_os(name), crate::login_env::vars()),
        ..cpus
    });
    let threads = cpus.filter(|cpus| cpus.pools).map(|cpus| cpus.usable);
    crate::diag::event(
        "machine",
        &[
            ("usable", usable.into()),
            ("online", online.into()),
            ("threads", threads.into()),
        ],
    );
    cpus
}

#[cfg(test)]
thread_local! {
    /// The CPUs a test gives; none otherwise, so no test's prompt or
    /// commands depend on the machine that runs it.
    static GIVEN: std::cell::Cell<Option<Cpus>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn cpus() -> Option<Cpus> {
    GIVEN.get()
}

/// Makes `cpus()` give `cpus` on this thread until the guard is dropped.
#[cfg(test)]
pub(crate) fn give(cpus: Cpus) -> impl Drop {
    struct Given;
    impl Drop for Given {
        fn drop(&mut self) {
            GIVEN.set(None);
        }
    }
    GIVEN.set(Some(cpus));
    Given
}

/// `usable` and `online` as `Cpus` when fewer are usable than are online,
/// with thread pools sized to them.
fn fewer(usable: Option<usize>, online: Option<usize>) -> Option<Cpus> {
    let (usable, online) = (usable?, online?);
    (usable < online).then_some(Cpus {
        usable,
        online,
        pools: true,
    })
}

/// Whether Letronna's own environment, read by `own`, or the login shell's
/// variables `login` size a thread pool already. Any one is the user's
/// choice, and each of `THREAD_VARS` caps or stands in for another, so
/// then none is set.
fn chosen(own: impl Fn(&str) -> Option<OsString>, login: &[(OsString, OsString)]) -> bool {
    THREAD_VARS
        .iter()
        .chain(&ALSO_CHOSEN)
        .any(|name| own(name).is_some() || login.iter().any(|(set, _)| set == name))
}

#[cfg(unix)]
fn online() -> Option<usize> {
    // SAFETY: sysconf only reads a value of the system's.
    let online = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    usize::try_from(online).ok().filter(|&online| online > 0)
}

#[cfg(not(unix))]
fn online() -> Option<usize> {
    None
}

/// The sentence the system prompt adds about `cpus`. It names the
/// variables commands start with, as OMP_THREAD_LIMIT caps even a count a
/// command sets itself, so a run that needs more threads must unset it.
pub(crate) fn line(cpus: Cpus) -> String {
    let unit = if cpus.usable == 1 { "CPU" } else { "CPUs" };
    let pools = if cpus.pools {
        format!(
            ", and start with OMP_THREAD_LIMIT, OMP_NUM_THREADS, \
             OPENBLAS_NUM_THREADS and MKL_NUM_THREADS set to {}",
            cpus.usable
        )
    } else {
        String::new()
    };
    format!(
        " Commands here get about {} {unit}, though the machine has {}{pools}; size \
         parallel jobs and thread counts to that.",
        cpus.usable, cpus.online
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_fewer_cpus_than_the_machine_has_are_worth_a_word() {
        assert_eq!(fewer(Some(4), Some(4)), None);
        assert_eq!(
            fewer(Some(1), Some(4)),
            Some(Cpus {
                usable: 1,
                online: 4,
                pools: true,
            })
        );
        // A count that cannot be read says nothing either way.
        assert_eq!(fewer(None, Some(4)), None);
        assert_eq!(fewer(Some(1), None), None);
    }

    #[test]
    fn the_counts_are_this_machines_and_said_only_when_fewer_are_usable() {
        #[cfg(unix)]
        assert!(online().is_some_and(|online| online > 0));
        let usable = std::thread::available_parallelism().unwrap().get();
        match measure() {
            Some(cpus) => assert!(cpus.usable == usable && usable < cpus.online),
            None => assert!(online().is_none_or(|online| usable >= online)),
        }
    }

    #[test]
    fn a_thread_pool_size_set_anywhere_is_the_users_choice() {
        let none = |_: &str| None;
        assert!(!chosen(none, &[]));
        // In Letronna's own environment, or in the login shell's.
        let own = |name: &str| (name == "MKL_NUM_THREADS").then(|| "2".into());
        assert!(chosen(own, &[]));
        assert!(chosen(none, &[("OMP_NUM_THREADS".into(), "8".into())]));
        // A pool the four would cap counts too; other variables do not.
        assert!(chosen(none, &[("BLIS_NUM_THREADS".into(), "4".into())]));
        assert!(!chosen(none, &[("PATH".into(), "/bin".into())]));
    }

    #[test]
    fn the_line_says_what_commands_get_and_start_with() {
        let one = Cpus {
            usable: 1,
            online: 4,
            pools: true,
        };
        assert_eq!(
            line(one),
            " Commands here get about 1 CPU, though the machine has 4, and start \
             with OMP_THREAD_LIMIT, OMP_NUM_THREADS, OPENBLAS_NUM_THREADS and \
             MKL_NUM_THREADS set to 1; size parallel jobs and thread counts to that."
        );
        // Where the user chose a size, the pools are as they set them.
        let chosen = line(Cpus {
            pools: false,
            ..one
        });
        assert_eq!(
            chosen,
            " Commands here get about 1 CPU, though the machine has 4; size \
             parallel jobs and thread counts to that."
        );
        let two = line(Cpus {
            usable: 2,
            online: 8,
            pools: true,
        });
        assert!(
            two.contains(" about 2 CPUs, though the machine has 8, and start with "),
            "{two}"
        );
        assert!(two.contains(" MKL_NUM_THREADS set to 2; "), "{two}");
    }
}
