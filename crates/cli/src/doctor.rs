//! `letronna doctor`: the first things to look at when something is wrong,
//! on one screen, with an exit code a script can read -- 1 when any line is
//! a problem. Nothing secret is printed: a key is only said to be saved or
//! not, and the gateway is asked whether it is there without it.

use crate::render;
use letronna_core::{auth, diag, gateway, store::Settings};
use std::path::Path;

/// Less free space than this on the data folder's disk is a problem: a
/// session, the logs and an update download all need room to be written.
const LOW_SPACE: u64 = 500 * 1024 * 1024;

/// One line of the report.
struct Check {
    what: &'static str,
    found: String,
    problem: bool,
}

/// Print the report and return the exit code.
pub fn run(offline: bool) -> i32 {
    let checks = report(&letronna_core::chat::data_dir(), offline);
    for check in &checks {
        let found = if check.problem {
            render::red(&check.found)
        } else {
            check.found.clone()
        };
        render::line(&format!(
            "{} {found}",
            render::dim(&format!("{:<10}", check.what))
        ));
    }
    let problems = checks.iter().filter(|check| check.problem).count();
    render::line(&match problems {
        0 => render::green("No problems found."),
        1 => render::red("1 problem."),
        n => render::red(&format!("{n} problems.")),
    });
    i32::from(problems > 0)
}

/// Every check, on the data folder `dir`; `offline` leaves out the one that
/// asks the gateway.
fn report(dir: &Path, offline: bool) -> Vec<Check> {
    let fine = |what, found: String| Check {
        what,
        found,
        problem: false,
    };
    let broken = |what, found: String| Check {
        what,
        found,
        problem: true,
    };
    let mut checks = vec![
        fine("version", letronna_core::update::VERSION.to_owned()),
        fine(
            "system",
            format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        ),
    ];
    let free = free_space(dir);
    let folder = match free {
        Some(bytes) => format!("{} ({:.1} GB free)", dir.display(), bytes as f64 / 1e9),
        None => dir.display().to_string(),
    };
    checks.push(match free {
        Some(bytes) if bytes < LOW_SPACE => broken("data", folder),
        _ => fine("data", folder),
    });
    match settings(dir) {
        Ok(settings) => {
            checks.push(fine("settings", "readable".into()));
            checks.extend(provider(dir, &settings, offline));
        }
        Err(error) => checks.push(broken("settings", error)),
    }
    checks.push(fine("logs", diag::logs(dir).display().to_string()));
    let crash = diag::last_crash(dir).map_or("none".into(), |path| path.display().to_string());
    checks.push(fine("last crash", crash));
    checks
}

fn settings(dir: &Path) -> Result<Settings, String> {
    match std::fs::read(dir.join("settings.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("settings.json cannot be read: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err("nothing is set up yet: run `letronna setup`".into())
        }
        Err(error) => Err(format!("settings.json cannot be read: {}", error.kind())),
    }
}

