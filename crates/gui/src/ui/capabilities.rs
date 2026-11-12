//! The Capabilities page: what Letronna can reach, in three tabs. Skills are
//! saved instructions; Tools are Letronna's own tools by family, each with a
//! switch, what it still needs and how often it was called; MCP is each
//! server with its tools, and a sheet that adds one from its README.
//!
//! A switch here is a permission rule in core, so the terminal and helpers
//! keep to it too. What the page shows is read when a tab opens and after a
//! change, not on every redraw, and counting the calls runs off the UI
//! thread; nothing ticks while the page is open.

use super::PageColumn;
use crate::prelude::*;
use gpui_component::switch::Switch;
use gpui_component::tab::{Tab, TabBar};
use letronna_core::mcp_paste::{self, Draft};
use letronna_core::toolset::{self, FAMILIES, Family, Fix, Readiness};

/// How far back the Tools tab counts calls.
const CALL_DAYS: u64 = 30;

actions!(
    letronna,
    [
        /// Open the Capabilities page.
        OpenCapabilities,
        /// Show the tab after the one open, round to the first.
        NextCapabilityTab,
        /// Show the tab before the one open, round to the last.
        PrevCapabilityTab
    ]
);

/// A tab of the Capabilities page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum CapabilityTab {
    #[default]
    Skills,
    Tools,
    Mcp,
}

const TABS: [(CapabilityTab, &str); 3] = [
    (CapabilityTab::Skills, "Skills"),
    (CapabilityTab::Tools, "Tools"),
    (CapabilityTab::Mcp, "MCP"),
];

/// A family on the Tools tab, as read when the tab opened or last changed.
pub(crate) struct FamilyRow {
    family: Family,
    tools: Vec<String>,
    /// Those of `tools` switched off.
    off: Vec<String>,
    readiness: Option<Readiness>,
}

/// What the Tools and MCP tabs hold between redraws.
pub(crate) struct Capabilities {
    pub(crate) tab: CapabilityTab,
    families: Vec<FamilyRow>,
    /// Calls of each tool in the last `CALL_DAYS` days; `None` while they
    /// are being counted.
    calls: Option<std::collections::BTreeMap<String, usize>>,
    /// Every tool switched off, which the MCP tab's switches read.
    pub(crate) off: Vec<String>,
    /// Why the last switch on the Tools tab did not all take.
    tools_note: Option<String>,
    paste: Entity<TextareaState>,
    server_name: Entity<InputState>,
    server_target: Entity<InputState>,
    server_extras: Entity<TextareaState>,
    /// Why the pasted text filled nothing in, or what the last add came to.
    server_note: Option<Result<String, String>>,
}

impl Capabilities {
    /// The tabs' state, and the subscription that fills in the add form
    /// from what is pasted.
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Chat>) -> (Self, Subscription) {
        let paste = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 6)
                .placeholder("Paste a server's JSON from its README, or the line that starts it")
        });
        let (server_name, server_target, server_extras) =
            server_fields(&Draft::default(), window, cx);
        let filled = cx.subscribe_in(&paste, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::Change) {
                this.fill_server(window, cx);
            }
        });
        let state = Self {
            tab: CapabilityTab::default(),
            families: Vec::new(),
            calls: None,
            off: Vec::new(),
            tools_note: None,
            paste,
            server_name,
            server_target,
            server_extras,
            server_note: None,
        };
        (state, filled)
    }
}

impl Chat {
    /// Open the Capabilities page on the tab it last showed.
    pub(crate) fn open_capabilities(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.page = Page::Capabilities;
        self.show_capability(self.capabilities.tab, window, cx);
    }

    /// Show `tab`, reading afresh what it lists.
    pub(crate) fn show_capability(
        &mut self,
        tab: CapabilityTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.capabilities.tab = tab;
        match tab {
            // The terminal or the session's project may have brought skills
            // since the list was read.
            CapabilityTab::Skills => self.reload_skills(),
            CapabilityTab::Tools => {
                self.reload_tools();
                self.count_calls(cx);
            }
            CapabilityTab::Mcp => self.capabilities.off = letronna_core::tools::turned_off(),
        }
        self.focus_composer(window, cx);
        cx.notify();
    }

    /// Show the tab `step` along from the open one, going round.
    pub(crate) fn step_capability(
        &mut self,
        step: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let at = TABS
            .iter()
            .position(|(tab, _)| *tab == self.capabilities.tab)
            .unwrap_or(0);
        let next = (at as isize + step).rem_euclid(TABS.len() as isize) as usize;
        self.show_capability(TABS[next].0, window, cx);
    }

