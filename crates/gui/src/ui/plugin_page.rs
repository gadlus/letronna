//! The Plugins page: plugins added from git, what each brings, the one
//! consent a plugin's MCP servers and code wait for, a plugin's own
//! settings, and the marketplaces' lists.

use super::PageColumn;
use crate::prelude::*;
use letronna_core::plugins::{self as extensions, Market, Plugin};

/// The Plugins page's state, kept together so `Chat` holds one field.
pub(crate) struct PluginsPage {
    pub(crate) list: Vec<Plugin>,
    /// Why the plugins could not be read; shown above the list.
    pub(crate) error: Option<String>,
    /// Where to add a plugin from: a git URL or owner/repo.
    pub(crate) source: Entity<InputState>,
    /// A marketplace index link to add.
    pub(crate) market_source: Entity<InputState>,
    /// A clone, an update or a marketplace read is running, which waits on
    /// the network.
    pub(crate) busy: bool,
    /// What the last change came to, or why it failed.
    pub(crate) note: Option<Result<String, String>>,
    /// The marketplaces, with what they list as last read: only when asked,
    /// since each is a fetch.
    pub(crate) markets: Vec<Market>,
    /// The marketplaces have been read since the window opened.
    pub(crate) markets_read: bool,
}

impl PluginsPage {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Chat>) -> Self {
        let (list, error) = match extensions::list() {
            Ok(list) => (list, None),
            Err(why) => (Vec::new(), Some(why)),
        };
        Self {
            list,
            error,
            source: cx.new(|cx| {
                InputState::new(window, cx).placeholder("Git URL, or a GitHub repo as owner/repo")
            }),
            market_source: cx.new(|cx| {
                InputState::new(window, cx).placeholder("Link to a marketplace's JSON index")
            }),
            busy: false,
            note: None,
            markets: unread_markets(),
            markets_read: false,
        }
    }
}

impl Chat {
    /// Read the plugins again: the terminal may have changed them.
    pub(crate) fn reload_plugins(&mut self) {
        (self.plugins.list, self.plugins.error) = match extensions::list() {
            Ok(list) => (list, None),
            Err(why) => (Vec::new(), Some(why)),
        };
    }

