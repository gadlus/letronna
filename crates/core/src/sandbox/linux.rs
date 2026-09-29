//! The sandbox on Linux: Landlock for files, a seccomp filter for the
//! network, both set in the child between fork and exec so that Letronna
//! itself is never confined.
//!
//! Landlock only ever allows: a rule grants rights beneath a folder, and
//! nothing can take them back further down. So a folder is hidden by
//! granting reads on everything around it instead, down the path from `/`,
//! and a key file inside the project cannot be hidden at all, since the
//! command must read back what it writes there. The gate already sends a
//! command that names a `.env` file to a card. Listing a folder is granted
//! everywhere, so the names in a hidden folder show though nothing in it
//! reads; otherwise `/`, the home folder and every folder on the way to a
//! hidden one could not be listed at all.
//!
//! What each rule needs of the kernel's Landlock ABI: reading, writing and
//! making files is ABI 1 (Linux 5.13), moving a file into another folder
//! ABI 2, and truncating one ABI 3 (Linux 6.2). Before ABI 3 truncating is
//! not held anywhere, so on an older kernel no command is confined and the
//! gate asks as before. Ioctls on devices are held from ABI 5 and signals
//! to processes outside the sandbox from ABI 6; on an older kernel a
//! confined command may still signal the user's other processes. Whatever
//! the ABI, it runs in a session of its own, without a terminal to push
//! keys into.
//!
//! The network is a seccomp filter, not Landlock's own rules: those need
//! ABI 4 (Linux 6.7) and hold only TCP, so DNS and any other UDP would get
//! out. The filter refuses creating any socket at all, a local one too,
//! since Docker's socket and the session bus run what they are asked
//! outside the sandbox. A pair of connected sockets stays allowed, as
//! Python and Node use one to talk to themselves.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::path::{Path, PathBuf};

/// The folders to grant reading beneath so that a command reads everything
/// under `root` but `hidden`: `root` itself when nothing hidden lies in it,
/// otherwise what `children` lists in it, taken the same way. Each hidden
/// path is left out whole, and so is everything a listing does not give.
fn readable(
    root: &Path,
    hidden: &[PathBuf],
    children: &dyn Fn(&Path) -> Vec<PathBuf>,
) -> Vec<PathBuf> {
    if hidden.iter().any(|path| path == root) {
        return Vec::new();
    }
    if !hidden.iter().any(|path| path.starts_with(root)) {
        return vec![root.to_path_buf()];
    }
    children(root)
        .iter()
        .flat_map(|child| readable(child, hidden, children))
        .collect()
}

/// What `dir` holds, less its links: a rule on a link would grant whatever
/// it points at, a hidden folder too, while a link into a readable folder
/// still works, as Landlock checks where it leads.
fn listing(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| !kind.is_symlink()))
        .map(|entry| entry.path())
        .collect()
}

/// The keys `home` holds that read_file guards, with every link followed:
/// the names in `SECRETS` and the `.env` files at its top.
fn guarded_at_home(home: &Path) -> Vec<PathBuf> {
    let listed = crate::tools::SECRETS.iter().map(|entry| home.join(entry));
    let env = std::fs::read_dir(home)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| crate::tools::secret(Path::new(name)))
        });
    let mut found: Vec<PathBuf> = listed
        .chain(env)
        .filter_map(|path| std::fs::canonicalize(path).ok())
        .collect();
    // `.ssh` and the like are both listed and found at the top.
    found.sort();
    found.dedup();
    found
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub(super) use kernel::here;

