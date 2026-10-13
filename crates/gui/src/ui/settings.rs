use crate::prelude::*;
use letronna_core::jev::KEY_SLOT;

actions!(
    settings,
    [
        PreviousSettingsPage,
        NextSettingsPage,
        SettingsTab,
        SettingsTabBack
    ]
);

type ApplyMode = std::rc::Rc<dyn Fn(plugins::PresenceMode, &mut App)>;

pub(crate) const PAGES: &[(&str, AppIcon)] = &[
    ("Account", AppIcon::User),
    ("Usage", AppIcon::Clock),
    ("Accounts", AppIcon::Plugs),
    ("Search", AppIcon::Search),
    ("Model", AppIcon::Robot),
    ("Appearance", AppIcon::Palette),
    ("Thinking", AppIcon::Sparkles),
    ("Memory", AppIcon::FileText),
    ("Privacy", AppIcon::Shield),
    ("Wallet", AppIcon::Wallet),
    ("Shortcuts", AppIcon::Keyboard),
    ("Plugins", AppIcon::Puzzle),
    ("About", AppIcon::Info),
];

/// How far down the window toasts start: below settings' close button and
/// its focus ring, past settings' 44px margin and the button's row, 12px
/// in and 20px tall. At gpui-component's 50px an error covered both.
pub(crate) const TOASTS_TOP: Pixels = px(88.);

impl Chat {
    /// Open a gateway's row to set it up, or close it if it is already open.
    pub(crate) fn select_gateway(
        &mut self,
        name: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = *self.gateway_pick.read(cx) == name;
        self.gateway_pick.update(cx, |g, cx| {
            *g = if open { String::new() } else { name.into() };
            cx.notify();
        });
        if open {
            return;
        }
        // Custom's address, or the one a gateway of the user's own has.
        let address = base_url(name, &self.store.settings.custom_endpoint);
        self.endpoint
            .update(cx, |input, cx| input.set_value(address, window, cx));
        let key = self.keys.get(key_slot(name)).cloned().unwrap_or_default();
        self.key
            .update(cx, |input, cx| input.set_value(key, window, cx));
        cx.notify();
    }

    /// Use the browser at the address in the field, or with none this
    /// computer's; the one running now is let go of, its pages kept.
    pub(crate) fn save_browser_address(&mut self, cx: &mut Context<Self>) {
        let address = self.browser_address.read(cx).value().trim().to_string();
        let slot = letronna_core::browser::ELSEWHERE;
        if address.is_empty() {
            self.keys.remove(slot);
        } else {
            self.keys.insert(slot.to_owned(), address);
        }
        if self.write_keys(slot) {
            letronna_core::browser::restart();
        }
        cx.notify();
    }