    /// Read the families again: which tools are off and what each needs.
    pub(crate) fn reload_tools(&mut self) {
        let off = letronna_core::tools::turned_off();
        self.capabilities.families = FAMILIES
            .iter()
            .map(|&family| {
                let tools = family.tools();
                FamilyRow {
                    off: tools
                        .iter()
                        .filter(|name| off.contains(name))
                        .cloned()
                        .collect(),
                    tools,
                    family,
                    readiness: family.readiness(),
                }
            })
            .collect();
        self.capabilities.off = off;
    }

    /// Count the calls of each tool off the UI thread: it reads every
    /// session written to in the last `CALL_DAYS` days.
    fn count_calls(&mut self, cx: &mut Context<Self>) {
        let since = backend::now().saturating_sub(CALL_DAYS * 24 * 60 * 60);
        cx.spawn(async move |this, cx| {
            let counts = cx
                .background_executor()
                .spawn(async move { letronna_core::store::tool_calls(since) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.capabilities.calls = Some(counts);
                cx.notify();
            });
        })
        .detach();
    }

    /// Switch a family's tools all off, or all on when every one is off.
    fn switch_family(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(row) = self.capabilities.families.get(index) else {
            return;
        };
        let on = row.off.len() == row.tools.len();
        self.capabilities.tools_note = toolset::switch(&row.tools, on).err();
        self.reload_tools();
        cx.notify();
    }

    /// Go where a family's readiness note is put right.
    fn fix_family(&mut self, fix: Fix, window: &mut Window, cx: &mut Context<Self>) {
        let page = match fix {
            Fix::Search => "Search",
            Fix::Account => "Account",
            Fix::Plugins => "Plugins",
            Fix::Messaging => {
                self.page = Page::Messaging;
                self.focus_composer(window, cx);
                cx.notify();
                return;
            }
        };
        self.settings_page.update(cx, |shown, cx| {
            *shown = page;
            cx.notify();
        });
        self.open_settings(window, cx);
    }

