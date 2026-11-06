//! Settings › Permissions: the rules "Always allow" kept, and those that ask
//! first or deny, each with a way to take it back out.
//!
//! A yes kept by mistake used to last until `permissions.json` was edited by
//! hand. The page shows the user's rules as last read, which Settings and
//! this page read again as they open and after each removal rather than on
//! every redraw; the gate reads the file again before its next call, so a
//! rule removed here is gone at once, in the terminal too. A project's own
//! `.letronna/permissions.json` is listed below but not offered: it is the
//! repo's, and changed there.

use crate::prelude::*;
use letronna_core::permissions::{self, Effect, Rule};
use std::path::Path;

impl Chat {
    /// Takes `rule` out of the user's rules and reads them again; one the
    /// terminal took out meanwhile just goes from the list.
    pub(crate) fn forget_rule(&mut self, rule: &Rule, cx: &mut Context<Self>) {
        if let Err(why) = permissions::forget(&permissions::user_file(), rule) {
            self.error = Some(why);
        }
        self.read_rules();
        cx.notify();
    }

    /// The Permissions page of Settings.
    pub(crate) fn permissions_page(&self, cx: &mut Context<Self>) -> Div {
        let page = v_flex().gap_5().child(super::settings::heading(
            "Permissions",
            "What \"Always allow\" kept, and what is set to ask first or be denied. A rule removed here holds from the next call, in the terminal too.",
            cx,
        ));
        let rules = match &self.rules {
            Ok(rules) => rules,
            Err(why) => {
                return page.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(why.clone()),
                );
            }
        };
        let on_screen = self.store.conversations.get(self.store.active);
        let workspace = on_screen.and_then(|conversation| conversation.workspace.clone());
        let home =
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
        let mut page = page;
        if rules.is_empty() {
            page = page.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("None kept yet. \"Always allow\" on a call's card adds one here."),
            );
        }
        for (effect, title) in [
            (Effect::Allow, "Allowed"),
            (Effect::Ask, "Asked first"),
            (Effect::Deny, "Denied"),
        ] {
            let kind: Vec<&Rule> = rules.iter().filter(|rule| rule.effect == effect).collect();
            if kind.is_empty() {
                continue;
            }
            let mut list = v_flex()
                .border_1()
                .border_color(cx.theme().border)
                .rounded(cx.theme().radius);
            for (i, rule) in kind.into_iter().enumerate() {
                let (what, detail) = shown(rule, workspace.as_deref(), home.as_deref());
                let gone = rule.clone();
                list = list.child(
                    row(i, rule, what, detail, cx).child(
                        Button::new(SharedString::from(format!("forget-rule-{title}-{i}")))
                            .plain(cx)
                            .xsmall()
                            .icon(Icon::new(AppIcon::Trash))
                            .tooltip("Remove")
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.forget_rule(&gone, cx)),
                            ),
                    ),
                );
            }
            page = page.child(
                v_flex()
                    .gap_2()
                    .child(super::settings::label(title, cx))
                    .child(list),
            );
        }
        let own = (workspace.as_deref())
            .and_then(|folder| permissions::project_rules(folder).ok())
            .unwrap_or_default();
        if own.is_empty() {
            return page;
        }
        let mut list = v_flex()
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius);
        for (i, rule) in own.iter().enumerate() {
            let (what, detail) = shown(rule, None, None);
            let detail = format!("{} · {detail}", effect_word(rule.effect));
            list = list.child(row(i, rule, what, detail, cx));
        }
        page.child(
            v_flex()
                .gap_2()
                .child(super::settings::label("This project's own", cx))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "In its {}, which belongs to the repo, so they are changed there.",
                            permissions::PROJECT_FILE
                        )),
                )
                .child(list),
        )
    }
}

/// One rule's row, the `i`th of its list: what it covers above where, a
/// pattern in the code font and a whole tool in the plain one.
fn row(i: usize, rule: &Rule, what: String, detail: String, cx: &App) -> Div {
    h_flex()
        .when(i > 0, |d| d.border_t_1().border_color(cx.theme().border))
        .px_4()
        .py_2()
        .gap_3()
        .items_center()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_sm()
                        .when(!rule.whole(), |d| d.font_family(MONO_FONT))
                        .child(what),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(detail),
                ),
        )
}

/// What a rule covers, and the tool and folder it holds for.
fn shown(rule: &Rule, workspace: Option<&Path>, home: Option<&Path>) -> (String, String) {
    let what = if rule.whole() {
        "Every call".to_owned()
    } else {
        // The file is the user's to edit; what it holds is shown as text.
        rule.pattern
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .collect()
    };
    let tool = match rule.tool.as_str() {
        "run_command" => "commands",
        tool => tool,
    };
    let place = match &rule.project {
        None => "everywhere".to_owned(),
        Some(folder) if Some(folder.as_path()) == workspace => "in this project".to_owned(),
        Some(folder) => match home.and_then(|home| folder.strip_prefix(home).ok()) {
            Some(inside) => format!("in {}", Path::new("~").join(inside).display()),
            None => format!("in {}", folder.display()),
        },
    };
    let off = rule.effect == Effect::Deny && rule.whole() && rule.project.is_none();
    let off = if off {
        " · switched off on the Capabilities page"
    } else {
        ""
    };
    (what, format!("{tool} · {place}{off}"))
}

fn effect_word(effect: Effect) -> &'static str {
    match effect {
        Effect::Allow => "allowed",
        Effect::Ask => "asked first",
        Effect::Deny => "denied",
    }
}

#[cfg(test)]
mod tests {
    use super::{Effect, Rule, shown};
    use std::path::{Path, PathBuf};

    fn rule(tool: &str, pattern: &str, effect: Effect, project: Option<&str>) -> Rule {
        Rule {
            tool: tool.into(),
            pattern: pattern.into(),
            effect,
            project: project.map(PathBuf::from),
        }
    }

    #[test]
    fn a_rule_row_says_what_it_covers_then_the_tool_and_where() {
        let here = Some(Path::new("/home/user_a/app"));
        let home = Some(Path::new("/home/user_a"));
        let said = |rule: &Rule| shown(rule, here, home);
        let test = rule(
            "run_command",
            "cargo test",
            Effect::Allow,
            Some("/home/user_a/app"),
        );
        assert_eq!(
            said(&test),
            ("cargo test".into(), "commands · in this project".into())
        );
        let push = rule(
            "run_command",
            "git push",
            Effect::Deny,
            Some("/home/user_a/site"),
        );
        // Shown with this computer's own separator.
        let site = if cfg!(windows) { r"~\site" } else { "~/site" };
        assert_eq!(said(&push).1, format!("commands · in {site}"));
        let docs = rule("web_fetch", "docs.rs", Effect::Allow, None);
        assert_eq!(
            said(&docs),
            ("docs.rs".into(), "web_fetch · everywhere".into())
        );
        let off = rule("speak", "*", Effect::Deny, None);
        assert_eq!(
            said(&off),
            (
                "Every call".into(),
                "speak · everywhere · switched off on the Capabilities page".into()
            )
        );
        let sly = rule("run_command", "ls\u{1b}[2J", Effect::Allow, None);
        assert_eq!(said(&sly).0, "ls[2J");
    }
}
