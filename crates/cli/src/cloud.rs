//! `letronna cloud`: run a task on Letronna's servers instead of this
//! computer, and see, follow or stop the runs there.

use crate::render;
use letronna_core::cloud::{self, DiscordBot, Hears, Run, TIERS, Task};
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
  letronna cloud relay discord on --server <id> [--role <id>]... [--admins]
                                       have the cloud answer your Discord bot in
                                       one server, day and night, for members
                                       with a role, its admins, or both
  letronna cloud relay discord [off]   how that bot is, or stop
  letronna cloud apps                  the apps and workers your runs left running
  letronna cloud logs <thread>         what one printed last
  letronna cloud down <thread>         take one down
  letronna cloud up <thread>           bring a stopped worker back

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
        Some("apps") => apps(),
        Some("logs") => one_id(&words[1..]).and_then(|thread| {
            let (logs, failure) = cloud::app_logs(&thread)?;
            if !failure.is_empty() {
                eprintln!("{}", render::red(&failure));
            }
            println!("{}", if logs.trim().is_empty() { "(nothing printed yet)" } else { logs.trim_end() });
            Ok(())
        }),
        Some(power @ ("down" | "up")) => one_id(&words[1..]).and_then(|thread| {
            let up = power == "up";
            cloud::app_power(&thread, up)?;
            println!("{}", if up { format!("{thread} is starting again.") } else { format!("{thread} is down.") });
            Ok(())
        }),
        Some("relay") => match words.get(1).map(String::as_str) {
            Some("on") => cloud::relay(true).map(|()| {
                println!("Letronna Cloud now answers your Telegram chat whenever this computer has been off for three minutes.");
            }),
            Some("off") => cloud::relay(false).map(|()| println!("The cloud no longer answers your Telegram chat.")),
            Some("discord") => discord(&words[2..]),
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

/// Whether `id` reads as one of Discord's IDs.
fn discord_id(id: &str) -> bool {
    (15..=21).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit())
}

/// Who `letronna cloud relay discord on <words>` has the bot hear, refused
/// here when it could not be right, before the bot's token is sent anywhere.
fn discord_hears(words: &[String]) -> Result<Hears, String> {
    let mut hears = Hears::default();
    let mut rest = words.iter();
    while let Some(word) = rest.next() {
        let mut value = |flag: &str| {
            rest.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value."))
        };
        match word.as_str() {
            "--server" => hears.server = value("--server")?,
            "--role" => hears.roles.push(value("--role")?),
            "--admins" => hears.admins = true,
            "--tier" => hears.tier = value("--tier")?,
            other => return Err(format!("Unknown option {other}. See letronna cloud --help.")),
        }
    }
    if !discord_id(&hears.server) {
        return Err("Say which server: --server <its ID>, a number. In Discord, turn on Developer Mode, then right-click the server and Copy Server ID.".into());
    }
    if let Some(role) = hears.roles.iter().find(|role| !discord_id(role)) {
        return Err(format!("{role} is not a role's ID, which is a number."));
    }
    if hears.roles.is_empty() && !hears.admins {
        return Err("Say who the bot hears: --role <ID> for members with that role (once for each role), --admins for the server's owner and its Administrators, or both.".into());
    }
    if !matches!(hears.tier.as_str(), "" | "light" | "normal") {
        return Err("--tier is light or normal.".into());
    }
    Ok(hears)
}

/// The bot the cloud answers, in a line or two.
fn discord_line(bot: &DiscordBot) -> String {
    if bot.bot.id.is_empty() {
        return "The cloud answers no Discord bot of yours. Start with: letronna cloud relay discord on --server <id> --admins".into();
    }
    let mut who = Vec::new();
    if !bot.roles.is_empty() {
        who.push(match bot.roles.len() {
            1 => "members with the role you named".to_owned(),
            n => format!("members with one of {n} roles"),
        });
    }
    if bot.admins {
        who.push("the server's owner and Administrators".to_owned());
    }
    let name = |named: &cloud::Named| match named.name.is_empty() {
        true => named.id.clone(),
        false => named.name.clone(),
    };
    let mut line = format!(
        "{} in {}: {}. It hears {}. {} asked today. Thread {}.",
        name(&bot.bot),
        name(&bot.server),
        bot.state,
        who.join(" and "),
        bot.asks_today,
        bot.thread
    );
    if !bot.failure.is_empty() {
        line.push_str(&format!("\n{}", bot.failure));
    }
    line
}

