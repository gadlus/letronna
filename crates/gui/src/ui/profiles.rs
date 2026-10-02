//! Profiles, switched and made from the account chip's menu.
//!
//! A profile is a data folder, chosen at runtime through
//! `letronna_core::profiles`: switching to one, or cloning it into a new
//! one, is core's job. This file's job is only to ask for a name and what to
//! clone from, and to bring the window's own state -- the store, the model
//! and key fields, the skills list, who is signed in -- back in step with
//! whichever folder is active now.

use crate::prelude::*;
use gpui_component::menu::PopupMenu;
use std::{cell::Cell, rc::Rc};

impl Chat {
    /// Why switching profiles right now would leave something stranded
    /// mid-way through, or `None` when it is safe.
    fn profile_switch_blocked(&self) -> Option<&'static str> {
        if self.busy {
            return Some("Stop the reply before switching profiles.");
        }
        // Each would end into a session of the old profile's by its id.
        if !self.runs.is_empty() {
            return Some("Stop the runs beside the chat before switching profiles.");
        }
        if self.helpers_working() > 0 {
            return Some("Wait for helpers to finish before switching profiles.");
        }
        if letronna_core::jobs::list().iter().any(|job| job.running) {
            return Some("Wait for background jobs to finish before switching profiles.");
        }
        None
    }

    /// Point `data_dir` at `name`'s folder and bring the window's state in
    /// step with it, without restarting the app.
    pub(crate) fn switch_profile(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if letronna_core::profiles::active() == name {
            return;
        }
        if let Some(why) = self.profile_switch_blocked() {
            self.error = Some(why.into());
            cx.notify();
            return;
        }
        if let Err(err) = letronna_core::profiles::switch(name) {
            self.error = Some(err);
            cx.notify();
            return;
        }
        self.reload_profile(window, cx);
    }

    /// Ask for a name and what to clone it from, then switch to it.
    pub(crate) fn open_new_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.profile_name
            .update(cx, |input, cx| input.set_value("", window, cx));
        let profiles = letronna_core::profiles::list();
        // The clone source, by its place in `profiles`; `None` starts empty.
        let picked: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));
        let name_input = self.profile_name.clone();
        let chat = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let (chat, name_input) = (chat.clone(), name_input.clone());
            let (profiles, picked) = (profiles.clone(), picked.clone());
            let current = picked.get();
            let choice = |key: usize, index: Option<usize>, label: String| {
                let picked = picked.clone();
                let chosen = current == index;
                Button::new(("profile-source", key))
                    .small()
                    // Picking the chosen one again changes nothing, and a
                    // disabled primary would read as the one not on offer.
                    .when(chosen, |b| b.primary())
                    .label(label)
                    .on_click(move |_, window, _| {
                        picked.set(index);
                        window.refresh();
                    })
            };
            let mut row = h_flex()
                .gap_2()
                .flex_wrap()
                .child(choice(0, None, "Start empty".into()));
            for (i, profile) in profiles.iter().enumerate() {
                row = row.child(choice(i + 1, Some(i), profile.name.clone()));
            }
            dialog
                .title(div().font_family(HEADING_FONT).child("New profile"))
                .w(px(420.))
                .confirm()
                .child(Input::new(&name_input))
                .child(
                    v_flex()
                        .mt_3()
                        .gap_2()
                        .child(super::settings::label("Clone from", cx))
                        .child(row),
                )
                .on_ok(move |_, window, cx| {
                    let name = name_input.read(cx).value().trim().to_string();
                    let clone_from = picked.get().map(|i| profiles[i].name.clone());
                    chat.update(cx, |this, cx| {
                        this.create_profile(name, clone_from, window, cx)
                    });
                    true
                })
        });
        self.profile_name
            .update(cx, |input, cx| input.focus(window, cx));
    }

    /// Make the profile the New profile dialog asked for, then switch to it.
    /// The copy runs off the window's thread: a profile with a browser
    /// folder or a long history is a lot of files.
    fn create_profile(
        &mut self,
        name: String,
        clone_from: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(why) = self.profile_switch_blocked() {
            self.error = Some(why.into());
            cx.notify();
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let made = cx
                .background_executor()
                .spawn(async move { letronna_core::profiles::create(&name, clone_from.as_deref()) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                // A reply may have started while the copy ran, and would
                // now be cut off from the folder it began in.
                let profile = match made {
                    Ok(profile) => profile,
                    Err(err) => {
                        this.error = Some(err);
                        cx.notify();
                        return;
                    }
                };
                if let Some(why) = this.profile_switch_blocked() {
                    this.error = Some(format!("Profile \"{}\" was made. {why}", profile.name));
                    this.profile_label = title_label();
                    cx.notify();
                    return;
                }
                if let Err(err) = letronna_core::profiles::switch(&profile.name) {
                    this.error = Some(err);
                    cx.notify();
                    return;
                }
                this.reload_profile(window, cx);
            });
        })
        .detach();
    }

    /// Re-read everything the active profile owns -- its sessions, keys,
    /// model, skills, account -- now that `data_dir` points at it, and drop
    /// what belonged to the old profile's sessions rather than carry ids
    /// that mean nothing here.
    ///
    /// The chat app listeners are restarted on the new profile's settings, so
    /// the old profile's bots stop feeding messages into this one.
    fn reload_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Only listed, as at start: each is read in when it is opened.
        let mut store = letronna_core::store::load_listing();
        store.active = store
            .conversations
            .iter()
            .position(|c| c.id == store.settings.active_id)
            .unwrap_or(store.conversations.len().saturating_sub(1));
        if store.conversations.is_empty() {
            store.conversations.push(Conversation {
                id: store.reserve_id(),
                title: "New session".into(),
                updated: backend::now(),
                ..Default::default()
            });
        }
        store.assign_ids();
        store.active = store.active.min(store.conversations.len() - 1);
        if store.settings.gateway.is_empty() {
            store.settings.gateway = GATEWAYS[0].name.into();
        }
        if store.settings.endpoint.is_empty() {
            store.settings.endpoint = std::env::var("AGENT_BASE_URL")
                .unwrap_or_else(|_| gateway(&store.settings.gateway).base.into());
        }
        if store.settings.model.is_empty() {
            store.settings.model = std::env::var("AGENT_MODEL").unwrap_or_default();
        }
        let keys = letronna_core::store::load_keys();
        let saved_key = keys
            .get(key_slot(&store.settings.gateway))
            .cloned()
            .or_else(|| std::env::var("AGENT_API_KEY").ok())
            .unwrap_or_default();

        crate::theme::apply(&store.settings.appearance, window, cx);
        letronna_core::browser::share_browser(store.settings.browser_shared);

        self.endpoint.update(cx, |input, cx| {
            input.set_value(store.settings.endpoint.clone(), window, cx)
        });
        self.model.update(cx, |input, cx| {
            input.set_value(store.settings.model.clone(), window, cx)
        });
        self.key
            .update(cx, |input, cx| input.set_value(saved_key, window, cx));
        self.composer
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.gateway_pick.update(cx, |g, cx| {
            *g = store.settings.gateway.clone();
            cx.notify();
        });
        self.presence_pick.update(cx, |p, cx| {
            *p = store.settings.presence.clone();
            cx.notify();
        });

        let workspace = store.conversations[store.active].workspace.clone();
        let moved = letronna_core::skills::migrate();
        let (skills, skills_error) = match letronna_core::skills::list(workspace.as_deref()) {
            Ok(skills) => (skills, moved.err()),
            Err(why) => (Vec::new(), Some(why)),
        };

        // The old profile's runner would go on running its jobs, and saving
        // them into its folder, so the new profile's takes over instead.
        letronna_core::schedule::stop();
        let (runner, events) = letronna_core::schedule::Runner::start();
        self.schedules = runner.list();
        self.schedule_runner = runner;
        Self::watch_schedules(events, cx);
        // Whatever is kept here is saved back, so left alone these would put
        // the old profile's tokens into the new one.
        self.messaging = letronna_core::messaging::load();
        let messaging = &self.messaging;
        for (input, value) in [
            (&self.token_input, messaging.telegram.token.clone()),
            (&self.webhook_input, messaging.discord.webhook.clone()),
            (
                &self.discord_token_input,
                messaging.discord.bot_token.clone(),
            ),
            (&self.discord_owner_input, messaging.discord.owner.clone()),
            (
                &self.discord_servers_input,
                messaging.discord.servers.join(", "),
            ),
            (
                &self.browser_address,
                keys.get(letronna_core::browser::ELSEWHERE)
                    .cloned()
                    .unwrap_or_default(),
            ),
        ] {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        self.pairing = None;
        self.hook_name = None;
        self.bot_name = None;
        self.discord_bot_name = None;
        self.draft_for = store.conversations[store.active].id;
        self.store = store;
        self.keys = keys;
        self.media_choices = letronna_core::media::choices();
        self.skills = skills;
        self.skills_error = skills_error;
        self.editing_skill = None;

        // Keyed by the old profile's session ids, which mean nothing here.
        self.open.clear();
        self.drafts.clear();
        self.attachments.clear();
        self.modes.clear();
        self.session_allow = letronna_core::gate::Grants::new();
        self.agent_asks.clear();
        self.thinking_open.clear();
        self.runs_open.clear();
        self.unseen.clear();
        self.found.clear();
        self.editing_message = None;
        // Sessions the old profile opened for a chat app, by ids that another
        // profile's sessions may share.
        self.remote = Default::default();
        self.follow_ups.clear();
        self.returned_follow_ups = None;
        // The rules file is re-read by its date, which may match.
        self.rules_read = None;
        self.reveal = None;
        self.revealed = None;
        self.aside = None;
        self.stop_video(window);
        self.viewing = None;
        self.steering = None;

        self.page = Page::Chat;
        self.error = None;
        self.teams = Vec::new();
        self.profile_label = title_label();
        self.restart_messaging(cx);
        self.refresh_account(cx);
        self.load_models(window, cx);
        self.focus_composer(window, cx);
        cx.notify();
    }
}