    /// Go back to the browser on this computer.
    pub(crate) fn clear_browser_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.browser_address
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.save_browser_address(cx);
    }

    /// Keep sessions for the number of days in the field, or for ever when it
    /// is empty or 0. Deleting happens at the next start.
    pub(crate) fn save_keep_days(&mut self, cx: &mut Context<Self>) {
        let text = self.keep_days.read(cx).value().trim().to_string();
        let days = if text.is_empty() {
            0
        } else {
            match text.parse::<u32>() {
                Ok(days) => days,
                Err(_) => {
                    self.error = Some(format!(
                        "\"{text}\" is not a number of days. Leave it empty to keep sessions."
                    ));
                    cx.notify();
                    return;
                }
            }
        };
        self.store.settings.keep_days = days;
        self.save();
        cx.notify();
    }

    /// Save the window's key for `slot`, or its removal, and nothing else, so
    /// a key the terminal saved meanwhile is kept and seen here too; one that
    /// could not be saved or removed is said, and the keys shown are the
    /// ones there are.
    fn write_keys(&mut self, slot: &str) -> bool {
        let key = self.keys.get(slot).map(String::as_str);
        let doing = if key.is_some() { "save" } else { "remove" };
        match save_key(slot, key) {
            Ok(keys) => {
                self.keys = keys;
                true
            }
            Err(why) => {
                self.error = Some(format!("Could not {doing} the key: {why}."));
                self.keys = letronna_core::store::load_keys();
                false
            }
        }
    }

    /// Save the open row's key (and URL, for Custom or a gateway of the
    /// user's own), close it, and reload every gateway's models. An address
    /// that cannot be used is said, and nothing is saved.
    pub(crate) fn save_gateway(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editing = self.gateway_pick.read(cx).clone();
        if editing.is_empty() {
            return;
        }
        let address = self.endpoint.read(cx).value().trim().to_string();
        let kept = if is_own(&editing) {
            change_own(&editing, &address).map(|_| ())
        } else if gateway(&editing).base.is_empty() && address.is_empty() {
            self.store.settings.custom_endpoint.clear();
            Ok(())
        } else if gateway(&editing).base.is_empty() {
            own_base(&address).map(|base| self.store.settings.custom_endpoint = base)
        } else {
            Ok(())
        };
        if let Err(why) = kept {
            self.error = Some(why);
            cx.notify();
            return;
        }
        let key = self.key.read(cx).value().trim().to_string();
        let slot = key_slot(&editing).to_string();
        if key.is_empty() {
            self.keys.remove(&slot);
        } else {
            self.keys.insert(slot, key);
        }
        self.write_keys(key_slot(&editing));
        self.save();
        self.gateway_pick.update(cx, |g, cx| {
            g.clear();
            cx.notify();
        });
        self.load_models(window, cx);
    }

    /// Save and register the quick entry shortcut typed on the Shortcuts
    /// page. Left as it was if the spec does not parse or nothing will grab
    /// it -- Wayland refuses every global key, so a failure there is expected
    /// rather than a typo.
    pub(crate) fn apply_hotkey(&mut self, cx: &mut Context<Self>) {
        let spec = self.hotkey.read(cx).value().trim().to_string();
        if spec.is_empty() || !crate::platform::hotkey::rebind(&spec, cx) {
            self.error = Some(
                "Couldn't set that shortcut. Check the spelling (e.g. \"ctrl+shift+space\"), \
                 or use the tray's Quick entry item instead -- Wayland lets nothing grab a \
                 global key at all."
                    .into(),
            );
            cx.notify();
            return;
        }
        self.store.settings.quick_entry_hotkey = spec;
        self.save();
        self.error = None;
        cx.notify();
    }

    /// Forget a gateway's key (and URL, for Custom), or a gateway of the
    /// user's own with its key; its models leave the list.
    pub(crate) fn remove_gateway(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if is_own(name) {
            if let Err(why) = remove_own(name) {
                self.error = Some(why);
            }
            self.keys = letronna_core::store::load_keys();
            self.gateway_pick.update(cx, |g, cx| {
                g.clear();
                cx.notify();
            });
            self.load_models(window, cx);
            return;
        }
        self.keys.remove(key_slot(name));
        if gateway(name).base.is_empty() {
            self.store.settings.custom_endpoint.clear();
        }
        self.write_keys(key_slot(name));
        self.save();
        self.gateway_pick.update(cx, |g, cx| {
            g.clear();
            cx.notify();
        });
        self.load_models(window, cx);
    }

    /// Open the form for adding a gateway of the user's own, its fields
    /// empty, or close it if it is open. It shares the gateways' fields: one
    /// row is open at a time.
    pub(crate) fn open_add_gateway(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = *self.gateway_pick.read(cx) == ADD_ROW;
        self.gateway_pick.update(cx, |g, cx| {
            *g = if open { String::new() } else { ADD_ROW.into() };
            cx.notify();
        });
        if open {
            return;
        }
        for input in [&self.gateway_name, &self.endpoint, &self.key] {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        cx.notify();
    }

    /// Add the gateway in the open form, with its key if one was given, and
    /// reload every gateway's models; or say why it cannot be added.
    pub(crate) fn add_gateway(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.gateway_name.read(cx).value().trim().to_string();
        let address = self.endpoint.read(cx).value().trim().to_string();
        // A name a gateway has already: the field gets the next free one,
        // and Add again takes it. Nothing is replaced or renamed unasked.
        if let Some(free) = free_name(&name) {
            let spelled = holder(&name).unwrap_or(&name).to_owned();
            let free = free_name(&spelled).unwrap_or(free);
            let change = match holder(&name).is_some_and(is_own) {
                true => format!(", or close this and Edit {spelled} to change its address"),
                false => String::new(),
            };
            self.error = Some(format!(
                "There is a gateway called {spelled} already. Press Add gateway again to add this one as {free}{change}."
            ));
            self.gateway_name
                .update(cx, |input, cx| input.set_value(free, window, cx));
            cx.notify();
            return;
        }
        let name = match add_own(&name, &address) {
            Ok((name, _)) => name,
            Err(why) => {
                self.error = Some(why);
                cx.notify();
                return;
            }
        };
        let key = self.key.read(cx).value().trim().to_string();
        if !key.is_empty() {
            self.keys.insert(key_slot(name).to_owned(), key);
            self.write_keys(key_slot(name));
        }
        self.gateway_pick.update(cx, |g, cx| {
            g.clear();
            cx.notify();
        });
        self.load_models(window, cx);
    }

    /// Start a provider's browser sign-in. Its row waits, with a field for
    /// the address the browser ended on when it cannot reach this computer.
    pub(crate) fn provider_sign_in(
        &mut self,
        name: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use letronna_core::providers::auth;
        let flow = match auth::begin(name) {
            Ok(flow) => flow,
            Err(why) => {
                self.error = Some(why);
                cx.notify();
                return;
            }
        };
        cx.open_url(&flow.url);
        self.provider_sign_in = Some(flow.clone());
        self.key
            .update(cx, |input, cx| input.set_value("", window, cx));
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(flow.finish());
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.provider_sign_in = None;
                    match result {
                        Ok(_) => {
                            this.gateway_pick.update(cx, |g, _| g.clear());
                            this.load_models(window, cx);
                        }
                        Err(why) if why == "Sign-in cancelled." => {}
                        Err(why) => this.error = Some(why),
                    }
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }

    /// Hand the waiting sign-in the address pasted into the row's field.
    pub(crate) fn provider_paste(&mut self, cx: &mut Context<Self>) {
        let text = self.key.read(cx).value().trim().to_string();
        if let Some(flow) = &self.provider_sign_in
            && let Err(why) = flow.paste(&text)
        {
            self.error = Some(why);
            cx.notify();
        }
    }

    pub(crate) fn provider_sign_out(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(why) = letronna_core::providers::auth::sign_out(name) {
            self.error = Some(why);
        }
        self.load_models(window, cx);
        cx.notify();
    }

    /// Move the tier at `at` one place earlier, or later, in the order the
    /// router reaches for accounts.
    pub(crate) fn move_tier(&mut self, at: usize, up: bool, cx: &mut Context<Self>) {
        use letronna_core::providers::router;
        let mut order = router::order(&self.store.settings);
        let to = if up { at.checked_sub(1) } else { Some(at + 1) };
        let Some(to) = to.filter(|&to| to < order.len()) else {
            return;
        };
        order.swap(at, to);
        router::reorder(&mut self.store.settings, &order);
        self.save();
        cx.notify();
    }

    /// Open a search service's row to set it up, or close it if it is open.
    /// It shares the gateways' fields: one row is open at a time.
    pub(crate) fn select_search(
        &mut self,
        name: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let row = format!("search:{name}");
        let open = *self.gateway_pick.read(cx) == row;
        self.gateway_pick.update(cx, |g, cx| {
            *g = if open { String::new() } else { row };
            cx.notify();
        });
        let Some(service) = letronna_core::search::service(name).filter(|_| !open) else {
            return;
        };
        let address = letronna_core::search::choice().address(service);
        self.endpoint
            .update(cx, |input, cx| input.set_value(address, window, cx));
        let key = self.keys.get(name).cloned().unwrap_or_default();
        self.key
            .update(cx, |input, cx| input.set_value(key, window, cx));
        cx.notify();
    }

    /// Open Jev's row to give it a key or an address, or close it if open.
    /// It shares the gateways' fields: one row is open at a time.
    pub(crate) fn select_jev(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = *self.gateway_pick.read(cx) == JEV_ROW;
        self.gateway_pick.update(cx, |g, cx| {
            *g = if open { String::new() } else { JEV_ROW.into() };
            cx.notify();
        });
        if open {
            return;
        }
        let address = self.store.settings.jev_endpoint.clone();
        self.endpoint
            .update(cx, |input, cx| input.set_value(address, window, cx));
        let key = self.keys.get(KEY_SLOT).cloned().unwrap_or_default();
        self.key
            .update(cx, |input, cx| input.set_value(key, window, cx));
        cx.notify();
    }

    /// Save Jev's open row: its key in the key store, its address in the
    /// settings, unless the address is one a key must not go to.
    pub(crate) fn save_jev(&mut self, cx: &mut Context<Self>) {
        let address = self.endpoint.read(cx).value().trim().to_string();
        let here = ["http://localhost", "http://127.0.0.1", "http://[::1]"];
        if !address.is_empty()
            && !address.starts_with("https://")
            && !here.iter().any(|h| address.starts_with(h))
        {
            self.error = Some(
                "Jev's address must start with https://, or be on this computer, as its key \
                 goes with every search."
                    .into(),
            );
            cx.notify();
            return;
        }
        let key = self.key.read(cx).value().trim().to_string();
        if key.is_empty() {
            self.keys.remove(KEY_SLOT);
        } else {
            self.keys.insert(KEY_SLOT.to_owned(), key);
        }
        self.write_keys(KEY_SLOT);
        self.store.settings.jev_endpoint = address;
        self.save();
        self.gateway_pick.update(cx, |g, cx| {
            g.clear();
            cx.notify();
        });
        cx.notify();
    }

    /// Forget Jev's key and address, so it is asked through the account.
    pub(crate) fn remove_jev(&mut self, cx: &mut Context<Self>) {
        self.keys.remove(KEY_SLOT);
        self.write_keys(KEY_SLOT);
        self.store.settings.jev_endpoint.clear();
        self.save();
        self.gateway_pick.update(cx, |g, cx| {
            g.clear();
            cx.notify();
        });
        cx.notify();
    }

    /// Where Jev is asked, as its row says it.
    pub(crate) fn jev_status(&self) -> String {
        let address = self.store.settings.jev_endpoint.trim();
        let own_key = self.keys.get(KEY_SLOT).is_some_and(|key| !key.is_empty());
        match (address, own_key) {
            ("", true) => "Your key, at TypeSafe".into(),
            ("", false) => "Your Letronna account".into(),
            (address, _) => address.to_owned(),
        }
    }

    /// Put the open search row's address back to the service's official one.
    pub(crate) fn reset_search_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.gateway_pick.read(cx).clone();
        let Some(service) = open
            .strip_prefix("search:")
            .and_then(letronna_core::search::service)
        else {
            return;
        };
        self.endpoint.update(cx, |input, cx| {
            input.set_value(service.official, window, cx)
        });
    }

    /// Save the open search row's key and address, and search with it from now on.
    pub(crate) fn save_search(&mut self, cx: &mut Context<Self>) {
        let open = self.gateway_pick.read(cx).clone();
        let Some(service) = open
            .strip_prefix("search:")
            .and_then(letronna_core::search::service)
        else {
            return;
        };
        let mut choice = letronna_core::search::choice();
        choice.set_address(service, self.endpoint.read(cx).value().as_ref());
        if !service.self_hosted() {
            let key = self.key.read(cx).value().trim().to_string();
            if key.is_empty() {
                self.keys.remove(service.name);
            } else {
                self.keys.insert(service.name.to_owned(), key);
            }
            self.write_keys(service.name);
        }
        choice.service = service.name.to_owned();
        if let Err(error) = letronna_core::search::choose(&choice) {
            self.error = Some(error);
        }
        self.gateway_pick.update(cx, |g, cx| {
            g.clear();
            cx.notify();
        });
        cx.notify();
    }

    /// Search with the keyless service alone, switched on, so no search goes
    /// through the gateway's paid search; or, `alone` false, choose none, so
    /// the gateway's search goes first where it sells one.
    pub(crate) fn use_search_alone(&mut self, name: &str, alone: bool, cx: &mut Context<Self>) {
        let mut choice = letronna_core::search::choice();
        choice.service = match alone {
            true => name.to_owned(),
            false => String::new(),
        };
        if let Err(error) = letronna_core::search::choose(&choice) {
            self.error = Some(error);
        }
        if alone {
            self.store.settings.keyless_search = true;
            self.save();
        }
        cx.notify();
    }

    /// Forget a search service's key and address; if it was the one in use,
    /// the agent no longer searches.
    pub(crate) fn remove_search(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(service) = letronna_core::search::service(name) else {
            return;
        };
        let mut choice = letronna_core::search::choice();
        choice.set_address(service, "");
        // Taken out even when the window could not read it.
        self.keys.remove(name);
        self.write_keys(name);
        if choice.service == name {
            choice.service.clear();
        }
        if let Err(error) = letronna_core::search::choose(&choice) {
            self.error = Some(error);
        }
        self.gateway_pick.update(cx, |g, cx| {
            g.clear();
            cx.notify();
        });
        cx.notify();
    }

    /// The Model page's Save: the default model, and the session on screen's
    /// when it was changed here, as a pick from the composer changes both.
    pub(crate) fn apply_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let listed = self.fallbacks.read(cx).value().to_string();
        let listed: Vec<String> = (listed.split(','))
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
            .collect();
        // Nothing is saved while one of them does not read, so the page stays
        // open on what was typed, with the error saying which.
        let fallbacks = match letronna_core::fallback::Fallback::parse_all(&listed) {
            Ok(fallbacks) => fallbacks,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.store.settings.fallbacks = fallbacks.iter().map(ToString::to_string).collect();
        let typed = self.model.read(cx).value().to_string();
        if typed != self.store.settings.model {
            let gateway = self.store.settings.gateway.clone();
            self.store.choose_model(self.store.active, &gateway, &typed);
        }
        // Cleared before saving, not after: clearing last threw away the very
        // error that says the save failed.
        self.error = None;
        self.save();
        self.load_models(window, cx);
        self.close_settings(window, cx);
    }

    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Snapshot the window before the overlay covers it. Platforms that
        // cannot sample it return None, which falls back to the plain scrim.
        self.drop_backdrop(cx);
        // macOS 26 reads it from the next frame drawn, which the overlay is
        // see-through for, so that frame is the window without it.
        self.settings_reading = crate::platform::backdrop::ask(window);
        if self.settings_reading {
            window.refresh();
            self.read_backdrop(0, window, cx);
        } else {
            self.settings_backdrop = crate::platform::backdrop::capture(window);
        }
        // Only as it opens: opened again on another page, the keys are in it.
        if !self.settings_open {
            self.settings_before = window.focused(cx);
        }
        self.settings_open = true;
        self.gateway_pick.update(cx, |g, _| g.clear());
        self.settings_closing = false;
        // Somewhere inside the chat has to hold focus for escape to reach
        // it; opened from a menu, nothing does.
        self.settings_focus.focus(window, cx);
        // The terminal may have signed in or out since this last looked.
        self.refresh_account(cx);
        if *self.settings_page.read(cx) == "Usage" {
            self.load_usage(cx);
        }
        if *self.settings_page.read(cx) == "Wallet" {
            self.read_wallet(false, cx);
        }
        // Memory may have been written since, by the model or by hand.
        let on_screen = self.store.conversations.get(self.store.active);
        let workspace = on_screen.and_then(|conversation| conversation.workspace.clone());
        let session = on_screen.map_or(0, |conversation| conversation.id);
        self.memory
            .update(cx, |page, cx| page.refresh(workspace, session, window, cx));
        cx.notify();
    }

    /// Pick up the frame asked for behind settings once it is drawn, a frame
    /// or two from now; a few frames without it, and settings opens over the
    /// plain scrim.
    fn read_backdrop(&mut self, tries: u8, window: &mut Window, cx: &mut Context<Self>) {
        cx.on_next_frame(window, move |this, window, cx| {
            if !this.settings_reading {
                return;
            }
            let read = crate::platform::backdrop::capture(window);
            if read.is_none() && tries < 3 {
                // Drawn again, so there is a frame to read.
                window.refresh();
                return this.read_backdrop(tries + 1, window, cx);
            }
            if read.is_none() {
                crate::platform::backdrop::forget(window);
            }
            // A blur from before wears the window as it was.
            this.drop_backdrop(cx);
            this.settings_backdrop = read;
            this.settings_reading = false;
            cx.notify();
        });
    }

    /// Blur the window behind settings again, as it now looks: once the
    /// theme changes, the snapshot taken on opening wears the old colours.
    /// Settings stays open meanwhile, drawn see-through for the frame read,
    /// and the old blur stays until the new one is in.
    pub(crate) fn retake_backdrop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Only where one was taken: anywhere else settings is over the plain
        // scrim.
        if !self.settings_open || self.settings_closing || self.settings_backdrop.is_none() {
            return;
        }
        self.settings_reading = true;
        window.refresh();
        if crate::platform::backdrop::ask(window) {
            self.read_backdrop(0, window, cx);
        } else {
            // Read off the screen, so once the frame without settings is on
            // it: drawn next frame, and shown by the one after.
            cx.on_next_frame(window, |this, window, cx| this.read_backdrop(0, window, cx));
        }
    }

    pub(crate) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_closing {
            return;
        }
        self.settings_closing = true;
        if std::mem::take(&mut self.settings_reading) {
            crate::platform::backdrop::forget(window);
        }
        // A readiness note on the Tools tab may have sent the user here.
        if self.page == Page::Capabilities {
            self.reload_tools();
        }
        match keys_back(self.settings_before.take(), window) {
            Some(before) => window.focus(&before, cx),
            None => self.focus_composer(window, cx),
        }
        cx.notify();
        // With motion off there is nothing to watch go.
        if cx.reduce_motion() {
            self.settings_open = false;
            self.settings_closing = false;
            self.drop_backdrop(cx);
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(crate::ui::motion::QUICK)
                .await;
            let _ = this.update(cx, |this, cx| {
                this.settings_open = false;
                this.settings_closing = false;
                this.drop_backdrop(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Let go of the blurred snapshot behind settings, and of its place on
    /// the GPU, where gpui keeps every picture it has drawn until told.
    pub(crate) fn drop_backdrop(&mut self, cx: &mut Context<Self>) {
        if let Some(backdrop) = self.settings_backdrop.take() {
            // Once the window is free: gpui leaves out the window being
            // updated, which is where this is called from.
            cx.defer(move |cx| cx.drop_image(backdrop, None));
        }
    }

    /// The settings overlay, mounted while open so both open and close animate.
    pub(crate) fn settings_overlay(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.settings_open {
            return None;
        }
        let closing = self.settings_closing;
        // In at the standard pace, out quicker, so the click after closing
        // never meets it.
        let anim = || {
            if closing {
                crate::ui::motion::exit(crate::ui::motion::QUICK)
            } else {
                crate::ui::motion::enter(crate::ui::motion::STANDARD)
            }
        };
        let blur = self.settings_backdrop.clone().map(|image| {
            img(image)
                .absolute()
                .size_full()
                .object_fit(ObjectFit::Fill)
                .with_animation(
                    if closing { "blur-out" } else { "blur-in" },
                    anim(),
                    move |el, delta| el.opacity(if closing { 1. - delta } else { delta }),
                )
        });
        let scrim = div()
            .id("settings-scrim")
            .absolute()
            .size_full()
            .bg(gpui::black())
            // Going, it leaves a click to what is under it.
            .when(!closing, |scrim| {
                scrim.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_settings(window, cx)),
                )
            })
            .with_animation(
                if closing { "scrim-out" } else { "scrim-in" },
                anim(),
                move |el, delta| {
                    el.opacity(if closing {
                        0.12 * (1. - delta)
                    } else {
                        0.12 * delta
                    })
                },
            );
        let panel = self
            .settings_panel(window, cx)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .with_animation(
                if closing { "panel-out" } else { "panel-in" },
                anim(),
                move |el, delta| {
                    let p = if closing { 1. - delta } else { delta };
                    el.opacity(p).mt(px(-crate::ui::motion::RISE * (1. - p)))
                },
            );
        Some(
            div()
                .absolute()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                // See-through while the frame behind it is read, as that
                // frame is the window without it; still laid out, so it
                // keeps its focus and its animations do not start over.
                .when(self.settings_reading, |overlay| overlay.opacity(0.))
                .children(blur)
                .child(scrim)
                .children(self.grain(&self.grains.settings))
                .child(panel)
                .into_any_element(),
        )
    }

    fn settings_panel(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let endpoint = self.endpoint.clone();
        let model = self.model.clone();
        let key = self.key.clone();
        let hotkey = self.hotkey.clone();
        let chat = cx.entity();
        let pick = self.gateway_pick.clone();
        let presence = self.presence_pick.clone();
        let page = self.settings_page.clone();
        let vp = window.viewport_size();
        let gap = px(44.);
        let height = vp.height - gap * 2.;
        let current_page = *page.read(cx);
        let body_scroll = super::scroll_of(&"settings-body".into(), window, cx);
        // A tab stop a row, so Tab reaches the pages, and Up and Down step
        // through them from the one with the keys.
        let handles: std::rc::Rc<[FocusHandle]> = PAGES
            .iter()
            .map(|(name, _)| {
                window
                    .use_keyed_state(
                        SharedString::from(format!("settings-nav-{name}")),
                        cx,
                        |_, cx| cx.focus_handle().tab_stop(true),
                    )
                    .read(cx)
                    .clone()
            })
            .collect();
        let step = {
            let (page, chat, handles) = (page.clone(), chat.clone(), handles.clone());
            move |down: bool, window: &mut Window, cx: &mut App| {
                let Some(at) = handles.iter().position(|handle| handle.is_focused(window)) else {
                    return;
                };
                let next = if down {
                    (at + 1).min(PAGES.len() - 1)
                } else {
                    at.saturating_sub(1)
                };
                show_page(PAGES[next].0, &page, &chat, cx);
                window.focus(&handles[next], cx);
            }
        };
        let back = step.clone();
        let nav = PAGES.iter().zip(handles.iter()).fold(
            v_flex()
                .w(px(210.))
                .h_full()
                .flex_shrink_0()
                .gap_0p5()
                .p_3()
                .bg(cx.theme().sidebar)
                .border_r_1()
                .border_color(cx.theme().border)
                .key_context("SettingsNav")
                .on_action(move |_: &PreviousSettingsPage, window, cx| back(false, window, cx))
                .on_action(move |_: &NextSettingsPage, window, cx| step(true, window, cx)),
            |nav, ((name, icon), focus)| {
                let page = page.clone();
                let chat = chat.clone();
                let active = *name == current_page;
                nav.child(
                    h_flex()
                        .id(*name)
                        .track_focus(focus)
                        .h(px(30.))
                        .px_2()
                        .gap_2()
                        .items_center()
                        .rounded(cx.theme().radius)
                        // Always there, so the ring the keys bring moves
                        // nothing.
                        .border_1()
                        .border_color(cx.theme().transparent)
                        .cursor_pointer()
                        .text_sm()
                        .font_family(HEADING_FONT)
                        .font_weight(FontWeight::MEDIUM)
                        .when(active, |this| this.bg(cx.theme().sidebar_accent))
                        .focus(|s| s.border_color(cx.theme().ring))
                        .active(|s| s.bg(cx.theme().sidebar_accent.opacity(0.6)))
                        .child(
                            Icon::new(*icon)
                                .size_4()
                                .text_color(cx.theme().muted_foreground),
                        )
                        .child(*name)
                        .on_click(move |_, _, cx| show_page(name, &page, &chat, cx)),
                )
            },
        );
        let body: AnyElement = match current_page {
            "Model" => model_page(&model, &self.fallbacks, &chat, cx).into_any_element(),
            "Appearance" => {
                appearance_page(&chat, &self.store.settings, window, cx).into_any_element()
            }
            "Privacy" => privacy_page(
                &chat,
                &presence,
                &self.browser_address,
                &self.keep_days,
                &self.store.settings,
                cx,
            )
            .into_any_element(),
            "Account" => super::account::account_page(&chat, self, cx).into_any_element(),
            "Usage" => super::usage::usage_page(
                self.usage_report.as_ref(),
                self.usage_insights.as_ref(),
                cx,
            )
            .into_any_element(),
            "Thinking" => thinking_page(&chat, &self.store, window, cx).into_any_element(),
            "Shortcuts" => shortcuts_page(&hotkey, &chat, cx).into_any_element(),
            "Plugins" => plugins_page(
                self.gateway_rows(),
                &pick,
                &endpoint,
                &key,
                &page,
                &chat,
                cx,
            )
            .into_any_element(),
            "Wallet" => super::wallet::wallet_settings(&chat, &self.wallet, cx).into_any_element(),
            "Memory" => v_flex()
                .gap_5()
                .child(heading(
                    "Memory",
                    "What Letronna keeps from one chat to the next, as plain files you can open. What a chat loads goes into each of its requests, so the model keeps it short with its memory tool.",
                    cx,
                ))
                .child(super::remember::defaults(&chat, &self.store.settings, cx))
                .child(self.memory.clone())
                .into_any_element(),
            "About" => about_page(&chat, self, cx).into_any_element(),
            "Search" => search_page(
                self.account_search(),
                self.search_rows(),
                self.store.settings.keyless_search,
                &pick,
                &endpoint,
                &key,
                &chat,
                cx,
            )
            .child(tool_search_section(
                &self.store.settings.tool_search,
                self.jev_status(),
                &pick,
                &endpoint,
                &key,
                &chat,
                cx,
            ))
            .into_any_element(),
            _ => gateway_page(
                self.gateway_rows(),
                &pick,
                &endpoint,
                &key,
                &page,
                &chat,
                cx,
            )
            .child(add_gateway_row(
                &pick,
                &self.gateway_name,
                &endpoint,
                &key,
                &chat,
                cx,
            ))
            .child(tier_order(
                letronna_core::providers::router::order(&self.store.settings),
                &chat,
                cx,
            ))
            .child(local_endpoint(&self.store.settings, &chat, cx))
            .into_any_element(),
        };
        keeps_keys(div(), &self.settings_focus)
            .w(vp.width - gap * 2.)
            .bg(cx.theme().background)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius_lg)
            .child(
                h_flex()
                    .h(height)
                    .items_start()
                    .overflow_hidden()
                    .rounded(cx.theme().radius_lg)
                    .child(nav)
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(
                                div()
                                    .on_children_prepainted(super::follow_keys(&body_scroll))
                                    .id("settings-body")
                                    .track_scroll(&body_scroll)
                                    .size_full()
                                    .overflow_y_scroll()
                                    .px_8()
                                    .py_6()
                                    .child(div().child(body).with_animation(
                                        SharedString::from(format!("page-{current_page}")),
                                        crate::ui::motion::enter(crate::ui::motion::QUICK),
                                        |el, delta| el.opacity(delta).mt(px(6. * (1. - delta))),
                                    )),
                            )
                            .child(
                                div().absolute().top_3().right_3().child(
                                    Button::new("close-settings")
                                        .plain(cx)
                                        .xsmall()
                                        .icon(Icon::new(IconName::Close))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.close_settings(window, cx)
                                        })),
                                ),
                            ),
                    ),
            )
    }
}