/// `letronna cloud relay discord …`.
fn discord(words: &[String]) -> Result<(), String> {
    match words.first().map(String::as_str) {
        None => cloud::discord_relay_state().map(|bot| println!("{}", discord_line(&bot))),
        Some("off") => cloud::discord_relay_off().map(|()| {
            println!("The cloud no longer answers your Discord bot, and has forgotten its token. Letronna on this computer hears it again.");
        }),
        Some("on") => {
            let bot = cloud::discord_relay(&discord_hears(&words[1..])?)?;
            println!("Letronna Cloud now answers your Discord bot whether this computer is on or not; Letronna here leaves the bot to it.");
            println!("{}", discord_line(&bot));
            println!("Its tasks run apart from your account: set what it needs for thread {} alone.", bot.thread);
            Ok(())
        }
        Some(other) => Err(format!("Say on or off after discord, not {other}: letronna cloud relay discord on --server <id> --admins")),
    }
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

fn apps() -> Result<(), String> {
    let apps = cloud::apps()?;
    if apps.is_empty() {
        println!(
            "Nothing is left running. A run leaves an app or a worker when you ask it to keep one up."
        );
        return Ok(());
    }
    for app in apps {
        let at = app.address.as_deref().unwrap_or("-");
        println!(
            "{:<26} {:<7} {:<9} {:>4} MB  {at}",
            app.thread, app.kind, app.state, app.mb
        );
        if !app.failure.is_empty() {
            eprintln!(
                "{}",
                render::dim(&format!(
                    "  {}",
                    app.failure.lines().next().unwrap_or_default()
                ))
            );
        }
    }
    Ok(())
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
    fn a_discord_bot_is_told_who_it_hears_or_refused_before_its_token_is_sent() {
        let (server, role) = ("1065950016553881751", "1567927065670586488");
        assert_eq!(
            discord_hears(&words(&["--server", server, "--role", role, "--admins"])),
            Ok(Hears {
                server: server.into(),
                roles: vec![role.into()],
                admins: true,
                tier: String::new(),
            })
        );
        let two = discord_hears(&words(&["--role", role, "--tier", "normal", "--role", "123456789012345678", "--server", server])).unwrap();
        assert_eq!((two.roles.len(), two.admins, two.tier.as_str()), (2, false, "normal"));
        let refused = |all: &[&str]| discord_hears(&words(all)).unwrap_err();
        assert!(refused(&["--admins"]).contains("Say which server"));
        assert!(refused(&["--server", "my-server", "--admins"]).contains("Say which server"));
        assert!(refused(&["--server", server]).contains("Say who the bot hears"));
        assert!(refused(&["--server", server, "--role", "mods"]).contains("mods is not a role's ID"));
        assert!(refused(&["--server", server, "--admins", "--tier", "heavy"]).contains("light or normal"));
        assert!(refused(&["--server"]).contains("--server needs a value"));
        assert!(refused(&["--server", server, "--everyone"]).contains("Unknown option --everyone"));
    }

    #[test]
    fn the_bot_the_cloud_answers_is_said_in_a_line() {
        assert!(discord_line(&DiscordBot::default()).contains("answers no Discord bot"));
        let bot: DiscordBot = serde_json::from_str(
            r#"{"discord": true, "bot": {"id": "9", "name": "helper"}, "server": {"id": "5", "name": "The Server"},
                "roles": ["1567927065670586488"], "admins": true, "thread": "th_1", "state": "connected",
                "failure": "", "asks_today": 3, "invite": "https://discord.com/x"}"#,
        )
        .unwrap();
        assert_eq!(
            discord_line(&bot),
            "helper in The Server: connected. It hears members with the role you named and the server's owner and Administrators. 3 asked today. Thread th_1."
        );
        let failed = DiscordBot { state: "failed".into(), failure: "Discord refused the bot's token.".into(), admins: false, ..bot };
        assert!(discord_line(&failed).ends_with("Thread th_1.\nDiscord refused the bot's token."));
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