/// The active profile's name, small, next to the account chip -- shown only
/// once there is more than one, so a single-profile window still just says
/// nothing about it. Read from disk here, not while drawing, which happens
/// on every frame of a streaming reply.
pub(crate) fn title_label() -> Option<String> {
    (letronna_core::profiles::list().len() > 1).then(letronna_core::profiles::active)
}

/// The "Profile: …" item in the account chip's menu: a submenu listing
/// every profile, the active one checked, and a way to make another.
pub(crate) fn menu(
    menu: PopupMenu,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
    chat: Entity<Chat>,
) -> PopupMenu {
    let active = letronna_core::profiles::active();
    let profiles = letronna_core::profiles::list();
    let label = format!("Profile: {active}");
    menu.submenu(label, window, cx, move |menu, _, _| {
        let mut menu = menu.min_w(px(180.));
        for profile in &profiles {
            let on = profile.name == active;
            let (chat, name) = (chat.clone(), profile.name.clone());
            menu = menu.item(
                PopupMenuItem::new(profile.name.clone())
                    .when(on, |item| item.icon(Icon::new(IconName::Check)))
                    .on_click(move |_, window, cx| {
                        chat.update(cx, |this, cx| this.switch_profile(&name, window, cx))
                    }),
            );
        }
        let new_profile = chat.clone();
        menu.separator().item(
            PopupMenuItem::new("New profile")
                .icon(Icon::new(IconName::Plus))
                .on_click(move |_, window, cx| {
                    new_profile.update(cx, |this, cx| this.open_new_profile(window, cx))
                }),
        )
    })
}
