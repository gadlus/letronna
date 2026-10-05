//! "Remember me": when a chat saves to memory and where it keeps it. The
//! status bar's chip shows it for the chat on screen and opens a menu to
//! change it there; the Memory page of settings sets it for every chat that
//! has not chosen for itself.

use crate::prelude::*;
use gpui_component::{
    menu::PopupMenu,
    popover::Popover,
    radio::{Radio, RadioGroup},
    switch::Switch,
};
use letronna_core::memory::{Saving, Scope};

actions!(remember, [MemoryMenu]);

/// What a row of the chip's menu does to the chat it was opened over.
#[derive(Clone, Copy)]
enum Choice {
    Private,
    Save(Saving),
    Keep(Scope),
    /// Follow the settings again, in both.
    Default,
    /// Make the chat's choices the settings', which it then follows.
    MakeDefault,
}

impl Chat {
    /// When the chat on screen saves to memory and where it keeps it, in
    /// the accent colour while that is its own choice rather than the
    /// default; with `words`, said on the chip, else only in its tooltip.
    /// Opens the menu on a click, or on Enter or Space while it has the
    /// focus.
    pub(crate) fn memory_chip(&self, words: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let ix = self.store.active;
        let (saving, scope) = (self.store.memory_saving(ix), self.store.memory_scope(ix));
        let settings = &self.store.settings;
        let own = &self.store.conversations[ix];
        let chosen = overrides(own.memory_saving, settings.memory_saving)
            || overrides(own.memory_scope, settings.memory_scope);
        let whose = if chosen {
            format!(
                "{}, this chat's own choice; your default is {}.",
                shown(saving, scope),
                shown(settings.memory_saving, settings.memory_scope)
            )
        } else {
            format!("{}, your default.", shown(saving, scope))
        };
        let chip = Button::new("memory")
            .plain(cx)
            .xsmall()
            // Not over its own menu, which it would cover.
            .when(self.memory_menu.is_none(), |chip| {
                chip.tooltip(format!(
                    "When this chat saves to memory and where it keeps it: {whose} \
                     Saved memories are still used unless Keep in leaves them out."
                ))
            })
            .text_color(if chosen {
                cx.theme().primary
            } else {
                cx.theme().muted_foreground
            })
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(Icon::new(AppIcon::Brain).size_3())
                    .child(if words {
                        format!("Memory: {}", shown(saving, scope))
                    } else {
                        "Memory".to_owned()
                    }),
            )
            // The popover opens on a mouse press only, so a key opens it here.
            .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                if event.is_keyboard() {
                    this.open_memory_menu(window, cx);
                }
            }));
        let chat = cx.entity();
        Popover::new("memory-menu")
            .anchor(gpui::Anchor::BottomLeft)
            .appearance(false)
            .overlay_closable(false)
            .open(self.memory_menu.is_some())
            .on_open_change(move |open, window, cx| {
                chat.update(cx, |this, cx| {
                    if *open {
                        this.open_memory_menu(window, cx);
                    } else {
                        this.memory_menu = None;
                        cx.notify();
                    }
                })
            })
            .trigger(chip)
            .when_some(self.memory_menu.clone(), |popover, menu| {
                popover.content(move |_, _, _| menu.clone())
            })
    }

    /// Open the chip's menu with the keys in it: Private chat, when to save,
    /// where to keep, and the way back to the default or to make it one.
    pub(crate) fn open_memory_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.memory_menu.is_some() {
            return;
        }
        // Worked out here: the menu is built while this chat is borrowed.
        let ix = self.store.active;
        let conversation = &self.store.conversations[ix];
        let (id, project) = (conversation.id, conversation.workspace.is_some());
        let own = (conversation.memory_saving, conversation.memory_scope);
        let settings = &self.store.settings;
        let defaults = (settings.memory_saving, settings.memory_scope);
        let now = (self.store.memory_saving(ix), self.store.memory_scope(ix));
        let chat = cx.entity();
        // Closed, the menu hands the keys back to the composer.
        let back = self.composer.focus_handle(cx);
        let menu = PopupMenu::build(window, cx, move |menu, _, _| {
            let pick = |choice: Choice| {
                let chat = chat.clone();
                move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                    chat.update(cx, |this, cx| this.remember_as(id, choice, cx))
                }
            };
            let private = now == (Saving::WhenAsked, Scope::Chat);
            let mut menu = menu
                .action_context(back)
                .min_w(px(250.))
                .item(
                    described("Private chat", "Only when I ask · This chat only")
                        .checked(private)
                        .on_click(pick(Choice::Private)),
                )
                .separator()
                .label("Save");
            for saving in Saving::OFFERED {
                menu = menu.item(
                    PopupMenuItem::new(saving.label())
                        .checked(now.0 == saving)
                        .on_click(pick(Choice::Save(saving))),
                );
            }
            menu = menu.separator().label("Keep in");
            for scope in Scope::OFFERED {
                menu = menu.item(
                    PopupMenuItem::new(scope.label())
                        .checked(now.1 == scope)
                        .on_click(pick(Choice::Keep(scope))),
                );
            }
            // As core keeps it: a project's memory needs a project.
            if !project && now.1 == Scope::Project {
                menu = menu.label("No project folder yet, so this chat only");
            }
            menu.separator()
                .item(
                    described("Use my default", &shown(defaults.0, defaults.1))
                        .disabled(own == (None, None))
                        .on_click(pick(Choice::Default)),
                )
                .item(
                    PopupMenuItem::new("Make this my default")
                        .disabled(now == defaults)
                        .on_click(pick(Choice::MakeDefault)),
                )
        });
        menu.focus_handle(cx).focus(window, cx);
        cx.subscribe(&menu, |this, _, _: &DismissEvent, cx| {
            this.memory_menu = None;
            cx.notify();
        })
        .detach();
        self.memory_menu = Some(menu);
        cx.notify();
    }

    /// Does what `choice` says to session `id`, from its next call on, and
    /// saves it with the session.
    fn remember_as(&mut self, id: u64, choice: Choice, cx: &mut Context<Self>) {
        let Some(ix) = self.store.index_of(id) else {
            return;
        };
        let store = &mut self.store;
        match choice {
            Choice::Private => store.make_private(ix),
            Choice::Save(saving) => store.choose_memory_saving(ix, Some(saving)),
            Choice::Keep(scope) => store.choose_memory_scope(ix, Some(scope)),
            Choice::Default => {
                store.choose_memory_saving(ix, None);
                store.choose_memory_scope(ix, None);
            }
            Choice::MakeDefault => {
                store.settings.memory_saving = store.memory_saving(ix);
                store.settings.memory_scope = store.memory_scope(ix);
                // The default's now, so the chat follows it, and the chip
                // no longer marks them as its own.
                store.choose_memory_saving(ix, None);
                store.choose_memory_scope(ix, None);
            }
        }
        self.save();
        cx.notify();
    }
}