/// `panel`'s own focus on `el`, with Tab and Shift-Tab going round the
/// controls inside it: settings is modal, so the keys never reach the
/// window behind it.
fn keeps_keys(el: Div, panel: &FocusHandle) -> Div {
    let (next, back) = (panel.clone(), panel.clone());
    el.track_focus(panel)
        .key_context("Settings")
        .on_action(move |_: &SettingsTab, window, cx| tab_within(&next, false, window, cx))
        .on_action(move |_: &SettingsTabBack, window, cx| tab_within(&back, true, window, cx))
}

/// Move the keys to the next control inside `panel`, or with `back` the one
/// before, from its last round to its first and from its first to its last.
/// Not gpui-component's focus trap, which steps through the whole window
/// behind to come round and gives up after a hundred stops.
fn tab_within(panel: &FocusHandle, back: bool, window: &mut Window, cx: &mut App) {
    if back {
        window.focus_prev(cx);
    } else {
        window.focus_next(cx);
    }
    if panel.contains_focused(window, cx) {
        return;
    }
    // Past one end, so to the other: the panel's own focus comes just before
    // its first control, and its last is the one the next after is outside.
    panel.focus(window, cx);
    window.focus_next(cx);
    let Some(first) = window.focused(cx).filter(|_| back) else {
        return;
    };
    let mut last = first.clone();
    loop {
        window.focus_next(cx);
        match window.focused(cx) {
            Some(at) if at != first && panel.contains_focused(window, cx) => last = at,
            _ => break,
        }
    }
    window.focus(&last, cx);
}

/// Where the keys go as settings closes: back to `before`, where they were
/// as it opened, while that is still drawn in the chat. A menu or the
/// palette it was opened from has gone by then.
fn keys_back(before: Option<FocusHandle>, window: &Window) -> Option<FocusHandle> {
    // Escape is the chat's own, so it is there for anything drawn in it.
    before.filter(|before| window.is_action_available_in(&Dismiss, before))
}

/// Show settings page `name`, reading usage afresh for its page.
fn show_page(name: &'static str, page: &Entity<&'static str>, chat: &Entity<Chat>, cx: &mut App) {
    page.update(cx, |p, cx| {
        *p = name;
        cx.notify();
    });
    if name == "Usage" {
        chat.update(cx, |this, cx| this.load_usage(cx));
    }
    // The terminal may have changed the wallet since this last looked.
    if name == "Wallet" {
        chat.update(cx, |this, cx| this.read_wallet(false, cx));
    }
}

fn plugins_page(
    mut rows: Vec<GatewayRow>,
    pick: &Entity<String>,
    endpoint: &Entity<InputState>,
    key: &Entity<InputState>,
    page: &Entity<&'static str>,
    chat: &Entity<Chat>,
    cx: &App,
) -> Div {
    let mut list = v_flex();
    for info in plugins::list(cx) {
        let toggle = chat.clone();
        let id = info.id;
        let enabled = info.enabled;
        let provider = info.provider;
        let row = h_flex()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(info.name),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .px_1p5()
                                    .rounded(cx.theme().radius)
                                    .bg(cx.theme().secondary)
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if info.builtin { "Built in" } else { "External" }),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(info.description),
                    ),
            )
            .child(
                div().flex_shrink_0().cursor_pointer().child(
                    KeyedSwitch::new(SharedString::from(format!("plugin-{id}")))
                        .checked(enabled)
                        .on_click(move |_, window, cx| {
                            plugins::set_enabled(cx, id, !enabled);
                            toggle.update(cx, |this, cx| {
                                // Its provider's models join the list or leave it.
                                if provider.is_some() {
                                    this.load_models(window, cx);
                                }
                                cx.notify()
                            });
                        }),
                ),
            );
        let settings = if enabled {
            plugins::render_settings(cx, id)
        } else {
            None
        };
        // The provider it offers, with the row the Accounts page would give it.
        let account = provider.filter(|_| enabled).map(|name| {
            let own = rows.iter().position(|row| row.name == name);
            let own = own.map(|i| rows.remove(i)).into_iter().collect();
            gateway_list(own, pick, endpoint, key, page, chat, cx)
        });
        list = list.child(
            v_flex()
                .gap_2()
                .py_3()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(row)
                .children(settings)
                .children(account),
        );
    }
    v_flex()
        .gap_4()
        .child(heading(
            "Plugins",
            "Optional integrations. Toggle one off to unload it. Choices are saved in plugin-switches.json.",
            cx,
        ))
        .child(list)
}