    /// Run `work` off the UI thread, then say what it came to and read the
    /// plugins and skills again. One at a time: each waits on git or the
    /// network. When it succeeds, the add box is emptied if it still holds
    /// `typed`.
    fn plugin_task(
        &mut self,
        work: impl FnOnce() -> Result<String, String> + Send + 'static,
        typed: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.plugins.busy {
            return;
        }
        self.plugins.busy = true;
        self.plugins.note = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let done = cx.background_executor().spawn(async move { work() }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.plugins.busy = false;
                let source = &this.plugins.source;
                if done.is_ok()
                    && typed.is_some_and(|typed| source.read(cx).value().trim() == typed)
                {
                    source.update(cx, |input, cx| input.set_value("", window, cx));
                }
                this.plugins.note = Some(done);
                this.reload_plugins();
                this.reload_skills();
                cx.notify();
            });
        })
        .detach();
    }

    /// Add the plugin the box names. A failure leaves the box as it was so
    /// it can be corrected.
    pub(crate) fn add_plugin(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let source = self.plugins.source.read(cx).value().trim().to_owned();
        if source.is_empty() {
            return;
        }
        self.install_plugin(source.clone(), Some(source), window, cx);
    }

    /// Install from `source`; `typed` is the add box's text it came from,
    /// if it did, which is left there until the install succeeds.
    fn install_plugin(
        &mut self,
        source: String,
        typed: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.plugin_task(
            move || {
                extensions::add(&source).map(|plugin| match plugin.allowed {
                    true => format!("Added {}", plugin.name),
                    false => format!(
                        "Added {}. Its MCP servers wait until you allow them below.",
                        plugin.name
                    ),
                })
            },
            typed,
            window,
            cx,
        );
    }

    fn update_plugin(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.plugin_task(
            move || {
                extensions::update(&name).map(|kept| match kept {
                    None => format!("{name} is up to date"),
                    Some(kept) => format!(
                        "Updated {name}; the earlier copy is kept in {}",
                        kept.display()
                    ),
                })
            },
            None,
            window,
            cx,
        );
    }

    /// Switch a plugin on or off, or allow its servers. Each restarts the
    /// MCP servers to match, which returns once they are starting.
    fn change_plugin(
        &mut self,
        change: impl FnOnce() -> Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        // A note from before, such as that the servers still wait, would
        // now be out of date.
        self.plugins.note = change().err().map(Err);
        self.reload_plugins();
        self.reload_skills();
        cx.notify();
    }

    /// Allow the servers of the plugin as the page shows it. The consent is
    /// taken for what is on disk now, which an update here or in the
    /// terminal may have changed since the list was read, so a plugin that
    /// changed is shown again instead.
    fn allow_plugin(&mut self, shown: &Plugin, cx: &mut Context<Self>) {
        let name = shown.name.clone();
        let current = extensions::list().map(|now| unchanged(shown, &now));
        self.change_plugin(
            move || match current? {
                true => extensions::allow(&name),
                false => Err(format!(
                    "{name} changed since this page read it; look over its servers again before \
                     allowing them."
                )),
            },
            cx,
        );
    }

    fn remove_plugin(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let body = format!("{name} will be moved to the plugins archive on this device.");
        let chat = cx.entity();
        crate::ui::confirm_delete("Remove plugin", body, window, cx, move |_, cx| {
            chat.update(cx, |this, cx| {
                this.plugins.note = Some(
                    extensions::remove(&name)
                        .map(|kept| format!("Moved {name} to {}", kept.display())),
                );
                this.reload_plugins();
                this.reload_skills();
                cx.notify();
            })
        });
    }

    /// Add the marketplace the box names, then read them all. Not while a
    /// task runs: a read in flight would put back the list it began with,
    /// and one that cannot start would leave this one unread.
    pub(crate) fn add_market(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.plugins.busy {
            return;
        }
        let url = self
            .plugins
            .market_source
            .read(cx)
            .value()
            .trim()
            .to_owned();
        if url.is_empty() {
            return;
        }
        match extensions::add_market(&url) {
            Ok(()) => {
                self.plugins
                    .market_source
                    .update(cx, |input, cx| input.set_value("", window, cx));
                self.plugins.markets.push(Market {
                    url,
                    listings: Ok(Vec::new()),
                });
                self.read_markets(window, cx);
            }
            Err(why) => {
                self.plugins.note = Some(Err(why));
                cx.notify();
            }
        }
    }

    fn remove_market(&mut self, url: String, cx: &mut Context<Self>) {
        match extensions::remove_market(&url) {
            Ok(()) => self.plugins.markets.retain(|market| market.url != url),
            Err(why) => self.plugins.note = Some(Err(why)),
        }
        cx.notify();
    }

    /// Read every marketplace's index afresh, in the background.
    fn read_markets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.plugins.busy {
            return;
        }
        self.plugins.busy = true;
        self.plugins.note = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let read = cx
                .background_executor()
                .spawn(async move { extensions::market() })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.plugins.busy = false;
                match read {
                    Ok(markets) => {
                        this.plugins.markets = markets;
                        this.plugins.markets_read = true;
                    }
                    Err(why) => this.plugins.note = Some(Err(why)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn plugin_row(
        &self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let plugin = &self.plugins.list[index];
        let enabled = plugin.enabled;
        let busy = self.plugins.busy;
        let named = || plugin.name.clone();
        let (switch_name, update_name, remove_name) = (named(), named(), named());
        let shown = plugin.clone();
        let muted = cx.theme().muted_foreground;
        let small = |text: String, color: Hsla| div().text_xs().text_color(color).child(text);
        let mut brings: Vec<Div> = Vec::new();
        if let Some(why) = &plugin.broken {
            brings.push(small(why.clone(), cx.theme().danger));
        }
        let lists = [
            ("Skills", &plugin.skills),
            ("Commands", &plugin.commands),
            ("Styles", &plugin.styles),
        ];
        for (what, names) in lists {
            if !names.is_empty() {
                brings.push(small(format!("{what}: {}", names.join(", ")), muted));
            }
        }
        for (name, target) in &plugin.servers {
            brings.push(small(format!("MCP server {name}: {target}"), muted));
        }
        for (name, base) in &plugin.providers {
            brings.push(small(format!("Provider {name}: {base}"), muted));
        }
        if let Some(code) = &plugin.code {
            brings.push(small(code.consent(), muted));
            for line in code.brings() {
                brings.push(small(capitalised(&line), muted));
            }
            let python = letronna_core::plugins::code::Runtime::Python;
            if code.runtime == python && letronna_core::plugins::code::python_found() == Some(false)
            {
                brings.push(small(
                    "It needs Python 3.9 or later, which was not found on this computer. \
                     Install it from python.org, or set LETRONNA_PYTHON to the one to use."
                        .into(),
                    cx.theme().warning,
                ));
            }
        }
        for why in &plugin.clashes {
            brings.push(small(why.clone(), cx.theme().warning));
        }
        // Its own settings and its panel, once its code may run.
        let running = plugin.enabled && plugin.allowed && plugin.broken.is_none();
        let settings = (running && plugin.code.is_some())
            .then(|| self.plugin_settings_form(&plugin.name, window, cx))
            .flatten();
        let panel = (running.then(letronna_core::plugins::code::panels))
            .into_iter()
            .flatten()
            .find(|(name, _)| *name == plugin.name);
        let heading = match plugin.version.as_str() {
            "" => plugin.name.clone(),
            version => format!("{} {version}", plugin.name),
        };
        v_flex()
            .w_full()
            .gap_2()
            .py_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                // Clipped, not truncated: gpui keeps the first
                                // width a truncated line was measured at, here
                                // too narrow, and drew the name as "…".
                                div()
                                    .min_w_0()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .when(!enabled, |this| this.text_color(muted))
                                    .child(heading),
                            )
                            .child(small(plugin.description.clone(), muted)),
                    )
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .gap_1()
                            .items_center()
                            .when_some(panel, |this, (name, title)| {
                                this.child(
                                    Button::new(("plugin-panel", index))
                                        .plain(cx)
                                        .xsmall()
                                        .icon(Icon::new(AppIcon::Puzzle))
                                        .tooltip(format!("Show {title}"))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            let tab = crate::ui::panel::PanelTab::Plugin(name);
                                            this.show_tab(tab, window, cx)
                                        })),
                                )
                            })
                            .when(!plugin.source.is_empty(), |this| {
                                this.child(
                                    Button::new(("update-plugin", index))
                                        .plain(cx)
                                        .xsmall()
                                        .icon(Icon::new(AppIcon::Reload))
                                        .tooltip("Update")
                                        .disabled(busy)
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.update_plugin(update_name.clone(), window, cx)
                                        })),
                                )
                            })
                            .child(
                                Button::new(("remove-plugin", index))
                                    .plain(cx)
                                    .xsmall()
                                    .icon(Icon::new(AppIcon::Trash))
                                    .tooltip("Remove")
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.remove_plugin(remove_name.clone(), window, cx)
                                    })),
                            )
                            .child(
                                div().ml_1().cursor_pointer().child(
                                    gpui_component::switch::Switch::new(("plugin-on", index))
                                        .checked(enabled)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            let name = switch_name.clone();
                                            this.change_plugin(
                                                move || extensions::set_enabled(&name, !enabled),
                                                cx,
                                            )
                                        })),
                                ),
                            ),
                    ),
            )
            .when(!brings.is_empty(), |this| {
                this.child(v_flex().gap_0p5().children(brings))
            })
            .children(settings)
            .when(waits_for_consent(plugin), |this| {
                this.child(
                    h_flex()
                        .w_full()
                        .gap_3()
                        .items_center()
                        .justify_between()
                        .p_2()
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(cx.theme().warning.opacity(0.5))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(cx.theme().foreground)
                                .child(consent_words(plugin)),
                        )
                        .child(
                            Button::new(("allow-plugin", index))
                                .primary()
                                .xsmall()
                                .flex_shrink_0()
                                .label("Allow")
                                .disabled(busy)
                                .on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        this.allow_plugin(&shown, cx)
                                    }),
                                ),
                        ),
                )
            })
    }

    /// One marketplace and what it lists, each with a button to install it.
    fn market_section(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let market = &self.plugins.markets[index];
        let url = market.url.clone();
        let busy = self.plugins.busy;
        let muted = cx.theme().muted_foreground;
        let rows: Vec<AnyElement> = match &market.listings {
            Err(why) => vec![
                div()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(why.clone())
                    .into_any_element(),
            ],
            Ok(listings) if listings.is_empty() => vec![
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(if self.plugins.markets_read {
                        "It lists no plugins."
                    } else {
                        "Not read yet: Read lists fetches what it lists."
                    })
                    .into_any_element(),
            ],
            Ok(listings) => listings
                .iter()
                .enumerate()
                .map(|(at, listing)| {
                    let installed = self.plugins.list.iter().any(|p| p.name == listing.name);
                    let repo = listing.repo.clone();
                    h_flex()
                        .w_full()
                        .gap_3()
                        .items_center()
                        .justify_between()
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_sm()
                                        .truncate()
                                        .child(format!("{}  ·  {}", listing.name, listing.repo)),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(listing.description.clone()),
                                ),
                        )
                        .child(
                            Button::new(SharedString::from(format!("install-{index}-{at}")))
                                .plain(cx)
                                .xsmall()
                                .flex_shrink_0()
                                .label(if installed { "Installed" } else { "Install" })
                                .disabled(installed || busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.install_plugin(repo.clone(), None, window, cx)
                                })),
                        )
                        .into_any_element()
                })
                .collect(),
        };
        v_flex()
            .gap_2()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .min_w_0()
                            .text_xs()
                            .text_color(muted)
                            .truncate()
                            .child(url.clone()),
                    )
                    .child(
                        Button::new(("remove-market", index))
                            .plain(cx)
                            .xsmall()
                            .icon(Icon::new(AppIcon::Trash))
                            .tooltip("Stop listing this marketplace")
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_market(url.clone(), cx)
                            })),
                    ),
            )
            .children(rows)
    }

    pub(crate) fn plugins_page(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let busy = self.plugins.busy;
        let rows: Vec<AnyElement> = (0..self.plugins.list.len())
            .map(|index| self.plugin_row(index, window, cx).into_any_element())
            .collect();
        let markets: Vec<AnyElement> = (0..self.plugins.markets.len())
            .map(|index| self.market_section(index, cx).into_any_element())
            .collect();
        let note = self.plugins.note.as_ref().map(|note| {
            let (text, color) = match note {
                Ok(text) => (text.clone(), cx.theme().muted_foreground),
                Err(why) => (why.clone(), cx.theme().danger),
            };
            div().text_sm().text_color(color).child(text)
        });
        let problem = self.plugins.error.as_ref().map(|why| {
            div()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(why.clone())
        });
        let empty = (self.plugins.list.is_empty() && problem.is_none()).then(|| {
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No plugins yet. Add one from git, or from a marketplace below.")
        });
        let page = v_flex()
            .page_column()
            .py_6()
            .gap_4()
            .child(super::page_header(
                "Plugins",
                "A plugin brings skills, commands, styles, MCP servers and code in JavaScript \
                 or Python in one git repository. It is checked before it is installed, and its \
                 servers and code run only once you allow them.",
                [],
                cx,
            ))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.plugins.source).small().disabled(busy)),
                    )
                    .child(
                        Button::new("add-plugin")
                            .primary()
                            .small()
                            .label(if busy { "Working…" } else { "Add" })
                            .loading(busy)
                            .disabled(busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.add_plugin(window, cx)),
                            ),
                    ),
            )
            .children(note)
            .children(problem)
            .children(empty)
            .child(v_flex().gap_2().children(rows))
            .child(
                div()
                    .pt_2()
                    .text_base()
                    .font_family(HEADING_FONT)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Marketplaces"),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.plugins.market_source).small()),
                    )
                    .child(
                        Button::new("add-market")
                            .plain(cx)
                            .small()
                            .label("Add")
                            .disabled(busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.add_market(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("read-markets")
                            .plain(cx)
                            .small()
                            .icon(Icon::new(AppIcon::Reload))
                            .label("Read lists")
                            .disabled(busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.read_markets(window, cx)),
                            ),
                    ),
            )
            .children(markets);
        // The whole width scrolls, margins too, with the page in the column.
        div()
            .id("plugins-page")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(page)
    }
}