/// A menu row named `label`, with `detail` after it in muted text.
fn described(label: &'static str, detail: &str) -> PopupMenuItem {
    let detail = SharedString::from(detail.to_owned());
    PopupMenuItem::element(move |_, cx| {
        h_flex().gap_2().child(label).child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(detail.clone()),
        )
    })
}

/// The Memory page's choices for every chat that has not chosen for itself,
/// and the review that runs only while memory is saved automatically.
pub(crate) fn defaults(chat: &Entity<Chat>, settings: &Settings, cx: &App) -> Div {
    let about = |text: &'static str| {
        div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(text)
    };
    let saving = settings.memory_saving;
    let set = chat.clone();
    let when = RadioGroup::vertical("memory-saving")
        .selected_index(Saving::OFFERED.iter().position(|&s| s == saving))
        .children(Saving::OFFERED.map(|s| {
            Radio::new(s.name())
                .small()
                .label(s.label())
                .child(about(match s {
                    Saving::WhenAsked => {
                        "When you ask it to remember or forget something. It may offer in one line."
                    }
                    Saving::Ask => "Each save waits on a card that shows the exact entry.",
                    Saving::Auto => "Whenever it sees something that will matter later.",
                }))
        }))
        .on_click(move |at, _, cx| {
            set.update(cx, |this, cx| {
                this.store.settings.memory_saving = Saving::OFFERED[*at];
                this.save();
                cx.notify();
            })
        });
    let set = chat.clone();
    let scope = settings.memory_scope;
    let place = RadioGroup::vertical("memory-scope")
        .selected_index(Scope::OFFERED.iter().position(|&s| s == scope))
        .children(Scope::OFFERED.map(|s| {
            Radio::new(s.name())
                .small()
                .label(s.label())
                .child(about(match s {
                    Scope::Auto => {
                        "What it knows about you, its own notes and the project's, which every chat loads."
                    }
                    Scope::Project => {
                        "The project's notes: a chat loads those and its own, nothing else. With no project folder, as This chat only."
                    }
                    Scope::Chat => {
                        "Only the chat's own notes, which no other chat sees and which go when it is deleted."
                    }
                }))
        }))
        .on_click(move |at, _, cx| {
            set.update(cx, |this, cx| {
                this.store.settings.memory_scope = Scope::OFFERED[*at];
                this.save();
                cx.notify();
            })
        });
    v_flex()
        .gap_4()
        .child(
            v_flex()
                .gap_2()
                .child(super::settings::label("Save to memory", cx))
                .child(when),
        )
        .child(
            v_flex()
                .gap_2()
                .child(super::settings::label("Keep in", cx))
                .child(place),
        )
        .child(about(
            "New chats start with these, and a chat can choose for itself from Memory at the \
             bottom of the window. Saved memories are still used unless Keep in leaves them out.",
        ))
        .child(review_row(chat, settings, cx))
}

