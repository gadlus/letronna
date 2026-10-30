//! `/permissions`: the rules "Always allow" and the settings kept, and a way
//! to take one back out.
//!
//! A yes kept by mistake used to last until `permissions.json` was edited by
//! hand. The list shows the user's rules allowed first, then asked about,
//! then denied, each with the folder it holds in, and picking one removes
//! it; the gate reads the file again for the next call, so it is gone at
//! once, and the list comes back with what is left so several can go in a
//! row. A project's own rules, in its `.letronna/permissions.json`, are said
//! beside it but never offered: that file is the repo's and changed there.

use std::path::{Path, PathBuf};

use letronna_core::permissions::{self, Effect, Rule};

use crate::render;
use crate::session::Runner;

/// `/permissions`: list the user's rules and remove each one picked.
pub fn permissions(runner: &mut Runner, rest: &str, workspace: Option<&Path>) {
    render::blank();
    if !rest.is_empty() {
        render::line(&render::red(
            "Usage: /permissions lists your rules and removes the ones you pick.",
        ));
        return;
    }
    let file = permissions::user_file();
    let home =
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    let mut detail = vec![format!(
        "Kept in {}. Picking one removes it.",
        shown(&file, home.as_deref())
    )];
    detail.extend(project_lines(workspace));
    render::said(&render::blue("●"), &render::bold("Permission rules"));
    let mut removed = false;
    loop {
        let rules = match permissions::load(&file) {
            Ok(rules) => rules,
            Err(why) => {
                render::line(&render::red(&why));
                return;
            }
        };
        let listed = in_order(&rules);
        if listed.is_empty() {
            render::line(&render::dim(
                "None kept yet. \"Yes, and always allow\" on a command adds one.",
            ));
            for line in &detail[1..] {
                render::line(&render::dim(line));
            }
            return;
        }
        let labels: Vec<String> = (listed.iter())
            .map(|rule| label(rule, workspace, home.as_deref()))
            .collect();
        let mut options: Vec<&str> = labels.iter().map(String::as_str).collect();
        options.push("Done");
        let picked = runner
            .bar
            .pick_from("Remove which rule?", detail.clone(), &options);
        let Some(rule) = picked.and_then(|at| listed.get(at)) else {
            let left = if removed {
                "Left the rest"
            } else {
                "Kept them"
            };
            render::line(&render::dim(&format!("{left} as they are.")));
            return;
        };
        match permissions::forget(&file, rule) {
            Ok(true) => {
                removed = true;
                render::line(&format!(
                    "{} {}",
                    render::green("Removed"),
                    label(rule, workspace, home.as_deref())
                ));
            }
            // The window took it out meanwhile, which comes to the same.
            Ok(false) => render::line(&render::dim("That one was gone already.")),
            Err(why) => {
                render::line(&render::red(&why));
                return;
            }
        }
    }
}

/// `rules` as the list shows them: allowed first, then asked about, then
/// denied, each kind in the order it was kept.
fn in_order(rules: &[Rule]) -> Vec<&Rule> {
    [Effect::Allow, Effect::Ask, Effect::Deny]
        .into_iter()
        .flat_map(|effect| rules.iter().filter(move |rule| rule.effect == effect))
        .collect()
}

/// One rule in a line: what it does, to what, and where it holds.
fn label(rule: &Rule, workspace: Option<&Path>, home: Option<&Path>) -> String {
    let place = match &rule.project {
        None => "everywhere".to_owned(),
        Some(folder) if Some(folder.as_path()) == workspace => "here".to_owned(),
        Some(folder) => format!("in {}", shown(folder, home)),
    };
    let off = rule.effect == Effect::Deny && rule.whole() && rule.project.is_none();
    let off = if off { " (switched off)" } else { "" };
    format!("{} {place}{off}", does(rule))
}

/// What a rule does, and to what.
fn does(rule: &Rule) -> String {
    let effect = match rule.effect {
        Effect::Allow => "allow",
        Effect::Ask => "ask first about",
        Effect::Deny => "deny",
    };
    let what = if rule.whole() {
        format!("every `{}` call", rule.tool)
    } else if rule.tool == "run_command" {
        format!("`{}`", rule.pattern.trim())
    } else {
        format!("`{}` {}", rule.tool, rule.pattern.trim())
    };
    // The file is the user's to edit, and a pattern holding an escape would
    // restyle or move about the terminal when printed.
    let does = format!("{effect} {what}");
    does.chars().filter(|c| !c.is_control()).collect()
}

/// What the project's own rules file says, to read beside the list.
fn project_lines(workspace: Option<&Path>) -> Vec<String> {
    let Some(folder) = workspace else {
        return Vec::new();
    };
    match permissions::project_rules(folder) {
        Ok(rules) if rules.is_empty() => Vec::new(),
        Ok(rules) => {
            let mut lines = vec![format!(
                "This project's own, changed in its {}:",
                permissions::PROJECT_FILE
            )];
            lines.extend(
                in_order(&rules)
                    .into_iter()
                    .map(|rule| format!("  {}", does(rule))),
            );
            lines
        }
        Err(why) => vec![why],
    }
}

/// A path with the home folder written `~`.
fn shown(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(inside) => Path::new("~").join(inside).display().to_string(),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(tool: &str, pattern: &str, effect: Effect, project: Option<&str>) -> Rule {
        Rule {
            tool: tool.into(),
            pattern: pattern.into(),
            effect,
            project: project.map(PathBuf::from),
        }
    }

    #[test]
    fn a_rule_is_named_by_what_it_does_and_where_it_holds() {
        let here = Some(Path::new("/home/user_a/app"));
        let home = Some(Path::new("/home/user_a"));
        let said = |rule: &Rule| label(rule, here, home);
        let test = rule(
            "run_command",
            "cargo test",
            Effect::Allow,
            Some("/home/user_a/app"),
        );
        assert_eq!(said(&test), "allow `cargo test` here");
        let push = rule(
            "run_command",
            "git push",
            Effect::Deny,
            Some("/home/user_a/site"),
        );
        assert_eq!(said(&push), "deny `git push` in ~/site");
        let docs = rule("web_fetch", "docs.rs", Effect::Ask, None);
        assert_eq!(
            said(&docs),
            "ask first about `web_fetch` docs.rs everywhere"
        );
        let off = rule("speak", "*", Effect::Deny, None);
        assert_eq!(
            said(&off),
            "deny every `speak` call everywhere (switched off)"
        );
        let sly = rule("run_command", "ls\u{1b}[2J", Effect::Allow, None);
        assert_eq!(said(&sly), "allow `ls[2J` everywhere");
    }

    #[test]
    fn rules_are_listed_allowed_first_then_asked_about_then_denied() {
        let rules = [
            rule("run_command", "git push", Effect::Deny, None),
            rule("run_command", "cargo test", Effect::Allow, None),
            rule("web_fetch", "*", Effect::Ask, None),
            rule("run_command", "ls", Effect::Allow, None),
        ];
        let order: Vec<&str> = in_order(&rules)
            .iter()
            .map(|rule| rule.pattern.as_str())
            .collect();
        assert_eq!(order, ["cargo test", "ls", "*", "git push"]);
    }
}