pub(crate) fn heading(title: &'static str, blurb: &'static str, cx: &App) -> Div {
    v_flex()
        .gap_1()
        .child(
            div()
                .text_base()
                .font_family(HEADING_FONT)
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(blurb),
        )
}

pub(crate) fn label(text: &'static str, cx: &App) -> Div {
    div()
        .text_xs()
        .font_family(HEADING_FONT)
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

/// Where the browser runs: this computer, or an address given for one
/// elsewhere.
fn elsewhere_row(chat: &Entity<Chat>, address: &Entity<InputState>, cx: &App) -> Div {
    let now = match letronna_core::browser::remote_place() {
        Some(place) => format!("Now the browser at {place}."),
        None => "Now Chrome or Edge on this computer.".to_owned(),
    };
    let (save, clear) = (chat.clone(), chat.clone());
    v_flex()
        .gap_2()
        .px_3()
        .py_2p5()
        .rounded(cx.theme().radius)
        .bg(cx.theme().secondary.opacity(0.3))
        .child(
            v_flex()
                .gap_0p5()
                .child(div().text_sm().child("Browser elsewhere"))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "A cloud browser's connect address, its key in it as the service \
                             gives it (wss://…?token=…), or Chrome on another machine started \
                             with --remote-debugging-port (http://its-address:9222). Empty, the \
                             browser runs on this computer. {now}"
                        )),
                ),
        )
        .child(
            h_flex()
                .gap_2()
                .child(div().flex_1().child(Input::new(address)))
                .child(
                    Button::new("browser-elsewhere-clear")
                        .plain(cx)
                        .small()
                        .label("This computer")
                        .on_click(move |_, window, cx| {
                            clear.update(cx, |this, cx| this.clear_browser_address(window, cx))
                        }),
                )
                .child(
                    Button::new("browser-elsewhere-save")
                        .primary()
                        .small()
                        .label("Save")
                        .on_click(move |_, _, cx| {
                            save.update(cx, |this, cx| this.save_browser_address(cx))
                        }),
                ),
        )
}

fn keep_row(chat: &Entity<Chat>, days: &Entity<InputState>, kept: u32, cx: &App) -> Div {
    let now = match kept {
        0 => "Now every session is kept.".to_owned(),
        1 => "Now sessions untouched for a day are deleted.".to_owned(),
        n => format!("Now sessions untouched for {n} days are deleted."),
    };
    let save = chat.clone();
    v_flex()
        .gap_2()
        .px_3()
        .py_2p5()
        .rounded(cx.theme().radius)
        .bg(cx.theme().secondary.opacity(0.3))
        .child(
            v_flex()
                .gap_0p5()
                .child(div().text_sm().child("Delete sessions older than N days"))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "Looked for when Letronna starts with no other copy open, at \
                             most once a day. Pinned sessions and one in use are kept. Empty \
                             keeps them all. {now}"
                        )),
                ),
        )
        .child(
            h_flex()
                .gap_2()
                .child(div().flex_1().child(Input::new(days)))
                .child(
                    Button::new("keep-days-save")
                        .primary()
                        .small()
                        .label("Save")
                        .on_click(move |_, _, cx| {
                            save.update(cx, |this, cx| this.save_keep_days(cx))
                        }),
                ),
        )
}

fn privacy_page(
    chat: &Entity<Chat>,
    presence: &Entity<String>,
    address: &Entity<InputState>,
    keep_days: &Entity<InputState>,
    settings: &Settings,
    cx: &App,
) -> Div {
    let (default_mode, shared) = (settings.mode, settings.browser_shared);
    let unseen = settings.browse_unseen;
    let notifications = settings.notifications;
    let current = plugins::PresenceMode::from_str(presence.read(cx));
    let enabled = plugins::list(cx)
        .iter()
        .find(|p| p.id == plugins::DISCORD)
        .map(|p| p.enabled)
        .unwrap_or(true);

    let apply: ApplyMode = {
        let chat = chat.clone();
        let presence = presence.clone();
        std::rc::Rc::new(move |mode: plugins::PresenceMode, cx: &mut App| {
            presence.update(cx, |p, cx| {
                *p = mode.as_str().into();
                cx.notify();
            });
            plugins::emit(cx, plugins::AppEvent::PresenceMode(mode));
            chat.update(cx, |this, cx| {
                this.store.settings.presence = mode.as_str().into();
                this.save();
                cx.notify();
            });
        })
    };

    let level_label = if current == plugins::PresenceMode::Minimal {
        "Hide activity details"
    } else {
        "Show what I am doing"
    };
    let detailed = apply.clone();
    let minimal = apply.clone();
    let level = h_flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px_3()
        .py_2p5()
        .rounded(cx.theme().radius)
        .bg(cx.theme().secondary.opacity(0.3))
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("What to share"),
        )
        .child(
            Button::new("presence-level")
                .plain(cx)
                .small()
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(level_label)
                        .child(Icon::new(IconName::ChevronDown).size_3()),
                )
                .dropdown_menu(move |menu, _, _| {
                    let detailed = detailed.clone();
                    let minimal = minimal.clone();
                    menu.min_w(px(220.))
                        .item(
                            PopupMenuItem::new("Show what I am doing")
                                .checked(current == plugins::PresenceMode::Detailed)
                                .on_click(move |_, _, cx| {
                                    detailed(plugins::PresenceMode::Detailed, cx)
                                }),
                        )
                        .item(
                            PopupMenuItem::new("Hide activity details")
                                .checked(current == plugins::PresenceMode::Minimal)
                                .on_click(move |_, _, cx| {
                                    minimal(plugins::PresenceMode::Minimal, cx)
                                }),
                        )
                }),
        );

    let approvals = h_flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px_3()
        .py_2p5()
        .rounded(cx.theme().radius)
        .bg(cx.theme().secondary.opacity(0.3))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(div().text_sm().child("Default mode"))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            "The mode a session starts in; its chip by the message box changes \
                             it for that session. Bypass writes files and runs commands without \
                             asking, but some commands that are hard to undo, such as recursive \
                             deletes and force pushes, still ask.",
                        ),
                ),
        )
        .child(
            Button::new("default-mode")
                .plain(cx)
                .small()
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(default_mode.label())
                        .child(Icon::new(IconName::ChevronDown).size_3()),
                )
                .dropdown_menu({
                    let chat = chat.clone();
                    move |menu, _, _| {
                        super::composer::offered().fold(menu, |menu, choice| {
                            let chat = chat.clone();
                            menu.item(
                                PopupMenuItem::new(choice.label())
                                    .checked(choice == default_mode)
                                    .on_click(move |_, _, cx| {
                                        chat.update(cx, |this, cx| {
                                            this.store.settings.mode = choice;
                                            this.save();
                                            this.approvals_changed(cx);
                                            cx.notify();
                                        })
                                    }),
                            )
                        })
                    }
                }),
        );

    let browser = h_flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px_3()
        .py_2p5()
        .rounded(cx.theme().radius)
        .bg(cx.theme().secondary.opacity(0.3))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(div().text_sm().child("One browser for every session"))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            "Share the sign-ins and pages of one browser, remembered between \
                             runs. Off, each session gets a private one, forgotten when Letronna \
                             quits.",
                        ),
                ),
        )
        .child(
            div().flex_shrink_0().cursor_pointer().child(
                KeyedSwitch::new("browser-shared")
                    .checked(shared)
                    .on_click({
                        let chat = chat.clone();
                        move |_, _, cx| {
                            chat.update(cx, |this, cx| {
                                this.store.settings.browser_shared = !shared;
                                this.save();
                                letronna_core::browser::share_browser(!shared);
                                cx.notify();
                            })
                        }
                    }),
            ),
        );

    let seen = h_flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px_3()
        .py_2p5()
        .rounded(cx.theme().radius)
        .bg(cx.theme().secondary.opacity(0.3))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(div().text_sm().child("Show the page as the agent browses"))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            "The browser opens beside the chat when the agent uses it. Off, the \
                             agent browses unseen, lighter on memory; open the browser from the \
                             menu to look.",
                        ),
                ),
        )
        .child(div().flex_shrink_0().cursor_pointer().child(
            KeyedSwitch::new("browser-seen").checked(!unseen).on_click({
                let chat = chat.clone();
                move |_, _, cx| {
                    chat.update(cx, |this, cx| {
                        this.store.settings.browse_unseen = !unseen;
                        this.save();
                        cx.notify();
                    })
                }
            }),
        ));

    v_flex()
        .gap_4()
        .child(heading(
            "Agent",
            "Write and command tools ask before they act. Turn that off only for folders you trust.",
            cx,
        ))
        .child(approvals)
        .child(heading(
            "Notifications",
            "What the computer is told while Letronna's window is hidden or behind others.",
            cx,
        ))
        .child(toggle_row(
            "notifications",
            "Notify when Letronna needs you",
            "A reply that ends, a card or question waiting, a helper or scheduled run done. Clicking one opens its session.",
            notifications,
            chat,
            cx,
            std::rc::Rc::new(move |this: &mut Chat| {
                this.store.settings.notifications = !notifications
            }),
        ))
        .child(heading(
            "Sessions",
            "Sessions stay on this computer until you delete them, unless you choose otherwise.",
            cx,
        ))
        .child(keep_row(chat, keep_days, settings.keep_days, cx))
        .child(heading(
            "Browser",
            "Each session browses in a browser of its own, as a separate agent would.",
            cx,
        ))
        .child(browser)
        .child(seen)
        .child(elsewhere_row(chat, address, cx))
        .child(heading(
            "Discord Rich Presence",
            "What Letronna shares with Discord. The integration is turned on or off in the Plugins tab.",
            cx,
        ))
        .child(if enabled {
            level.into_any_element()
        } else {
            div()
                .p_3()
                .rounded(cx.theme().radius)
                .bg(cx.theme().secondary.opacity(0.3))
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Discord Rich Presence is off. Enable it in the Plugins tab to choose what to share.")
                .into_any_element()
        })
}

fn about_page(chat: &Entity<Chat>, this: &Chat, cx: &App) -> Div {
    let muted = cx.theme().muted_foreground;
    let line =
        |text: &'static str, size: f32| div().text_size(px(size)).text_color(muted).child(text);
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .text_center()
        .child(
            img("images/letronna.png")
                .size(px(150.))
                .rounded(cx.theme().radius_lg)
                .mb_3(),
        )
        .child(
            div()
                .font_family(WORDMARK_FONT)
                .italic()
                .text_size(px(36.))
                .child(BRAND),
        )
        .child(line(
            concat!(
                "Version ",
                env!("CARGO_PKG_VERSION"),
                "  ·  build ",
                env!("GIT_HASH")
            ),
            13.,
        ))
        .child(updates(chat, this, cx))
        .child(
            div()
                .pt_6()
                .child(line("© 2026 Gadlus LLC · MIT License", 12.)),
        )
}

/// Where updating stands, the button to look now, and whether to look alone.
fn updates(chat: &Entity<Chat>, this: &Chat, cx: &App) -> Div {
    use crate::ui::update::Updating;
    let looking = matches!(this.updating, Updating::Checking | Updating::Downloading(_));
    let (check, next) = (chat.clone(), chat.clone());
    let manual = this.store.settings.manual_updates;
    v_flex()
        .w(px(420.))
        .pt_3()
        .gap_3()
        .items_center()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(this.update_status()),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("check-updates")
                        .secondary()
                        .small()
                        .label("Check for updates")
                        .disabled(looking)
                        .on_click(move |_, _, cx| {
                            check.update(cx, |this, cx| this.check_for_updates(true, cx))
                        }),
                )
                .when(
                    matches!(this.updating, Updating::Ready(_) | Updating::Out(_)),
                    |row| {
                        let out = matches!(this.updating, Updating::Out(_));
                        row.child(
                            Button::new("apply-update")
                                .primary()
                                .small()
                                .label(if out { "Download" } else { "Restart now" })
                                .disabled(this.busy)
                                .on_click(move |_, _, cx| {
                                    next.update(cx, |this, cx| {
                                        if out {
                                            this.download_update(cx)
                                        } else {
                                            this.restart_to_update(cx)
                                        }
                                    })
                                }),
                        )
                    },
                ),
        )
        .child(div().w_full().text_left().child(toggle_row(
            "auto-update",
            "Update automatically",
            "Download new versions in the background; they install when Letronna restarts.",
            !manual,
            chat,
            cx,
            std::rc::Rc::new(move |this: &mut Chat| this.store.settings.manual_updates = !manual),
        )))
}