/// The gateway the current model is on, whether it is set up, and whether
/// it answers.
fn provider(dir: &Path, settings: &Settings, offline: bool) -> Vec<Check> {
    let name = gateway::gateway(&settings.gateway).name;
    let keys: std::collections::HashMap<String, String> = std::fs::read(dir.join("keys.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let key_saved = keys
        .get(gateway::key_slot(name))
        .is_some_and(|key| !key.trim().is_empty());
    let (ready, how) = match name {
        gateway::LETRONNA if auth::signed_in() => (true, "signed in"),
        gateway::LETRONNA => (false, "not signed in: run `letronna setup`"),
        "Custom" if settings.custom_endpoint.trim().is_empty() => (false, "no URL given"),
        _ if !gateway::gateway(name).needs_key => (true, "no key needed"),
        _ if key_saved => (true, "key saved"),
        _ => (false, "no key saved: run `letronna setup`"),
    };
    let model = match settings.model.as_str() {
        "" => "no model chosen".to_owned(),
        model => format!("model {model}"),
    };
    let mut checks = vec![Check {
        what: "provider",
        found: format!("{name}, {model}, {how}"),
        problem: !ready || settings.model.is_empty(),
    }];
    let (base, _) = gateway::resolve(name, &settings.custom_endpoint, "");
    if base.trim().is_empty() {
        return checks;
    }
    // A pasted URL can carry a key of its own.
    let shown = diag::mask(&base, &[]);
    let (found, problem) = if offline {
        (format!("{shown}, not tried (--offline)"), false)
    } else {
        match diag::reachable(&format!("{}/models", base.trim_end_matches('/'))) {
            Ok(status) => (format!("{shown} answers (HTTP {status})"), false),
            Err(error) => (format!("{shown}: {error}"), true),
        }
    };
    checks.push(Check {
        what: "endpoint",
        found,
        problem,
    });
    checks
}

/// Bytes free on the disk the data folder is on, or on the nearest folder
/// above it that exists yet.
fn free_space(dir: &Path) -> Option<u64> {
    let dir = dir.ancestors().find(|path| path.exists())?;
    free_on(dir)
}

#[cfg(unix)]
fn free_on(dir: &Path) -> Option<u64> {
    let out = std::process::Command::new("df")
        .arg("-Pk")
        .arg(dir)
        .output()
        .ok()?;
    available(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(windows)]
fn free_on(dir: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(
            dir: *const u16,
            free: *mut u64,
            total: *mut u64,
            total_free: *mut u64,
        ) -> i32;
    }
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
    let mut free = 0u64;
    // SAFETY: `wide` is a NUL-terminated path that outlives the call, `free`
    // is a u64 the call fills in, and the totals it may skip are null.
    let done = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (done != 0).then_some(free)
}

/// Bytes available, from `df -Pk` output: the column before the "NN%" one,
/// counted from there because a disk's name can have spaces in it.
#[cfg(unix)]
fn available(df: &str) -> Option<u64> {
    let row: Vec<&str> = df.lines().nth(1)?.split_whitespace().collect();
    let capacity = row.iter().rposition(|word| word.ends_with('%'))?;
    let kilobytes: u64 = row.get(capacity.checked_sub(1)?)?.parse().ok()?;
    Some(kilobytes * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("letronna-doctor-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn text(checks: &[Check]) -> String {
        checks
            .iter()
            .map(|check| format!("{} {}\n", check.what, check.found))
            .collect()
    }

    #[test]
    fn an_offline_report_names_the_setup_and_never_the_key() {
        let dir = scratch("set-up");
        let settings = r#"{"endpoint": "", "model": "gpt-test", "gateway": "OpenAI"}"#;
        std::fs::write(dir.join("settings.json"), settings).unwrap();
        std::fs::write(
            dir.join("keys.json"),
            r#"{"OpenAI": "sk-test-0123456789abcdef"}"#,
        )
        .unwrap();
        let checks = report(&dir, true);
        let said = text(&checks);
        assert!(!said.contains("sk-test"), "{said}");
        assert!(
            said.contains("provider OpenAI, model gpt-test, key saved"),
            "{said}"
        );
        assert!(said.contains("not tried (--offline)"), "{said}");
        assert!(said.contains("last crash none"), "{said}");
        assert!(checks.iter().all(|check| !check.problem), "{said}");
    }

    #[test]
    fn nothing_set_up_is_a_problem_that_says_what_to_run() {
        let checks = report(&scratch("empty"), true);
        assert!(
            checks
                .iter()
                .any(|check| check.problem && check.found.contains("letronna setup")),
            "{}",
            text(&checks)
        );
    }

    #[cfg(unix)]
    #[test]
    fn free_space_is_read_from_the_column_before_the_percentage() {
        let df = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                  map auto_home 100 40 60 40% /System/Volumes/Data/home\n";
        assert_eq!(available(df), Some(60 * 1024));
        assert_eq!(available("Filesystem\n"), None);
    }
}