    /// Fill in the add form from what was pasted, leaving the fields as they
    /// are when it reads as no server.
    fn fill_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pasted = self.capabilities.paste.read(cx).value().to_string();
        if pasted.trim().is_empty() {
            // Add empties the box too, and what it came to stays.
            if self
                .capabilities
                .server_note
                .as_ref()
                .is_some_and(Result::is_err)
            {
                self.capabilities.server_note = None;
            }
            cx.notify();
            return;
        }
        let draft = match mcp_paste::read(&pasted) {
            Ok(draft) => draft,
            Err(why) => {
                self.capabilities.server_note = Some(Err(why));
                cx.notify();
                return;
            }
        };
        self.set_server_fields(&draft, window, cx);
        self.capabilities.server_note = None;
        cx.notify();
    }

    /// Add the server the form describes to mcp.json and start it.
    fn add_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = |input: &Entity<InputState>| input.read(cx).value().to_string();
        let draft = Draft {
            name: value(&self.capabilities.server_name),
            target: value(&self.capabilities.server_target),
            extras: self.capabilities.server_extras.read(cx).value().to_string(),
        };
        let added = mcp_paste::add(&draft);
        if added.is_ok() {
            // The sheet goes and the list behind it shows the server
            // starting; a failure stays in the sheet, to be put right there.
            window.close_dialog(cx);
            self.set_server_fields(&Draft::default(), window, cx);
            self.capabilities
                .paste
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.capabilities.server_note =
            Some(added.map(|name| format!("Added {name}. It is starting in the list.")));
        cx.notify();
    }

    /// Open the sheet that adds a server, with the keys in its paste box.
    pub(crate) fn open_server_adder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // What the last add came to is said once, not over a new one.
        self.capabilities.server_note = None;
        let chat = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            dialog
                .title(
                    div()
                        .font_family(WORDMARK_FONT)
                        .font_weight(FontWeight::MEDIUM)
                        .text_xl()
                        .child("Add an MCP server"),
                )
                .w(px(560.))
                .child(server_adder(&chat, cx))
        });
        // After opening, which takes the focus for the sheet itself.
        let paste = self.capabilities.paste.clone();
        paste.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    /// Put `draft` in the add form's fields.
    fn set_server_fields(&mut self, draft: &Draft, window: &mut Window, cx: &mut Context<Self>) {
        let (name, target, extras) = server_fields(draft, window, cx);
        self.capabilities.server_name = name;
        self.capabilities.server_target = target;
        self.capabilities.server_extras = extras;
    }

    pub(crate) fn capabilities_page(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let open = self.capabilities.tab;
        let selected = TABS.iter().position(|(tab, _)| *tab == open).unwrap_or(0);
        let tabs = TABS.iter().map(|&(tab, label)| {
            Tab::new().label(label).on_click(
                cx.listener(move |this, _, window, cx| this.show_capability(tab, window, cx)),
            )
        });
        let bar = TabBar::new("capability-tabs")
            .underline()
            .small()
            .selected_index(selected)
            .children(tabs);
        let body = match open {
            CapabilityTab::Skills => self.skills_page(cx).into_any_element(),
            CapabilityTab::Tools => {
                let tab = self.tools_tab(window, cx);
                scrolled("tools-tab", tab, window, cx)
            }
            CapabilityTab::Mcp => {
                let tab = self.mcp_tab(window, cx);
                scrolled("mcp-tab", tab, window, cx)
            }
        };
        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                v_flex()
                    .page_column()
                    .pt_6()
                    .gap_3()
                    .child(super::page_header(
                        Page::Capabilities.title(),
                        Page::Capabilities.blurb(),
                        [],
                        cx,
                    ))
                    .child(bar),
            )
            .child(body)
    }

    fn tools_tab(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let rows: Vec<Div> = (0..self.capabilities.families.len())
            .map(|index| self.family_row(index, window, cx))
            .collect();
        v_flex()
            .gap_4()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        "Letronna's own tools, by what they reach. A family switched off is \
                         not offered to the model here, in the terminal or to a helper.",
                    ),
            )
            .children(
                self.capabilities
                    .tools_note
                    .clone()
                    .map(|why| div().text_sm().text_color(cx.theme().danger).child(why)),
            )
            .child(v_flex().children(rows))
    }

    fn family_row(&self, index: usize, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let row = &self.capabilities.families[index];
        let total = row.tools.len();
        let on = row.off.len() < total;
        let calls = match &self.capabilities.calls {
            None => "Counting calls…".to_owned(),
            Some(counts) => match row.family.calls(counts) {
                0 => format!("No calls in {CALL_DAYS} days"),
                1 => format!("1 call in {CALL_DAYS} days"),
                n => format!("{n} calls in {CALL_DAYS} days"),
            },
        };
        let mut names = row.tools.join(" · ");
        if on && !row.off.is_empty() {
            names.push_str(&format!(" — {} of {total} off", row.off.len()));
        }
        let readiness = row.readiness.map(|ready| match ready.fix {
            Some(fix) => Button::new(("family-fix", index))
                .framed(cx)
                .xsmall()
                .label(ready.note)
                .on_click(cx.listener(move |this, _, window, cx| this.fix_family(fix, window, cx)))
                .into_any_element(),
            None => div()
                .text_xs()
                .text_color(cx.theme().warning)
                .child(ready.note)
                .into_any_element(),
        });
        // Nothing to switch until a plugin adds a tool.
        let switch = (total > 0).then(|| {
            let flip = cx.listener(move |this, _, _, cx| this.switch_family(index, cx));
            let id = SharedString::from(format!("family-{index}"));
            switch(id, on, window, cx, flip)
        });
        h_flex()
            .items_center()
            .justify_between()
            .gap_3()
            .py_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .when(!on, |this| this.text_color(cx.theme().muted_foreground))
                                    .child(row.family.title()),
                            )
                            .children(readiness),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(row.family.blurb()),
                    )
                    .when(!names.is_empty(), |this| {
                        this.child(
                            div()
                                .text_xs()
                                .font_family(MONO_FONT)
                                .text_color(cx.theme().muted_foreground)
                                .child(names),
                        )
                    }),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .gap_3()
                    .items_center()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(calls),
                    )
                    .children(switch),
            )
    }

    fn mcp_tab(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let chat = cx.entity();
        // What the last add came to; why one failed is said in the sheet.
        let added = match &self.capabilities.server_note {
            Some(Ok(added)) => Some(added.clone()),
            _ => None,
        };
        super::mcp::mcp_page(&chat, &self.capabilities.off, added, window, cx)
    }
}