/// What allowing `plugin` lets happen, for the box that asks.
fn consent_words(plugin: &Plugin) -> &'static str {
    let elsewhere = !(plugin.servers.is_empty() && plugin.providers.is_empty());
    match (plugin.code.is_some(), elsewhere) {
        (true, false) => {
            "Its code runs on this computer as it says above. Nothing runs until you allow \
             it; an update that changes its code asks again."
        }
        (true, true) => {
            "Its code and MCP servers run on this computer, and its providers are sent your \
             messages. None is used until you allow them; an update that changes them asks again."
        }
        (false, _) => {
            "Its MCP servers run the programs above on this computer, and its providers are sent \
             your messages. None is used until you allow them; an update that changes them asks \
             again."
        }
    }
}

/// `line` with its first letter a capital, as a line on its own starts.
fn capitalised(line: &str) -> String {
    let mut letters = line.chars();
    match letters.next() {
        Some(first) => first.to_uppercase().chain(letters).collect(),
        None => String::new(),
    }
}

/// Whether the plugin has servers or code that wait to be allowed. One
/// whose manifest does not read is not allowed either, but has none to show.
pub(crate) fn waits_for_consent(plugin: &Plugin) -> bool {
    !plugin.allowed
        && !(plugin.servers.is_empty() && plugin.providers.is_empty() && plugin.code.is_none())
}