/// The switch for memory reviews, which run only while memory is saved
/// automatically: otherwise each save is the user's to ask for or say yes
/// to, so it shows as it is, unchangeable, with why.
fn review_row(chat: &Entity<Chat>, settings: &Settings, cx: &App) -> Div {
    let reviewing = settings.memory_review;
    if settings.memory_saving == Saving::Auto {
        return super::settings::toggle_row(
            "memory-review",
            "Review memory",
            "Every 10 of your messages, look back over the session and add what is worth remembering. Each look is a model call on your account.",
            reviewing,
            chat,
            cx,
            std::rc::Rc::new(move |this: &mut Chat| this.store.settings.memory_review = !reviewing),
        );
    }
    let muted = cx.theme().muted_foreground;
    h_flex()
        .justify_between()
        .items_center()
        .gap_4()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(div().text_sm().text_color(muted).child("Review memory"))
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("Runs only while memory is saved Automatically."),
                ),
        )
        .child(
            Switch::new("memory-review")
                .checked(reviewing)
                .disabled(true),
        )
}

/// The answer on a memory save's card that keeps it in `place`.
pub(crate) fn keep_words(place: Scope) -> &'static str {
    match place {
        Scope::Auto => "Save everywhere",
        Scope::Project => "Save for this project",
        Scope::Chat => "Save for this chat",
    }
}

/// When a chat saves and where it keeps memory, as the chip says it: the
/// Private chat preset by its name, anything else by both choices.
fn shown(saving: Saving, scope: Scope) -> String {
    match (saving, scope) {
        (Saving::WhenAsked, Scope::Chat) => "Private chat".to_owned(),
        _ => format!("{} · {}", saving.label(), scope.label()),
    }
}

/// Whether a chat's `own` choice differs from the `default`: one the same
/// as the default changes nothing the user would see.
fn overrides<T: PartialEq>(own: Option<T>, default: T) -> bool {
    own.is_some_and(|own| own != default)
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{keep_words, overrides, shown};
    use letronna_core::memory::{Saving, Scope};

    #[test]
    fn a_private_chat_reads_as_its_name_and_any_other_choice_as_both() {
        assert_eq!(shown(Saving::WhenAsked, Scope::Chat), "Private chat");
        assert_eq!(
            shown(Saving::WhenAsked, Scope::Auto),
            "Only when I ask · Everywhere"
        );
        assert_eq!(
            shown(Saving::Auto, Scope::Chat),
            "Automatically · This chat only"
        );
    }

    #[test]
    fn a_chat_overrides_the_default_only_with_a_choice_of_its_own_that_differs() {
        assert!(!overrides(None, Saving::Ask));
        assert!(!overrides(Some(Saving::Ask), Saving::Ask));
        assert!(overrides(Some(Scope::Chat), Scope::Auto));
    }

    #[test]
    fn each_place_a_card_offers_is_a_save_named_by_it() {
        let words: Vec<&str> = Scope::OFFERED.into_iter().map(keep_words).collect();
        assert_eq!(
            words,
            [
                "Save everywhere",
                "Save for this project",
                "Save for this chat"
            ]
        );
    }
}