/// Night, Paper or the computer's own, and the film grain on or off.
fn appearance_page(
    chat: &Entity<Chat>,
    settings: &Settings,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    const LOOKS: [(&str, &str, &str); 3] = [
        ("", "night", "Night — midnight blue around a dark ink page"),
        (
            "paper",
            "paper",
            "Paper — ink on a light page, for daylight",
        ),
        (
            "system",
            "system",
            "Follow system — Night or Paper as the computer is set",
        ),
    ];

    let mut looks = v_flex().gap_2();
    for (value, id, text) in LOOKS {
        let active = settings.appearance == value;
        // A tab stop of its own, so Tab reaches each choice and Enter or
        // Space takes it.
        let focus = window
            .use_keyed_state(SharedString::from(format!("look-{id}")), cx, |_, cx| {
                cx.focus_handle()
            })
            .read(cx)
            .clone();
        let chat = chat.clone();
        looks = looks.child(
            h_flex()
                .id(id)
                .track_focus(&focus.tab_stop(true))
                .cursor_pointer()
                .items_center()
                .gap_2p5()
                .p_2p5()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(if active {
                    cx.theme().primary
                } else {
                    cx.theme().border
                })
                .focus(|s| s.border_color(cx.theme().ring))
                .child(div().flex_1().min_w_0().text_sm().child(text))
                // Paper's focus ring is its primary blue, so the chosen row
                // is told apart by its tick.
                .when(active, |row| {
                    row.child(
                        Icon::new(IconName::Check)
                            .small()
                            .text_color(cx.theme().primary),
                    )
                })
                .on_click(move |_, window, cx| {
                    chat.update(cx, |this, cx| {
                        this.store.settings.appearance = value.to_string();
                        this.save();
                        crate::theme::apply(value, window, cx);
                        // The blurred snapshot behind settings still wears
                        // the old colours.
                        this.retake_backdrop(window, cx);
                        cx.notify();
                    })
                }),
        );
    }

    let grain = settings.grain;
    let grain = toggle_row(
        "grain",
        "Film grain",
        "A faint grain on the frame and the empty pages, as letronna.ai has. Never over the conversation, code or diffs.",
        grain,
        chat,
        cx,
        std::rc::Rc::new(move |this: &mut Chat| this.store.settings.grain = !grain),
    );
    let motion = settings.motion;
    let motion = toggle_row(
        "motion",
        "Motion",
        "Dialogs, panels and new messages ease in. Always off while your computer is set to reduce motion.",
        motion,
        chat,
        cx,
        std::rc::Rc::new(move |this: &mut Chat| this.store.settings.motion = !motion),
    );

    v_flex()
        .gap_4()
        .child(heading(
            "Appearance",
            "How the window looks. Paper suits daylight; following the system switches with the computer.",
            cx,
        ))
        .child(v_flex().gap_1().child(label("Theme", cx)).child(looks))
        .child(grain)
        .child(motion)
}

fn model_page(
    model: &Entity<InputState>,
    fallbacks: &Entity<InputState>,
    chat: &Entity<Chat>,
    cx: &App,
) -> Div {
    let save = chat.clone();
    v_flex()
        .gap_4()
        .child(heading(
            "Model",
            "The model used for new messages. Pick from the composer, or type an id here.",
            cx,
        ))
        .child(
            v_flex()
                .gap_1()
                .child(label("Default model", cx))
                .child(Input::new(model)),
        )
        .child(
            v_flex()
                .gap_1()
                .child(label("Fallbacks", cx))
                .child(Input::new(fallbacks))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "Asked in order when the model is overloaded, out of credit or gone. \
                             Gateway:model for one on another gateway. One that fails is passed \
                             over for {} minutes.",
                            letronna_core::fallback::COOLDOWN.as_secs() / 60
                        )),
                ),
        )
        .child(
            h_flex().justify_end().child(
                Button::new("save-model")
                    .primary()
                    .small()
                    .label("Save")
                    .on_click(move |_, window, cx| {
                        save.update(cx, |this, cx| this.apply_settings(window, cx))
                    }),
            ),
        )
}

/// How hard the model thinks in the session on screen and new ones, whether
/// that thinking is shown, and whether it may look things up.
///
/// The effort levels are the ones the session's model offers, from its
/// gateway's model list, or low, medium and high when the list does not say.
/// The one marked is what the next request sends, which for a level picked on
/// another model is the nearest this one offers.
fn thinking_page(chat: &Entity<Chat>, store: &Store, window: &mut Window, cx: &mut App) -> Div {
    let settings = &store.settings;
    let (gateway, model) = store.model(store.active);
    let endpoint = base_url(gateway, &settings.custom_endpoint);
    let offered = letronna_core::models::efforts(&endpoint, model);
    let effort = store.effort(store.active);
    let sent = letronna_core::models::clamp(&offered, effort).map(|level| &*level.id);
    let auto = ("", "Whatever the model likes");
    let choices = offered.iter().map(|level| (&*level.id, &*level.label));

    let mut levels = v_flex().gap_2();
    for (at, (value, text)) in std::iter::once(auto).chain(choices).enumerate() {
        let active = sent.unwrap_or_default() == value;
        let (chat, value) = (chat.clone(), value.to_owned());
        levels = levels.child(
            h_flex()
                .id(("effort", at))
                .cursor_pointer()
                .items_center()
                .gap_2p5()
                .p_2p5()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(if active {
                    cx.theme().primary
                } else {
                    cx.theme().border
                })
                .child(div().text_sm().child(text.to_owned()))
                .on_click(move |_, _, cx| {
                    chat.update(cx, |this, cx| {
                        this.store.choose_effort(this.store.active, &value);
                        this.save();
                        cx.notify();
                    })
                }),
        );
    }

    // Read as the page opens, not on every frame: the window draws again for
    // each piece of a streamed reply, and this is a folder of files. The mark
    // is the settings file's, as requests read it; a saved name whose file
    // has gone reads as default, so it is marked so.
    let shown = window.use_keyed_state("styles", cx, |_, _| {
        let current = letronna_core::styles::current().map(|s| s.name);
        (letronna_core::styles::all(), current.unwrap_or_default())
    });
    let (all, current) = shown.read(cx).clone();
    let default = (
        String::new(),
        "Default — as Letronna usually writes".to_owned(),
    );
    let own = all.into_iter().map(|style| {
        let mut text = style.name.clone();
        if !style.about.is_empty() {
            text.push_str(&format!(" — {}", style.about));
        }
        if style.cut {
            text.push_str(&format!(
                " (cut at {} characters)",
                letronna_core::styles::LIMIT
            ));
        }
        (style.name, text)
    });
    let mut styles = v_flex().gap_2();
    for (at, (value, text)) in std::iter::once(default).chain(own).enumerate() {
        let active = current == value;
        let (chat, shown) = (chat.clone(), shown.clone());
        styles = styles.child(
            h_flex()
                .id(("style", at))
                .cursor_pointer()
                .items_center()
                .gap_2p5()
                .p_2p5()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(if active {
                    cx.theme().primary
                } else {
                    cx.theme().border
                })
                .child(div().text_sm().child(text))
                .on_click(move |_, _, cx| {
                    chat.update(cx, |this, cx| {
                        this.store.choose_style(&value);
                        this.save();
                        cx.notify();
                    });
                    shown.update(cx, |(_, current), _| current.clone_from(&value));
                }),
        );
    }

    let showing = settings.thinking;
    let show = toggle_row(
        "show-thinking",
        "Show the thinking",
        "Print the model's reasoning as it arrives, not only the answer it settles on.",
        showing,
        chat,
        cx,
        std::rc::Rc::new(move |this: &mut Chat| this.store.settings.thinking = !showing),
    );

    let searching = settings.search;
    let search = toggle_row(
        "web-search",
        "Search the web",
        "Let the gateway look things up for an answer, and cite the pages it read.",
        searching,
        chat,
        cx,
        std::rc::Rc::new(move |this: &mut Chat| this.store.settings.search = !searching),
    );

    let speaking = settings.voice;
    let voice = toggle_row(
        "voice",
        "Read replies aloud",
        "Speak each reply as it streams, and answer a chat app's voice note with one. \
         Paid from your Letronna credit by the character.",
        speaking,
        chat,
        cx,
        std::rc::Rc::new(move |this: &mut Chat| {
            this.store.settings.voice = !speaking;
            this.match_voice();
        }),
    );
    let verifying = settings.verify_edits;
    let verify = toggle_row(
        "verify-edits",
        "Check changes before finishing",
        "After changing files, ask the model once to build or test before it says the work is done.",
        verifying,
        chat,
        cx,
        std::rc::Rc::new(move |this: &mut Chat| this.store.settings.verify_edits = !verifying),
    );

    v_flex()
        .gap_4()
        .child(heading(
            "Thinking",
            "Models that can be told how hard to think will; the rest carry on as they are.",
            cx,
        ))
        .child(v_flex().gap_1().child(label("Effort", cx)).child(levels))
        .child(v_flex().gap_1().child(label("Style", cx)).child(styles))
        .child(show)
        .child(search)
        .child(voice)
        .child(verify)
}

/// A switch with a name and a line about what it does.
pub(crate) fn toggle_row(
    id: &'static str,
    title: &'static str,
    blurb: &'static str,
    on: bool,
    chat: &Entity<Chat>,
    cx: &App,
    set: std::rc::Rc<dyn Fn(&mut Chat)>,
) -> Div {
    let chat = chat.clone();
    h_flex()
        .justify_between()
        .items_center()
        .gap_4()
        .child(
            // Shrinks and wraps, so a long line never pushes the switch
            // off the page.
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(div().text_sm().child(title))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(blurb),
                ),
        )
        .child(
            div()
                .cursor_pointer()
                .child(KeyedSwitch::new(id).checked(on).on_click(move |_, _, cx| {
                    let set = set.clone();
                    chat.update(cx, |this, cx| {
                        set(this);
                        this.save();
                        cx.notify();
                    })
                })),
        )
}

/// What turning a switch does, given whether it is now on.
type Turn = std::rc::Rc<dyn Fn(&bool, &mut Window, &mut App)>;

/// A switch on a settings page that Tab reaches and Space or Enter turns,
/// as they press a button: one stop, as gpui-component's own, a stop of
/// its own since GPUI Kit 0.7, is kept out of the order.
#[derive(IntoElement)]
pub(crate) struct KeyedSwitch {
    id: SharedString,
    checked: bool,
    on_click: Option<Turn>,
}

impl KeyedSwitch {
    pub(crate) fn new(id: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            checked: false,
            on_click: None,
        }
    }

    pub(crate) fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    /// What turning it does, given whether it is now on.
    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(std::rc::Rc::new(handler));
        self
    }
}

impl RenderOnce for KeyedSwitch {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus = window
            .use_keyed_state(
                SharedString::from(format!("{}-focus", self.id)),
                cx,
                |_, cx| cx.focus_handle(),
            )
            .read(cx)
            .clone();
        let shown = SharedString::from(format!("{}-shown", self.id));
        let (checked, turn) = (self.checked, self.on_click);
        let mut switch = gpui_component::switch::Switch::new(self.id.clone())
            .checked(checked)
            .tab_stop(false);
        if let Some(turn) = turn.clone() {
            switch = switch.on_click(move |on, window, cx| turn(on, window, cx));
        }
        // Ringed as the Capabilities page's switches are, the edge always
        // there so the ring the keys bring moves nothing.
        let row = div().id(self.id).track_focus(&focus.clone().tab_stop(true));
        super::shown_on_focus(row, shown, &focus, window, cx)
            .flex_shrink_0()
            .p_0p5()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().transparent)
            .focus(|s| s.border_color(cx.theme().ring))
            // A click stays the switch's own, turning it as it is pressed
            // and leaving the keys where they were, as a button's does:
            // stopped on the way in, before the switch inside takes them.
            .capture_any_mouse_down(|_, window, _| window.prevent_default())
            .when_some(turn, |this, turn| {
                this.on_click(move |event, window, cx| {
                    if event.is_keyboard() {
                        turn(&!checked, window, cx)
                    }
                })
            })
            .child(switch)
    }
}