/// Whether the plugin `shown` is in `now` with the version, servers and code
/// the page showed, which is what allowing it consents to.
fn unchanged(shown: &Plugin, now: &[Plugin]) -> bool {
    now.iter().any(|plugin| {
        plugin.name == shown.name
            && plugin.version == shown.version
            && plugin.servers == shown.servers
            && plugin.providers == shown.providers
            && plugin.code == shown.code
    })
}

/// The marketplaces saved, each with nothing read from it yet.
fn unread_markets() -> Vec<Market> {
    extensions::markets()
        .unwrap_or_default()
        .into_iter()
        .map(|url| Market {
            url,
            listings: Ok(Vec::new()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{unchanged, waits_for_consent};
    use letronna_core::plugins::Plugin;
    use std::path::PathBuf;

    fn plugin(version: &str, servers: &[(&str, &str)]) -> Plugin {
        Plugin {
            name: "notes".into(),
            version: version.into(),
            description: String::new(),
            source: "https://example.com/notes.git".into(),
            folder: PathBuf::from("notes"),
            enabled: true,
            allowed: false,
            skills: Vec::new(),
            commands: Vec::new(),
            styles: Vec::new(),
            servers: servers
                .iter()
                .map(|(name, target)| (name.to_string(), target.to_string()))
                .collect(),
            providers: Vec::new(),
            code: None,
            clashes: Vec::new(),
            broken: None,
        }
    }

    #[test]
    fn allowing_waits_for_a_look_at_servers_that_changed_since_shown() {
        let shown = plugin("1.0", &[("notes:db", "node db.js")]);
        assert!(unchanged(&shown, std::slice::from_ref(&shown)));
        let swapped = plugin("1.0", &[("notes:db", "node other.js")]);
        assert!(!unchanged(&shown, &[swapped]));
        let bumped = plugin("1.1", &[("notes:db", "node db.js")]);
        assert!(!unchanged(&shown, &[bumped]));
        assert!(!unchanged(&shown, &[]));
    }

    #[test]
    fn code_waits_for_consent_and_allowing_waits_for_a_look_at_code_that_changed() {
        use letronna_core::plugins::code::{Code, Runtime, Start};
        let code = |network: bool| Code {
            runtime: Runtime::JavaScript,
            main: "plugin.js".into(),
            start: Start::OnUse,
            network,
            conversation: false,
            contributes: Default::default(),
        };
        let shown = Plugin {
            code: Some(code(false)),
            ..plugin("1.0", &[])
        };
        assert!(waits_for_consent(&shown), "code is a program, so it waits");
        assert!(unchanged(&shown, std::slice::from_ref(&shown)));
        let reaching = Plugin {
            code: Some(code(true)),
            ..plugin("1.0", &[])
        };
        assert!(!unchanged(&shown, &[reaching]));
        assert_eq!(super::capitalised("tools: count"), "Tools: count");
    }

    #[test]
    fn a_plugin_whose_manifest_does_not_read_asks_for_no_consent() {
        let waiting = plugin("1.0", &[("notes:db", "node db.js")]);
        assert!(waits_for_consent(&waiting));
        let broken = Plugin {
            broken: Some("plugin.json will not read".into()),
            ..plugin("", &[])
        };
        assert!(!waits_for_consent(&broken));
    }
}