/// What the sheet that adds a server holds: the paste box, the fields it
/// fills in to be checked, and Add. Nothing starts until Add is pressed.
fn server_adder(chat: &Entity<Chat>, cx: &App) -> Div {
    let state = &chat.read(cx).capabilities;
    let failed = match &state.server_note {
        Some(Err(why)) => Some(why.clone()),
        _ => None,
    };
    let field = |label: &'static str, input: &Entity<InputState>| {
        v_flex()
            .gap_1p5()
            .child(super::settings::label(label, cx))
            .child(Input::new(input).small().w_full())
    };
    let add = {
        let chat = chat.clone();
        Button::new("add-server")
            .primary()
            .small()
            .icon(Icon::new(AppIcon::PlugsConnected))
            .label("Add and connect")
            .on_click(move |_, window, cx| {
                chat.update(cx, |this, cx| this.add_server(window, cx));
            })
    };
    let cancel = Button::new("add-server-cancel")
        .framed(cx)
        .small()
        .label("Cancel")
        .on_click(|_, window, cx| window.close_dialog(cx));
    v_flex()
        .gap_3()
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(
                    "Paste what its README gives, a JSON block or an npx line, and check the \
                     fields it fills in.",
                ),
        )
        .child(Textarea::new(&state.paste).small().w_full())
        .child(
            h_flex()
                .gap_3()
                .items_end()
                .child(
                    field("Name", &state.server_name)
                        .w(px(160.))
                        .flex_shrink_0(),
                )
                .child(
                    field("Command or address", &state.server_target)
                        .flex_1()
                        .min_w_0(),
                ),
        )
        .child(
            v_flex()
                .gap_1p5()
                .child(super::settings::label(
                    "Variables or headers, one a line",
                    cx,
                ))
                .child(Textarea::new(&state.server_extras).small().w_full()),
        )
        .children(failed.map(|why| div().text_sm().text_color(cx.theme().danger).child(why)))
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .pt_1()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("A key stays in mcp.json on this computer."),
                )
                .child(cancel)
                .child(add),
        )
}

/// The add form's name, command and variables fields, holding `draft`.
/// They are built afresh rather than set: setting the text of a box without
/// the focus starts its cursor blinking, redrawing the window twice a second
/// until the box has been focused and left.
fn server_fields(
    draft: &Draft,
    window: &mut Window,
    cx: &mut Context<Chat>,
) -> (
    Entity<InputState>,
    Entity<InputState>,
    Entity<TextareaState>,
) {
    let name = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("github")
            .default_value(draft.name.clone())
    });
    let target = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("npx -y @modelcontextprotocol/server-github, or https://…")
            .default_value(draft.target.clone())
    });
    let extras = cx.new(|cx| {
        TextareaState::new(window, cx)
            .auto_grow(1, 4)
            .placeholder("API_KEY=… for a command, or Authorization: Bearer … for an address")
            .default_value(draft.extras.clone())
    });
    (name, target, extras)
}

/// A switch that Tab reaches and Enter or Space flips, as a click does:
/// one stop, as gpui-component's own, a stop of its own since GPUI Kit
/// 0.7, is kept out of the order and a click never gives it the keys.
pub(crate) fn switch(
    id: SharedString,
    on: bool,
    window: &mut Window,
    cx: &mut App,
    flip: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let focus = window
        .use_keyed_state(SharedString::from(format!("{id}-focus")), cx, |_, cx| {
            cx.focus_handle()
        })
        .read(cx)
        .clone();
    let control = div()
        .id(id.clone())
        .track_focus(&focus.clone().tab_stop(true));
    super::shown_on_focus(control, format!("{id}-shown").into(), &focus, window, cx)
        .flex_shrink_0()
        .p_0p5()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().transparent)
        .focus(|s| s.border_color(cx.theme().ring))
        .cursor_pointer()
        .capture_any_mouse_down(|_, window, _| window.prevent_default())
        .child(
            Switch::new(SharedString::from(format!("{id}-switch")))
                .checked(on)
                .tab_stop(false),
        )
        .on_click(flip)
}

/// `content` under the tabs, scrolling on its own across the whole width,
/// margins too, with the content in the column.
fn scrolled(id: &'static str, content: Div, window: &mut Window, cx: &mut App) -> AnyElement {
    let scroll = super::scroll_of(&id.into(), window, cx);
    div()
        .on_children_prepainted(super::follow_keys(&scroll))
        .id(id)
        .track_scroll(&scroll)
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .child(v_flex().page_column().py_6().child(content))
        .into_any_element()
}