/// One gateway as the settings list shows it.
pub(crate) struct GatewayRow {
    name: &'static str,
    /// What it is, or for one of the user's own, where it is.
    blurb: String,
    status: String,
    good: bool,
    /// The row's button: "Add key", "Add URL", "Edit", "Sign in", "Sign
    /// out", or none.
    action: Option<&'static str>,
    pub(crate) set_up: bool,
    /// Plan and usage, as `router::usage` reads them; empty when unknown.
    usage: String,
    /// Signed in to in the browser, not with a key.
    browser: bool,
    caveat: Option<&'static str>,
    /// Its browser sign-in is waiting now.
    waiting: bool,
    /// A plugin offers it, so it is shown under the plugin, not here.
    plugin: bool,
}

impl Chat {
    /// Every gateway with where it stands now, taken before the dialog is
    /// built so the dialog never has to read this entity.
    pub(crate) fn gateway_rows(&self) -> Vec<GatewayRow> {
        gateways()
            .iter()
            .map(|g| {
                let count = self.models.iter().filter(|(n, _)| n == g.name).count();
                let (status, good, set_up) = status(
                    g.name,
                    &self.keys,
                    &self.store.settings.custom_endpoint,
                    count,
                );
                let provider = letronna_core::providers::find(g.name);
                let browser = matches!(
                    provider.map(|p| p.auth()),
                    Some(letronna_core::providers::AuthKind::OAuth(_))
                );
                let action = match g.name {
                    _ if (browser || g.name == LETRONNA) && !set_up => Some("Sign in"),
                    _ if browser || g.name == LETRONNA => Some("Sign out"),
                    _ if provider.map(|p| p.auth())
                        == Some(letronna_core::providers::AuthKind::None) =>
                    {
                        None
                    }
                    "Custom" if !set_up => Some("Add URL"),
                    _ if !set_up => Some("Add key"),
                    _ => Some("Edit"),
                };
                let status = if self.loading_models && set_up {
                    "Checking…".into()
                } else {
                    status
                };
                GatewayRow {
                    name: g.name,
                    blurb: match is_own(g.name) {
                        true => base_url(g.name, ""),
                        false => g.blurb.to_owned(),
                    },
                    status,
                    good,
                    action,
                    set_up,
                    usage: if set_up {
                        letronna_core::providers::router::usage(g.name)
                    } else {
                        String::new()
                    },
                    browser,
                    caveat: provider.and_then(|p| p.caveat()),
                    plugin: provider.and_then(|p| p.plugin()).is_some(),
                    waiting: self
                        .provider_sign_in
                        .as_ref()
                        .is_some_and(|f| f.name == g.name),
                }
            })
            .collect()
    }

    /// The gateways that take a key or a URL, for the setup page's "Use my
    /// own key" step: Letronna is the Sign in step there, and Ollama the
    /// Local model one, so neither belongs in this list too.
    pub(crate) fn key_gateway_rows(&self) -> Vec<GatewayRow> {
        self.gateway_rows()
            .into_iter()
            .filter(|row| row.name != LETRONNA && row.name != "Ollama" && !row.browser)
            .collect()
    }
}

/// One search service as the settings list shows it.
pub(crate) struct SearchRow {
    name: &'static str,
    blurb: &'static str,
    status: &'static str,
    good: bool,
    set_up: bool,
    self_hosted: bool,
    /// Needs nothing set up, so it has no key or address to edit; its
    /// switch is below the list.
    keyless: bool,
    /// Chosen by name, so it is the only one asked.
    chosen: bool,
}

/// Searching through the Letronna account, as the first row of the search
/// list shows it.
pub(crate) struct AccountSearch {
    status: &'static str,
    blurb: &'static str,
    /// Searches go through the account now.
    good: bool,
    signed_in: bool,
    /// A service is chosen, which is then the only one asked.
    chosen: bool,
    /// The last check with the API found no credit to pay for a search.
    broke: bool,
}

impl Chat {
    /// Where searching through the Letronna account stands now.
    pub(crate) fn account_search(&self) -> AccountSearch {
        let chosen = !letronna_core::search::choice().service.is_empty();
        let Some(account) = &self.account else {
            return AccountSearch {
                status: "Signed out",
                blurb: "Sign in to search through your Letronna account, paid by the search from its credit.",
                good: false,
                signed_in: false,
                chosen,
                broke: false,
            };
        };
        // An account never checked with the API has no plan yet, and its
        // credit is not known.
        let credit = account
            .team
            .as_ref()
            .map_or(account.credits, |team| team.credits);
        let broke = !account.plan.is_empty() && credit <= 0;
        let (status, blurb) = match (chosen, broke) {
            (true, _) => (
                "Not in use",
                "Through your account, paid by the search from its credit. The service you chose is used instead.",
            ),
            (false, true) => (
                "No credit",
                "Through your account, paid by the search. Add credit to use it; DuckDuckGo answers until then.",
            ),
            (false, false) => (
                "In use",
                "Through your account, paid by the search from its credit, with DuckDuckGo behind it.",
            ),
        };
        AccountSearch {
            status,
            blurb,
            good: !chosen && !broke,
            signed_in: true,
            chosen,
            broke,
        }
    }

    /// Every search service with where it stands now.
    pub(crate) fn search_rows(&self) -> Vec<SearchRow> {
        let choice = letronna_core::search::choice();
        letronna_core::search::SERVICES
            .iter()
            .map(|s| {
                let (status, good, set_up) =
                    letronna_core::search::status(s.name, &choice, &self.keys);
                SearchRow {
                    name: s.name,
                    blurb: s.blurb,
                    status,
                    good,
                    set_up,
                    self_hosted: s.self_hosted(),
                    keyless: s.keyless,
                    chosen: choice.service == s.name,
                }
            })
            .collect()
    }
}

/// The Letronna row at the head of the search list: where searching through
/// the account stands, and what would change it.
fn account_search_row(row: AccountSearch, chat: &Entity<Chat>, cx: &App) -> Div {
    let dot = if row.good {
        cx.theme().green
    } else {
        cx.theme().muted_foreground.opacity(0.5)
    };
    let chat = chat.clone();
    let action = match (row.signed_in, row.chosen, row.broke) {
        (false, _, _) => Some(
            Button::new("search-account")
                .small()
                .primary()
                .label("Sign in")
                .on_click(move |_, window, cx| {
                    chat.update(cx, |this, cx| this.open_account(window, cx))
                }),
        ),
        (true, true, _) => Some(
            Button::new("search-account")
                .small()
                .plain(cx)
                .label("Use")
                .on_click(move |_, _, cx| {
                    chat.update(cx, |this, cx| this.use_search_alone("", false, cx))
                }),
        ),
        (true, false, true) => Some(
            Button::new("search-account")
                .small()
                .plain(cx)
                .label("Add credit")
                .on_click(|_, _, cx| cx.open_url(&letronna_core::auth::checkout())),
        ),
        (true, false, false) => None,
    };
    h_flex()
        .px_4()
        .py_3()
        .gap_4()
        .items_center()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    div()
                        .text_sm()
                        .font_family(HEADING_FONT)
                        .font_weight(FontWeight::MEDIUM)
                        .child(letronna_core::BRAND),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(row.blurb),
                ),
        )
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .flex_shrink_0()
                .child(div().size(px(7.)).rounded_full().bg(dot))
                .child(
                    div()
                        .text_xs()
                        .text_color(if row.good {
                            cx.theme().foreground
                        } else {
                            cx.theme().muted_foreground
                        })
                        .child(row.status),
                ),
        )
        .child(div().w(px(96.)).flex().justify_end().children(action))
}

#[allow(clippy::too_many_arguments)]
fn search_page(
    account: AccountSearch,
    rows: Vec<SearchRow>,
    keyless_search: bool,
    pick: &Entity<String>,
    endpoint: &Entity<InputState>,
    key: &Entity<InputState>,
    chat: &Entity<Chat>,
    cx: &App,
) -> Div {
    let open = pick.read(cx).clone();
    // DuckDuckGo, in use with no service chosen, then answers only when the
    // account's search cannot.
    let behind_account = account.signed_in && !account.chosen;
    let back_to = match account.signed_in {
        true => "With Letronna",
        false => "With gateway",
    };
    let mut list = v_flex()
        .border_1()
        .border_color(cx.theme().border)
        .rounded(cx.theme().radius)
        .child(account_search_row(account, chat, cx));
    for (i, row) in rows.into_iter().enumerate() {
        let name = row.name;
        let status = match (row.keyless && behind_account, row.status) {
            (true, "In use") => "If Letronna cannot",
            (_, status) => status,
        };
        let editing = open.strip_prefix("search:") == Some(name);
        let dot = if row.good {
            cx.theme().green
        } else {
            cx.theme().muted_foreground.opacity(0.5)
        };
        let action = match (row.set_up, row.self_hosted) {
            (true, _) => "Edit",
            (false, true) => "Add address",
            (false, false) => "Add key",
        };
        let button = {
            let chat = chat.clone();
            Button::new(("search-action", i))
                .small()
                .when(!row.set_up, |b| b.primary())
                .when(row.set_up, |b| b.plain(cx))
                .label(if editing { "Close" } else { action })
                .on_click(move |_, window, cx| {
                    chat.update(cx, |this, cx| this.select_search(name, window, cx))
                })
        };
        // DuckDuckGo has nothing to edit; it can be used alone, which keeps
        // every search off the gateway's paid search, or go back behind it.
        let alone = {
            let (chat, chosen) = (chat.clone(), row.chosen);
            Button::new(("search-alone", i))
                .small()
                .plain(cx)
                .label(if chosen { back_to } else { "Use alone" })
                .on_click(move |_, _, cx| {
                    chat.update(cx, |this, cx| this.use_search_alone(name, !chosen, cx))
                })
        };
        let head = h_flex()
            .px_4()
            .py_3()
            .gap_4()
            .items_center()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        div()
                            .text_sm()
                            .font_family(HEADING_FONT)
                            .font_weight(FontWeight::MEDIUM)
                            .child(name),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(row.blurb),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .flex_shrink_0()
                    .child(div().size(px(7.)).rounded_full().bg(dot))
                    .child(
                        div()
                            .text_xs()
                            .text_color(if row.good {
                                cx.theme().foreground
                            } else {
                                cx.theme().muted_foreground
                            })
                            .child(status),
                    ),
            )
            .child(
                div()
                    .w(px(96.))
                    .flex()
                    .justify_end()
                    .when(!row.keyless, |d| d.child(button))
                    .when(row.keyless, |d| d.child(alone)),
            );
        let mut item = v_flex()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(head);
        if editing {
            let (save, remove, reset) = (chat.clone(), chat.clone(), chat.clone());
            // A service's address starts as its official one; SearXNG has
            // none to go back to.
            let address = match row.self_hosted {
                true => v_flex()
                    .gap_1()
                    .child(label("Address, like http://mac-mini:8888", cx))
                    .child(Input::new(endpoint)),
                false => v_flex().gap_1().child(label("Address", cx)).child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().child(Input::new(endpoint)))
                        .child(
                            Button::new("search-reset")
                                .plain(cx)
                                .small()
                                .label("Reset")
                                .on_click(move |_, window, cx| {
                                    reset.update(cx, |this, cx| {
                                        this.reset_search_address(window, cx)
                                    })
                                }),
                        ),
                ),
            };
            item = item.child(
                v_flex()
                    .px_4()
                    .pb_4()
                    .gap_3()
                    .when(!row.self_hosted, |d| {
                        d.child(
                            v_flex()
                                .gap_1()
                                .child(label("API key", cx))
                                .child(Input::new(key)),
                        )
                    })
                    .child(address)
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .when(row.set_up, |d| {
                                d.child(
                                    Button::new("search-remove")
                                        .plain(cx)
                                        .small()
                                        .label("Remove")
                                        .on_click(move |_, _, cx| {
                                            remove
                                                .update(cx, |this, cx| this.remove_search(name, cx))
                                        }),
                                )
                            })
                            .child(
                                Button::new("search-save")
                                    .primary()
                                    .small()
                                    .label("Save and use")
                                    .on_click(move |_, _, cx| {
                                        save.update(cx, |this, cx| this.save_search(cx))
                                    }),
                            ),
                    ),
            );
        }
        list = list.child(item);
    }
    v_flex()
        .gap_5()
        .child(heading(
            "Search",
            "Where the agent searches the web, asking you before each search. Signed in, it searches through your Letronna account, paid by the search from its credit, with DuckDuckGo when that cannot answer; signed out, through the gateway where it sells search. A service of your own saved below is used instead, at its official address unless you give another, and DuckDuckGo's Use alone keeps every search off what is paid. Keys stay on this machine; the Web toggle is the gateway's own search inside a reply, where it has one.",
            cx,
        ))
        .child(list)
        .child(toggle_row(
            "keyless-search",
            "Search with DuckDuckGo when no search key is set",
            "DuckDuckGo has no official search API. Turned off, the agent searches only with \
             a service set up above, or through the gateway where it sells search.",
            keyless_search,
            chat,
            cx,
            std::rc::Rc::new(move |this: &mut Chat| {
                this.store.settings.keyless_search = !keyless_search
            }),
        ))
}