/// No backend on a processor the network filter does not know.
#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
pub(super) fn here() -> Option<&'static dyn super::Sandbox> {
    None
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod kernel {
    use super::super::{Policy, Sandbox};
    use landlock::{
        ABI, Access, AccessFs, CompatLevel, Compatible, RulesetAttr, RulesetCreated,
        RulesetCreatedAttr, RulesetStatus, Scope, path_beneath_rules,
    };
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::OnceLock;

    /// The newest ABI whose rights are asked for; a kernel with less keeps
    /// what it has of them, down to `NEEDED`.
    const LATEST: ABI = ABI::V9;

    /// The oldest ABI that holds every write to the project, truncating
    /// included; on a kernel older than this, commands ask.
    const NEEDED: ABI = ABI::V3;

    /// The kernel's name for the processor the filter was written for, which
    /// every system call it sees carries.
    #[cfg(target_arch = "x86_64")]
    const AUDIT_ARCH: u32 = 0xC000_003E;
    #[cfg(target_arch = "aarch64")]
    const AUDIT_ARCH: u32 = 0xC000_00B7;

    /// Where x32's system call numbers start, which an x86-64 process can
    /// also make, so a socket there cannot slip past under another number.
    const X32_SYSCALL_BIT: u32 = 0x4000_0000;

    /// Landlock and seccomp, or why they cannot run here.
    pub(super) struct Landlock {
        /// Tries confining a command, and says why it failed.
        probe: fn() -> Result<(), String>,
        /// What `probe` said, asked once.
        tried: OnceLock<Result<(), String>>,
    }

    static LANDLOCK: Landlock = Landlock {
        probe: confine_true,
        tried: OnceLock::new(),
    };

    /// Landlock, which says itself whether this kernel has it.
    pub(in super::super) fn here() -> Option<&'static dyn Sandbox> {
        Some(&LANDLOCK)
    }

    impl Landlock {
        /// Why commands cannot be confined here, tried once; None when they can.
        fn missing(&self) -> Option<&str> {
            self.tried
                .get_or_init(self.probe)
                .as_ref()
                .err()
                .map(String::as_str)
        }
    }

    impl Sandbox for Landlock {
        fn available(&self) -> bool {
            self.missing().is_none()
        }

        fn wrap(&self, command: &Command, policy: &Policy) -> Result<Command, String> {
            if let Some(why) = self.missing() {
                return Err(fallback(why));
            }
            let mut hidden = policy.hidden.clone();
            if let Some(home) = crate::tools::home() {
                hidden.extend(super::guarded_at_home(&home));
            }
            confine(command, policy, &hidden)
        }
    }

    /// What a command that should have run confined says when it cannot,
    /// as the gate asks instead: so must the model.
    fn fallback(why: &str) -> String {
        format!(
            "Commands cannot be sandboxed on this system ({why}), so this one did not \
             run. Call run_command again with unsandboxed: true, which needs the \
             user's approval."
        )
    }

    /// Runs a shell that does nothing, confined, as a kernel without
    /// Landlock, one older than `NEEDED`, a container that turns it off, or
    /// one that refuses a seccomp filter all show only when tried.
    fn confine_true() -> Result<(), String> {
        let policy = Policy {
            writable: Vec::new(),
            temp: std::env::temp_dir(),
            hidden: Vec::new(),
        };
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 0"]);
        let status = confine(&command, &policy, &[])?
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| format!("a confined command could not start: {e}"))?;
        if !status.success() {
            return Err("a confined command failed to run".into());
        }
        Ok(())
    }

    /// What to spawn in place of `command` so that it runs held to `policy`,
    /// with `hidden` neither read nor written.
    fn confine(command: &Command, policy: &Policy, hidden: &[PathBuf]) -> Result<Command, String> {
        let mut ruleset = Some(ruleset(policy, hidden)?);
        let filter = no_sockets();
        let mut wrapped = Command::new(command.get_program());
        wrapped.args(command.get_args());
        for (key, value) in command.get_envs() {
            match value {
                Some(value) => wrapped.env(key, value),
                None => wrapped.env_remove(key),
            };
        }
        wrapped.env("TMPDIR", &policy.temp);
        if let Some(dir) = command.get_current_dir() {
            wrapped.current_dir(dir);
        }
        // Between fork and exec nothing may allocate, as another thread may
        // have held the allocator's lock when the process was copied; the
        // rules and the filter are built above, and here only handed over.
        let hold = move || {
            // Its own session leaves it without a terminal, whose keys it
            // could otherwise push into the shell that started Letronna.
            if unsafe { libc::setsid() } == -1 {
                return Err(std::io::Error::last_os_error());
            }
            let refused = std::io::Error::from_raw_os_error(libc::EPERM);
            let Some(ruleset) = ruleset.take() else {
                return Err(refused);
            };
            match ruleset.restrict_self() {
                Ok(status) if status.ruleset != RulesetStatus::NotEnforced => {}
                _ => return Err(refused),
            }
            let program = libc::sock_fprog {
                len: filter.len() as u16,
                filter: filter.as_ptr().cast_mut(),
            };
            // Landlock has already set no_new_privs, which a filter needs.
            let set = unsafe {
                libc::prctl(
                    libc::PR_SET_SECCOMP,
                    libc::SECCOMP_MODE_FILTER,
                    &raw const program,
                )
            };
            if set != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        };
        unsafe { wrapped.pre_exec(hold) };
        Ok(wrapped)
    }

    /// The Landlock rules for `policy`: listing folders anywhere, reading
    /// everywhere but `hidden`, everything in the folders it may write, and
    /// `/dev/null`, which scripts write to everywhere.
    fn ruleset(policy: &Policy, hidden: &[PathBuf]) -> Result<RulesetCreated, String> {
        let failed = |e: landlock::RulesetError| format!("Landlock refused the rules: {e}");
        let reads = super::readable(Path::new("/"), hidden, &super::listing);
        landlock::Ruleset::default()
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(AccessFs::from_all(NEEDED))
            .map_err(|e| format!("Landlock ABI 3 is not available: {e}"))?
            .set_compatibility(CompatLevel::BestEffort)
            .handle_access(AccessFs::from_all(LATEST))
            .and_then(|ruleset| ruleset.scope(Scope::Signal))
            .and_then(|ruleset| ruleset.create())
            .and_then(|ruleset| ruleset.add_rules(path_beneath_rules(["/"], AccessFs::ReadDir)))
            .and_then(|ruleset| {
                ruleset.add_rules(path_beneath_rules(reads, AccessFs::from_read(LATEST)))
            })
            .and_then(|ruleset| {
                let writable = policy.writable.iter().map(PathBuf::as_path);
                let writable = writable.chain([Path::new("/dev/null")]);
                ruleset.add_rules(path_beneath_rules(writable, AccessFs::from_all(LATEST)))
            })
            .map_err(failed)
    }

    /// A seccomp filter that refuses, with EPERM, creating a socket or an
    /// io_uring (which can make one where no filter sees it), and every
    /// system call made as another processor.
    fn no_sockets() -> Vec<libc::sock_filter> {
        let refuse = libc::SECCOMP_RET_ERRNO | libc::EPERM as u32;
        let arch = std::mem::offset_of!(libc::seccomp_data, arch) as u32;
        let nr = std::mem::offset_of!(libc::seccomp_data, nr) as u32;
        let load = libc::BPF_LD | libc::BPF_W | libc::BPF_ABS;
        let equal = libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K;
        let at_least = libc::BPF_JMP | libc::BPF_JGE | libc::BPF_K;
        let give = libc::BPF_RET | libc::BPF_K;
        // Each jump counts the instructions it skips, to the last one.
        vec![
            op(load, 0, 0, arch),
            op(equal, 1, 0, AUDIT_ARCH),
            op(give, 0, 0, refuse),
            op(load, 0, 0, nr),
            op(at_least, 3, 0, X32_SYSCALL_BIT),
            op(equal, 2, 0, libc::SYS_socket as u32),
            op(equal, 1, 0, libc::SYS_io_uring_setup as u32),
            op(give, 0, 0, libc::SECCOMP_RET_ALLOW),
            op(give, 0, 0, refuse),
        ]
    }

    /// One instruction of the filter, as the kernel reads it.
    fn op(code: u32, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
        libc::sock_filter {
            code: code as u16,
            jt,
            jf,
            k,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Whether Landlock can confine a command here; when it cannot, the
        /// test that needs it says why and passes without checking.
        fn usable() -> bool {
            match LANDLOCK.missing() {
                None => true,
                Some(why) => {
                    eprintln!("skipped: Landlock cannot confine a command here ({why})");
                    false
                }
            }
        }

        /// A scratch tree for one test: a project, a file outside it, and a
        /// folder standing in for Letronna's own, holding a key.
        fn tree(name: &str) -> PathBuf {
            let dir = std::env::temp_dir()
                .join(format!("letronna-landlock-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("project")).unwrap();
            std::fs::create_dir_all(dir.join("own")).unwrap();
            std::fs::write(dir.join("outside.txt"), "original").unwrap();
            std::fs::write(dir.join("own/key"), "secret").unwrap();
            std::fs::canonicalize(dir).unwrap()
        }

        fn policy(dir: &Path) -> Policy {
            Policy {
                writable: vec![dir.join("project")],
                temp: dir.join("project"),
                hidden: vec![dir.join("own")],
            }
        }

        fn run(dir: &Path, program: &str, line: &str) -> std::process::Output {
            let mut command = Command::new(program);
            command.arg("-c").arg(line).current_dir(dir.join("project"));
            let policy = policy(dir);
            confine(&command, &policy, &policy.hidden)
                .unwrap()
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap()
        }

        #[test]
        fn a_write_inside_the_project_lands() {
            if !usable() {
                return;
            }
            let dir = tree("inside");
            let ran = run(&dir, "/bin/sh", "echo hi > inside.txt && cat inside.txt");
            assert!(ran.status.success(), "{ran:?}");
            assert_eq!(String::from_utf8_lossy(&ran.stdout), "hi\n");
            std::fs::remove_dir_all(dir).unwrap();
        }

        #[test]
        fn a_write_outside_the_project_is_refused() {
            if !usable() {
                return;
            }
            let dir = tree("outside");
            let ran = run(&dir, "/bin/sh", "echo hi > ../outside.txt");
            assert!(!ran.status.success(), "{ran:?}");
            let text = std::fs::read_to_string(dir.join("outside.txt")).unwrap();
            assert_eq!(text, "original");
            let ran = run(&dir, "/bin/sh", "truncate -s 0 ../outside.txt");
            assert!(!ran.status.success(), "{ran:?}");
            let text = std::fs::read_to_string(dir.join("outside.txt")).unwrap();
            assert_eq!(text, "original");
            std::fs::remove_dir_all(dir).unwrap();
        }

        #[test]
        fn reading_a_guarded_file_is_refused() {
            if !usable() {
                return;
            }
            let dir = tree("guarded");
            let ran = run(&dir, "/bin/sh", "cat ../own/key");
            assert!(!ran.status.success(), "{ran:?}");
            assert!(!String::from_utf8_lossy(&ran.stdout).contains("secret"));
            // Its neighbour, which is not hidden, still reads, and the
            // folder holding both still lists.
            let ran = run(&dir, "/bin/sh", "cat ../outside.txt");
            assert_eq!(String::from_utf8_lossy(&ran.stdout), "original");
            let ran = run(&dir, "/bin/sh", "ls ..");
            assert!(String::from_utf8_lossy(&ran.stdout).contains("outside.txt"));
            std::fs::remove_dir_all(dir).unwrap();
        }

        /// Bash's `/dev/tcp` connects with no other program, to a listener
        /// on this machine, so only the sandbox can be what stops it.
        #[test]
        fn a_network_connect_fails() {
            if !usable() {
                return;
            }
            if !Path::new("/bin/bash").exists() {
                eprintln!("skipped: no /bin/bash to connect with");
                return;
            }
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let line = format!("echo hi > /dev/tcp/127.0.0.1/{port}");
            let open = Command::new("/bin/bash")
                .arg("-c")
                .arg(&line)
                .output()
                .unwrap();
            assert!(open.status.success(), "unconfined: {open:?}");
            let dir = tree("network");
            let ran = run(&dir, "/bin/bash", &line);
            assert!(!ran.status.success(), "{ran:?}");
            let said = String::from_utf8_lossy(&ran.stderr).to_lowercase();
            assert!(said.contains("operation not permitted"), "{said}");
            std::fs::remove_dir_all(dir).unwrap();
        }

        #[test]
        fn without_landlock_it_asks_and_runs_nothing() {
            let missing = Landlock {
                probe: || Err("simulated: Landlock is not enabled in this kernel".into()),
                tried: OnceLock::new(),
            };
            assert!(!missing.available());
            let command = Command::new("/bin/true");
            let Err(why) = missing.wrap(&command, &policy(Path::new("/nowhere"))) else {
                panic!("a command was wrapped without Landlock");
            };
            assert!(why.contains("simulated"), "{why}");
            assert!(why.contains("unsandboxed: true"), "{why}");
        }

        #[test]
        fn the_filter_sends_a_socket_and_an_io_uring_to_the_refusal() {
            let filter = no_sockets();
            let refuse = libc::SECCOMP_RET_ERRNO | libc::EPERM as u32;
            for call in [libc::SYS_socket, libc::SYS_io_uring_setup] {
                let at = filter.iter().position(|op| op.k == call as u32).unwrap();
                let to = &filter[at + 1 + filter[at].jt as usize];
                assert_eq!(to.k, refuse, "system call {call}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tree of `/a/b/c`, `/a/b/d`, `/a/e` and `/f`, listed as `listing`
    /// would.
    fn children(dir: &Path) -> Vec<PathBuf> {
        let names: &[&str] = match dir.to_str() {
            Some("/") => &["/a", "/f"],
            Some("/a") => &["/a/b", "/a/e"],
            Some("/a/b") => &["/a/b/c", "/a/b/d"],
            _ => &[],
        };
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn readable_is_the_root_alone_when_nothing_is_hidden() {
        let reads = readable(Path::new("/"), &[], &children);
        assert_eq!(reads, [PathBuf::from("/")]);
    }

    #[test]
    fn readable_grants_every_neighbour_down_the_path_to_a_hidden_folder() {
        let hidden = [PathBuf::from("/a/b/c")];
        let reads = readable(Path::new("/"), &hidden, &children);
        let expected = ["/a/b/d", "/a/e", "/f"].map(PathBuf::from);
        assert_eq!(reads, expected);
    }

    #[test]
    fn readable_leaves_out_a_hidden_folder_whole() {
        let hidden = [PathBuf::from("/a")];
        let reads = readable(Path::new("/"), &hidden, &children);
        assert_eq!(reads, [PathBuf::from("/f")]);
    }

    #[test]
    fn readable_ignores_a_hidden_path_outside_the_root() {
        let hidden = [PathBuf::from("/elsewhere/key")];
        let reads = readable(Path::new("/a"), &hidden, &children);
        assert_eq!(reads, [PathBuf::from("/a")]);
    }

    #[test]
    fn guarded_at_home_finds_keys_and_env_files_but_not_examples() {
        let home = std::env::temp_dir().join(format!("letronna-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        std::fs::create_dir_all(home.join(".kube")).unwrap();
        std::fs::write(home.join(".kube/config"), "").unwrap();
        std::fs::write(home.join(".env"), "").unwrap();
        std::fs::write(home.join(".env.example"), "").unwrap();
        std::fs::write(home.join("notes.txt"), "").unwrap();
        let home = std::fs::canonicalize(home).unwrap();
        let found = guarded_at_home(&home);
        let expected = [".env", ".kube/config", ".ssh"].map(|name| home.join(name));
        assert_eq!(found, expected);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn listing_leaves_out_links() {
        let dir = std::env::temp_dir().join(format!("letronna-listing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("real")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();
        assert_eq!(listing(&dir), [dir.join("real")]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
