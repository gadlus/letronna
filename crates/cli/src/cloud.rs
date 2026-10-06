//! `letronna cloud`: run a task on Letronna's servers instead of this
//! computer, and see, follow or stop the runs there.

use crate::render;
use letronna_core::cloud::{self, Run, TIERS, Task};
use std::io::Write;

const HELP: &str = "\
Run a task in Letronna Cloud, on Letronna's servers (Pro)

  letronna cloud \"<task>\" [options]   start it and wait for the answer
  letronna cloud list                  your recent runs
  letronna cloud show <run>            one run's answer, or how it stands
  letronna cloud stop <run>            stop it
  letronna cloud support <run> on|off  let Letronna's support read its task and
                                       answer, or take that back
  letronna cloud relay on|off          have the cloud answer your Telegram bot
                                       while this computer is off

Options
      --repo <https-url>     a git repository to work in, cloned first
      --branch <name>        the branch to clone
      --tier <tier>          light (512 MB), normal (1 GB) or heavy (2 GB);
                             light unless set
      --model <name>         a hosted model (see letronna.ai/dashboard/models);
                             the cloud's default unless set
      --thread <id>          carry on an earlier run's thread: its folder and
                             conversation, kept on the server between runs
      --no-wait              start it and print its id

Runs are billed from your Letronna credit: model tokens as usual, and the
run's memory by the second.";

/// Run `letronna cloud <words>` and give the exit code.
pub fn run(words: &[String]) -> i32 {
    let outcome = match words.first().map(String::as_str) {
        None | Some("-h" | "--help" | "help") => {
            println!("{HELP}");
            return 0;
        }
        Some("list") => list(),
        Some("show") => one_id(&words[1..]).and_then(show),
        Some("stop") => one_id(&words[1..]).and_then(|id| {
            cloud::stop(&id)?;
            println!("Stopped {id}.");
            Ok(())
        }),
        Some("relay") => match words.get(1).map(String::as_str) {
            Some("on") => cloud::relay(true).map(|()| {
                println!("Letronna Cloud now answers your Telegram chat whenever this computer has been off for three minutes.");
            }),
            Some("off") => cloud::relay(false).map(|()| println!("The cloud no longer answers your Telegram chat.")),
            _ => Err("Say on or off: letronna cloud relay on".into()),
        },
        Some("support") => match &words[1..] {
            [id, on] if on == "on" || on == "off" => cloud::support(id, on == "on").map(|()| {
                if on == "on" {
                    println!("Letronna's support can now read {id}'s task and answer; each look is logged.");
                } else {
                    println!("Support can no longer read {id}.");
                }
            }),
            _ => Err("Say which run, and on or off: letronna cloud support run_abc123 on".into()),
        },
        Some(_) => parse(words).and_then(|(task, wait)| start(&task, wait)),
    };
    match outcome {
        Ok(()) => 0,
        Err(why) => {
            eprintln!("{}", render::red(&why));
            1
        }
    }
}

fn one_id(words: &[String]) -> Result<String, String> {
    match words {
        [id] => Ok(id.clone()),
        _ => Err("Name one run, like: letronna cloud show run_abc123".into()),
    }
}

/// The task and whether to wait for it, from the words after `cloud`.
fn parse(words: &[String]) -> Result<(Task, bool), String> {
    let mut task = Task::default();
    let mut wait = true;
    let mut prompt = Vec::new();
    let mut rest = words.iter();
    while let Some(word) = rest.next() {
        let mut value = |flag: &str| {
            rest.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value."))
        };
        match word.as_str() {
            "--repo" => task.repo = value("--repo")?,
            "--branch" => task.branch = value("--branch")?,
            "--tier" => {
                task.tier = value("--tier")?;
                if !TIERS.contains(&task.tier.as_str()) {
                    return Err(format!("--tier is one of {}.", TIERS.join(", ")));
                }
            }
            "--thread" => task.thread = value("--thread")?,
            "--model" => task.model = value("--model")?,
            "--no-wait" => wait = false,
            flag if flag.starts_with("--") => {
                return Err(format!("Unknown option {flag}. See letronna cloud --help."));
            }
            word => prompt.push(word.to_owned()),
        }
    }
    task.prompt = prompt.join(" ");
    if task.prompt.trim().is_empty() {
        return Err("Say what to do, like: letronna cloud \"add a test for parse\" --repo https://github.com/me/app".into());
    }
    Ok((task, wait))
}