/// The row of the shared fields that Jev's address and key open in.
const JEV_ROW: &str = "jev";

/// How find_tools ranks the tools a request leaves out, by `mode`, and
/// Jev's row, saying where it is asked (`status`), with its key and address
/// in the shared fields when open.
fn tool_search_section(
    mode: &str,
    status: String,
    pick: &Entity<String>,
    endpoint: &Entity<InputState>,
    key: &Entity<InputState>,
    chat: &Entity<Chat>,
    cx: &App,
) -> Div {
    let choices = [
        (
            "auto",
            "Jev while the chat is on a hosted model; by name for a model on this computer",
        ),
        ("jev", "Jev always, a model on this computer included"),
        ("names", "By their names only, and nothing is sent"),
    ];
    let mode = match mode {
        "jev" | "names" => mode,
        _ => "auto",
    };
    let mut ways = v_flex().gap_2();
    for (at, (value, text)) in choices.into_iter().enumerate() {
        let active = mode == value;
        let chat = chat.clone();
        ways = ways.child(
            h_flex()
                .id(("tool-search", at))
                .cursor_pointer()
                .items_center()
                .gap_2p5()
                .p_2p5()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(if active {
                    cx.theme().primary
                } else {
                    cx.theme().border
                })
                .child(div().text_sm().child(text))
                .on_click(move |_, _, cx| {
                    chat.update(cx, |this, cx| {
                        this.store.settings.tool_search = value.to_owned();
                        this.save();
                        cx.notify();
                    })
                }),
        );
    }
    let editing = pick.read(cx).as_str() == JEV_ROW;
    let open = chat.clone();
    let head = h_flex()
        .px_4()
        .py_3()
        .gap_4()
        .items_center()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    div()
                        .text_sm()
                        .font_family(HEADING_FONT)
                        .font_weight(FontWeight::MEDIUM)
                        .child("Jev"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(status),
                ),
        )
        .child(
            Button::new("jev-edit")
                .small()
                .plain(cx)
                .label(if editing { "Close" } else { "Edit" })
                .on_click(move |_, window, cx| {
                    open.update(cx, |this, cx| this.select_jev(window, cx))
                }),
        );
    let mut row = v_flex()
        .border_1()
        .border_color(cx.theme().border)
        .rounded(cx.theme().radius)
        .child(head);
    if editing {
        let (save, remove) = (chat.clone(), chat.clone());
        row = row.child(
            v_flex()
                .px_4()
                .pb_4()
                .gap_3()
                .child(
                    v_flex()
                        .gap_1()
                        .child(label("API key, blank for your Letronna account", cx))
                        .child(Input::new(key)),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .child(label(
                            "Address, blank for your Letronna account or, with a key, TypeSafe",
                            cx,
                        ))
                        .child(Input::new(endpoint)),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("jev-remove")
                                .plain(cx)
                                .small()
                                .label("Remove")
                                .on_click(move |_, _, cx| {
                                    remove.update(cx, |this, cx| this.remove_jev(cx))
                                }),
                        )
                        .child(
                            Button::new("jev-save")
                                .primary()
                                .small()
                                .label("Save")
                                .on_click(move |_, _, cx| {
                                    save.update(cx, |this, cx| this.save_jev(cx))
                                }),
                        ),
                ),
        );
    }
    v_flex()
        .gap_3()
        .child(heading(
            "Finding tools",
            "A request can leave tools out -- the less used ones in a run nobody watches, an MCP server's when there are many -- and find_tools looks them up. Jev, a small decision model, ranks them by what they do. It is sent the search's words and the tools' names and descriptions, nothing of the chat.",
            cx,
        ))
        .child(ways)
        .child(row)
}

fn shortcuts_page(hotkey: &Entity<InputState>, chat: &Entity<Chat>, cx: &App) -> Div {
    let save = chat.clone();
    v_flex()
        .gap_5()
        .child(heading(
            "Quick Entry",
            "Ctrl+Shift+Space by default. Press it from anywhere to pop a small bar over \
             whatever you are doing, even with Letronna in the tray.",
            cx,
        ))
        .child(
            v_flex()
                .gap_1()
                .child(label("Shortcut", cx))
                .child(Input::new(hotkey)),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(
                    "Modifiers first, then a key, joined with +: \"ctrl+shift+space\". \
                     Wayland lets nothing grab a global key at all; the tray's Quick entry \
                     item works there instead.",
                ),
        )
        .child(
            h_flex().justify_end().child(
                Button::new("save-hotkey")
                    .primary()
                    .small()
                    .label("Save shortcut")
                    .on_click(move |_, _, cx| save.update(cx, |this, cx| this.apply_hotkey(cx))),
            ),
        )
}

/// The gateway rows alone, each open to set up or close on click: the
/// Gateways settings page around it, and the setup page's "Use my own key"
/// step, which shows the same rows filtered to the ones that take a key.
pub(crate) fn gateway_list(
    rows: Vec<GatewayRow>,
    pick: &Entity<String>,
    endpoint: &Entity<InputState>,
    key: &Entity<InputState>,
    page: &Entity<&'static str>,
    chat: &Entity<Chat>,
    cx: &App,
) -> Div {
    let open = pick.read(cx).clone();
    let mut list = v_flex()
        .border_1()
        .border_color(cx.theme().border)
        .rounded(cx.theme().radius);
    for (i, row) in rows.into_iter().enumerate() {
        let name = row.name;
        let editing = open == name;
        let dot = if row.good {
            cx.theme().green
        } else {
            cx.theme().muted_foreground.opacity(0.5)
        };
        let button = row.action.map(|action| {
            let chat = chat.clone();
            let page = page.clone();
            Button::new(("gw-action", i))
                .small()
                .when(!row.set_up, |b| b.primary())
                .when(row.set_up, |b| b.plain(cx))
                .label(if editing { "Close" } else { action })
                .on_click(move |_, window, cx| match (action, name) {
                    ("Sign in", LETRONNA) => {
                        chat.update(cx, |this, cx| this.sign_in(cx));
                        page.update(cx, |p, cx| {
                            *p = "Account";
                            cx.notify();
                        });
                    }
                    ("Sign out", LETRONNA) => chat.update(cx, |this, cx| this.sign_out(cx)),
                    ("Sign out", _) => {
                        chat.update(cx, |this, cx| this.provider_sign_out(name, window, cx))
                    }
                    _ => chat.update(cx, |this, cx| this.select_gateway(name, window, cx)),
                })
        });
        let head = h_flex()
            .px_4()
            .py_3()
            .gap_4()
            .items_center()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        div()
                            .text_sm()
                            .font_family(HEADING_FONT)
                            .font_weight(FontWeight::MEDIUM)
                            .child(name),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(row.blurb),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .flex_shrink_0()
                    .child(div().size(px(7.)).rounded_full().bg(dot))
                    .child(
                        v_flex()
                            .items_end()
                            .text_xs()
                            .child(
                                div()
                                    .text_color(if row.good {
                                        cx.theme().foreground
                                    } else {
                                        cx.theme().muted_foreground
                                    })
                                    .child(row.status),
                            )
                            .when(!row.usage.is_empty(), |d| {
                                d.child(
                                    div()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(row.usage),
                                )
                            }),
                    ),
            )
            .child(div().w(px(84.)).flex().justify_end().children(button));
        let mut item = v_flex()
            .when(i > 0, |d| d.border_t_1().border_color(cx.theme().border))
            .child(head);
        if editing && row.browser {
            item = item.child(browser_sign_in(
                name,
                row.caveat,
                row.waiting,
                key,
                chat,
                cx,
            ));
        } else if editing {
            let custom = gateway(name).base.is_empty();
            let needs_key = gateway(name).needs_key;
            let save = chat.clone();
            let remove = chat.clone();
            item = item.child(
                v_flex()
                    .px_4()
                    .pb_4()
                    .gap_3()
                    .when(custom, |d| {
                        d.child(
                            v_flex()
                                .gap_1()
                                .child(label("Base URL", cx))
                                .child(Input::new(endpoint)),
                        )
                    })
                    .child(
                        v_flex()
                            .gap_1()
                            .child(label(
                                if needs_key {
                                    "API key"
                                } else {
                                    "API key (optional)"
                                },
                                cx,
                            ))
                            .child(Input::new(key).mask_toggle()),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .when(row.set_up, |d| {
                                d.child(
                                    Button::new("gw-remove")
                                        .plain(cx)
                                        .small()
                                        .label("Remove")
                                        .on_click(move |_, window, cx| {
                                            remove.update(cx, |this, cx| {
                                                this.remove_gateway(name, window, cx)
                                            })
                                        }),
                                )
                            })
                            .child(
                                Button::new("gw-save")
                                    .primary()
                                    .small()
                                    .label("Save")
                                    .on_click(move |_, window, cx| {
                                        save.update(cx, |this, cx| this.save_gateway(window, cx))
                                    }),
                            ),
                    ),
            );
        }
        list = list.child(item);
    }
    list
}