fn start(task: &Task, wait: bool) -> Result<(), String> {
    let run = cloud::start(task)?;
    if !wait {
        println!("{}  (thread {})", run.id, run.thread);
        return Ok(());
    }
    eprintln!(
        "{}",
        render::dim(&format!(
            "Started {} on a {} run (thread {}).",
            run.id, run.tier, run.thread
        ))
    );
    let mut said = String::new();
    let mut doing = String::new();
    let run = cloud::follow(
        &run.id,
        &mut |heard| match heard {
            cloud::Heard::Status(status) if status != said => {
                eprintln!("{}", render::dim(&format!("{status}…")));
                said = status;
            }
            cloud::Heard::Doing { doing: now, .. } if !now.is_empty() && now != doing => {
                eprintln!("{}", render::dim(&format!("  · {now}")));
                doing = now;
            }
            _ => {}
        },
        &|| false,
    )?;
    print_run(&run);
    let _ = std::io::stdout().flush();
    if run.status == "done" {
        Ok(())
    } else {
        Err(format!("The run {}.", run.status))
    }
}

fn show(id: String) -> Result<(), String> {
    print_run(&cloud::run(&id)?);
    Ok(())
}

fn print_run(run: &Run) {
    match run.status.as_str() {
        "done" => {
            println!("{}", run.answer.trim());
            if let Some(preview) = &run.preview {
                println!(
                    "
See what it made: {preview}"
                );
            }
        }
        "failed" => eprintln!("{}", render::red(&format!("Failed: {}", run.error.trim()))),
        status => println!("{} is {status}.", run.id),
    }
    eprintln!(
        "{}",
        render::dim(&format!(
            "{} · {} · {}s · thread {} (carry on with --thread {})",
            run.id, run.tier, run.seconds, run.thread, run.thread
        ))
    );
}

fn list() -> Result<(), String> {
    let runs = cloud::runs(20)?;
    if runs.is_empty() {
        println!("No cloud runs yet. Start one with: letronna cloud \"<task>\"");
        return Ok(());
    }
    for run in runs {
        let first_line = run.prompt.lines().next().unwrap_or_default();
        let short: String = first_line.chars().take(60).collect();
        println!(
            "{:<22} {:<8} {:<7} {:>5}s  {short}",
            run.id, run.status, run.tier, run.seconds
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(all: &[&str]) -> Vec<String> {
        all.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn a_task_takes_its_words_and_flags_in_any_order() {
        let (task, wait) = parse(&words(&[
            "fix",
            "--tier",
            "heavy",
            "the",
            "build",
            "--no-wait",
        ]))
        .unwrap();
        assert_eq!(task.prompt, "fix the build");
        assert_eq!(task.tier, "heavy");
        assert!(!wait);
        let (task, wait) = parse(&words(&[
            "go on",
            "--thread",
            "th_1",
            "--repo",
            "https://x/y",
        ]))
        .unwrap();
        assert_eq!(
            (task.thread.as_str(), task.repo.as_str(), wait),
            ("th_1", "https://x/y", true)
        );
    }

    #[test]
    fn a_wrong_task_is_refused_before_anything_is_sent() {
        assert!(
            parse(&words(&["--tier", "huge", "x"]))
                .unwrap_err()
                .contains("light, normal, heavy")
        );
        assert!(
            parse(&words(&["--repo"]))
                .unwrap_err()
                .contains("--repo needs")
        );
        assert!(
            parse(&words(&["--no-wait"]))
                .unwrap_err()
                .contains("Say what to do")
        );
        assert!(
            parse(&words(&["x", "--colour"]))
                .unwrap_err()
                .contains("Unknown option")
        );
    }
}