/// A browser sign-in's open row: what to know first and the button, or,
/// while the browser has it, a field for the address it ended on.
fn browser_sign_in(
    name: &'static str,
    caveat: Option<&'static str>,
    waiting: bool,
    key: &Entity<InputState>,
    chat: &Entity<Chat>,
    cx: &App,
) -> Div {
    let (go, paste, cancel) = (chat.clone(), chat.clone(), chat.clone());
    let body = v_flex()
        .px_4()
        .pb_4()
        .gap_3()
        .children(caveat.map(|caveat| {
            h_flex()
                .gap_2()
                .items_center()
                .text_xs()
                .child(
                    Icon::new(IconName::Info)
                        .size_3p5()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(caveat)
        }));
    if !waiting {
        return body.child(
            h_flex().justify_end().child(
                Button::new("gw-browser")
                    .primary()
                    .small()
                    .label("Sign in with browser")
                    .on_click(move |_, window, cx| {
                        go.update(cx, |this, cx| this.provider_sign_in(name, window, cx))
                    }),
            ),
        );
    }
    body.child(
        h_flex()
            .gap_2()
            .items_center()
            .child(
                crate::ui::busy::Spinner::new()
                    .small()
                    .color(cx.theme().primary),
            )
            .child(div().text_sm().child("Waiting for the browser.")),
    )
    .child(
        v_flex()
            .gap_1()
            .child(label(
                "Browser could not reach Letronna? Paste the address it ended on",
                cx,
            ))
            .child(Input::new(key)),
    )
    .child(
        h_flex()
            .gap_2()
            .justify_end()
            .child(
                Button::new("gw-cancel")
                    .plain(cx)
                    .small()
                    .label("Cancel")
                    .on_click(move |_, _, cx| {
                        cancel.update(cx, |this, _| {
                            if let Some(flow) = &this.provider_sign_in {
                                flow.cancel();
                            }
                        })
                    }),
            )
            .child(
                Button::new("gw-paste")
                    .primary()
                    .small()
                    .label("Use address")
                    .on_click(move |_, _, cx| paste.update(cx, |this, cx| this.provider_paste(cx))),
            ),
    )
}

/// The tiers in the order the router reaches for accounts when one is rate
/// limited or out of quota, each movable up or down.
fn tier_order(order: Vec<letronna_core::providers::Tier>, chat: &Entity<Chat>, cx: &App) -> Div {
    use letronna_core::providers::Tier;
    let last = order.len().saturating_sub(1);
    let section = v_flex().gap_2().child(heading(
        "When one runs out",
        "A rate-limited or exhausted account hands the request to the next one with the same model, in this order.",
        cx,
    ));
    let mut list = v_flex()
        .border_1()
        .border_color(cx.theme().border)
        .rounded(cx.theme().radius);
    for (i, tier) in order.into_iter().enumerate() {
        let title = match tier {
            Tier::Subscription => "Plans",
            Tier::ApiKey => "API keys",
            Tier::Credit => "Letronna credit",
            Tier::Free => "Free and local",
        };
        let names: Vec<&str> = letronna_core::providers::all()
            .into_iter()
            .filter(|p| letronna_core::providers::offered(p.info().name))
            .map(|p| p.info())
            .filter(|info| info.tier == tier)
            .map(|info| info.name)
            .collect();
        let (up, down) = (chat.clone(), chat.clone());
        list = list.child(
            h_flex()
                .when(i > 0, |d| d.border_t_1().border_color(cx.theme().border))
                .px_4()
                .py_2()
                .gap_3()
                .items_center()
                .child(
                    div()
                        .w(px(16.))
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("{}", i + 1)),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_sm()
                                .font_family(HEADING_FONT)
                                .font_weight(FontWeight::MEDIUM)
                                .child(title),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(names.join(", ")),
                        ),
                )
                .child(
                    Button::new(("tier-up", i))
                        .plain(cx)
                        .xsmall()
                        .icon(Icon::new(IconName::ChevronUp))
                        .disabled(i == 0)
                        .tooltip("Earlier")
                        .on_click(move |_, _, cx| {
                            up.update(cx, |this, cx| this.move_tier(i, true, cx))
                        }),
                )
                .child(
                    Button::new(("tier-down", i))
                        .plain(cx)
                        .xsmall()
                        .icon(Icon::new(IconName::ChevronDown))
                        .disabled(i == last)
                        .tooltip("Later")
                        .on_click(move |_, _, cx| {
                            down.update(cx, |this, cx| this.move_tier(i, false, cx))
                        }),
                ),
        );
    }
    section.child(list)
}

/// The switch for the endpoint other tools reach these accounts on, and,
/// while it is on, its address and token to copy.
fn local_endpoint(settings: &letronna_core::store::Settings, chat: &Entity<Chat>, cx: &App) -> Div {
    use letronna_core::providers::local;
    let on = settings.local_endpoint;
    let port = match settings.local_port {
        0 => local::DEFAULT_PORT,
        port => port,
    };
    let section = v_flex().gap_2().child(heading(
        "Local endpoint",
        "Other tools on this computer can use these accounts through an OpenAI-compatible API. It listens on 127.0.0.1 only and needs the token below.",
        cx,
    ));
    let section = section.child(toggle_row(
        "local-endpoint",
        "Serve these accounts on this computer",
        "Off by default. Set local_port in settings.json to use another port.",
        on,
        chat,
        cx,
        std::rc::Rc::new(move |this: &mut Chat| this.store.settings.local_endpoint = !on),
    ));
    if !on {
        return section;
    }
    let address = format!("http://127.0.0.1:{port}/v1");
    let row = |id: &'static str, label: &'static str, shown: String, copy: String| {
        h_flex()
            .gap_3()
            .items_center()
            .child(
                div()
                    .w(px(64.))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .font_family("monospace")
                    .child(shown),
            )
            .child(Button::new(id).framed(cx).xsmall().label("Copy").on_click(
                move |_, window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                    window.push_notification(
                        gpui_component::notification::Notification::success("Copied."),
                        cx,
                    );
                },
            ))
    };
    let token = local::token();
    let hidden = format!("{}…", &token[..token.len().min(8)]);
    section
        .child(row("local-address", "Address", address.clone(), address))
        .child(row("local-token", "Token", hidden, token))
}

/// The row the form for adding a gateway of the user's own opens in.
const ADD_ROW: &str = "add-gateway";

/// Below the gateways: a button to add one of the user's own, which opens
/// a form for its name, address and key.
fn add_gateway_row(
    pick: &Entity<String>,
    name: &Entity<InputState>,
    endpoint: &Entity<InputState>,
    key: &Entity<InputState>,
    chat: &Entity<Chat>,
    cx: &App,
) -> Div {
    let open = pick.read(cx).as_str() == ADD_ROW;
    let toggle = chat.clone();
    let head = h_flex()
        .px_4()
        .py_3()
        .gap_4()
        .items_center()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    div()
                        .text_sm()
                        .font_family(HEADING_FONT)
                        .font_weight(FontWeight::MEDIUM)
                        .child("A gateway of your own"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("LM Studio, vLLM, a proxy -- any server that answers as OpenAI's API does. Add as many as you like."),
                ),
        )
        .child(
            div().w(px(84.)).flex().justify_end().child(
                Button::new("gw-add-open")
                    .small()
                    .when(open, |b| b.plain(cx))
                    .when(!open, |b| b.primary())
                    .label(if open { "Close" } else { "Add" })
                    .on_click(move |_, window, cx| {
                        toggle.update(cx, |this, cx| this.open_add_gateway(window, cx))
                    }),
            ),
        );
    let mut row = v_flex()
        .border_1()
        .border_color(cx.theme().border)
        .rounded(cx.theme().radius)
        .child(head);
    if open {
        let add = chat.clone();
        row = row.child(
            v_flex()
                .px_4()
                .pb_4()
                .gap_3()
                .child(
                    v_flex()
                        .gap_1()
                        .child(label("Name", cx))
                        .child(Input::new(name)),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .child(label("Base URL, like http://localhost:1234/v1", cx))
                        .child(Input::new(endpoint)),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .child(label("API key (optional)", cx))
                        .child(Input::new(key).mask_toggle()),
                )
                .child(
                    h_flex().justify_end().child(
                        Button::new("gw-add")
                            .primary()
                            .small()
                            .label("Add gateway")
                            .on_click(move |_, window, cx| {
                                add.update(cx, |this, cx| this.add_gateway(window, cx))
                            }),
                    ),
                ),
        );
    }
    row
}

fn gateway_page(
    rows: Vec<GatewayRow>,
    pick: &Entity<String>,
    endpoint: &Entity<InputState>,
    key: &Entity<InputState>,
    page: &Entity<&'static str>,
    chat: &Entity<Chat>,
    cx: &App,
) -> Div {
    v_flex()
        .gap_5()
        .child(heading(
            "Accounts",
            "Every account you connect is used at once: its models join the model list, and each message goes to the account of the model you pick. Keys stay in keys.json on this machine; sign-ins stay in the system vault.",
            cx,
        ))
        .child(gateway_list(
            rows.into_iter().filter(|row| !row.plugin).collect(),
            pick,
            endpoint,
            key,
            page,
            chat,
            cx,
        ))
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{KeyedSwitch, keeps_keys, keys_back};
    use crate::app::Dismiss;
    use gpui::{
        AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement,
        ParentElement as _, Render, Styled as _, TestAppContext, VisualTestContext, Window, div,
        px,
    };
    use gpui_component::{Root, button::Button};

    /// A chat with a status bar of two stops, and settings over it: a page
    /// in its list, a switch and its close button.
    struct Over {
        behind: [FocusHandle; 2],
        panel: FocusHandle,
        page: FocusHandle,
    }

    impl Render for Over {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let stop = |focus: &FocusHandle| div().track_focus(focus).w(px(40.)).h(px(20.));
            div()
                .size_full()
                .key_context("Chat")
                .on_action(|_: &Dismiss, _, _| {})
                .child(stop(&self.behind[0]))
                .child(stop(&self.behind[1]))
                .child(
                    keeps_keys(div(), &self.panel)
                        .child(stop(&self.page))
                        .child(KeyedSwitch::new("grain"))
                        .child(Button::new("close-settings").label("Close")),
                )
        }
    }

    /// The window with settings open, its keys bound as the app binds them.
    fn open(cx: &mut TestAppContext) -> (Entity<Over>, &mut VisualTestContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.bind_keys(
                crate::keys(&gpui::DummyKeyboardMapper)
                    .into_iter()
                    .map(|(binding, _)| binding),
            );
        });
        let over = cx.new(|cx| Over {
            behind: [
                cx.focus_handle().tab_stop(true),
                cx.focus_handle().tab_stop(true),
            ],
            panel: cx.focus_handle(),
            page: cx.focus_handle().tab_stop(true),
        });
        let chat = over.clone();
        let (_, cx) = cx.add_window_view(|window, cx| Root::new(chat, window, cx));
        (over, cx)
    }

    fn focused(cx: &mut VisualTestContext) -> Option<FocusHandle> {
        cx.update(|window, cx| window.focused(cx))
    }

    #[gpui::test]
    fn tab_goes_round_the_controls_in_settings_and_never_behind(cx: &mut TestAppContext) {
        let (over, cx) = open(cx);
        let (panel, page) = cx.read(|cx| (over.read(cx).panel.clone(), over.read(cx).page.clone()));
        // As settings opens: the panel itself holds the keys.
        cx.update(|window, cx| panel.focus(window, cx));
        let mut seen = Vec::new();
        for _ in 0..3 {
            cx.simulate_keystrokes("tab");
            let at = focused(cx).unwrap();
            assert!(cx.update(|window, cx| panel.contains_focused(window, cx)));
            assert!(!seen.contains(&at));
            seen.push(at);
        }
        // The page in the list, the switch, the close button, then round.
        assert_eq!(seen[0], page);
        cx.simulate_keystrokes("tab");
        assert_eq!(focused(cx), Some(page.clone()));
        // Back from the first goes round to the close button, and on back.
        cx.simulate_keystrokes("shift-tab");
        assert_eq!(focused(cx).as_ref(), Some(&seen[2]));
        cx.simulate_keystrokes("shift-tab");
        assert_eq!(focused(cx).as_ref(), Some(&seen[1]));
        // From the panel itself, as it opens, back goes to the last too.
        cx.update(|window, cx| panel.focus(window, cx));
        cx.simulate_keystrokes("shift-tab");
        assert_eq!(focused(cx).as_ref(), Some(&seen[2]));
    }

    #[gpui::test]
    fn the_keys_go_back_where_they_were_when_settings_closes(cx: &mut TestAppContext) {
        let (over, cx) = open(cx);
        let (panel, behind) =
            cx.read(|cx| (over.read(cx).panel.clone(), over.read(cx).behind[1].clone()));
        // The status bar had them as settings opened.
        cx.update(|window, cx| behind.focus(window, cx));
        let before = focused(cx);
        cx.update(|window, cx| panel.focus(window, cx));
        cx.simulate_keystrokes("tab tab");
        // As it closes.
        cx.update(|window, cx| {
            if let Some(back) = keys_back(before, window) {
                window.focus(&back, cx);
            }
        });
        assert_eq!(focused(cx), Some(behind));
        // A menu it was opened from is gone by then, so it gets nothing back.
        let menu = cx.update(|_, cx| cx.focus_handle().tab_stop(true));
        cx.update(|window, _| assert_eq!(keys_back(Some(menu), window), None));
    }
}
