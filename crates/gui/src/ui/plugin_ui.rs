//! What plugins' code shows in the window: a chip each in the status bar, a
//! tab of the side panel drawn from the tree a plugin sends, its notices,
//! and its settings under it on the Plugins page.
//!
//! All of it is drawn here, with the window's own widgets and the theme's
//! colours, from what core keeps of what the plugins said; a plugin
//! describes, and nothing it sends is a page or a program. Core rings once
//! however much changed, and the window looks at most once a frame.

use crate::prelude::*;
use letronna_core::plugins::code::{
    self, Setting, SettingKind,
    ui::{self, Kind, Node, Size, Surfaces, Toast, Tone},
};
use serde_json::Value;

/// The plugins' chips the status bar has room for, and how wide each gets.
const CHIPS: usize = 3;
const CHIP_WIDTH: Pixels = px(180.);
/// How often the window looks at what the plugins show, at most.
const FRAME: std::time::Duration = std::time::Duration::from_millis(16);

/// What the plugins show, as the window last read it.
#[derive(Default)]
pub(crate) struct PluginUi {
    shown: Vec<(String, Surfaces)>,
    /// Notices not put up yet.
    toasts: Vec<Toast>,
}

impl Chat {
    /// Hear core's bell for what the plugins show, and read it again.
    pub(crate) fn watch_plugins(window: &mut Window, cx: &mut Context<Self>) {
        let bell = ui::listen();
        cx.spawn_in(window, async move |this, cx| {
            while bell.recv().await.is_ok() {
                let read = this.update(cx, |this, cx| {
                    this.plugin_ui.shown = ui::snapshot();
                    this.plugin_ui.toasts.extend(ui::take_toasts());
                    // A plugin's failures are reported as hooks' are.
                    this.show_hook_problems();
                    cx.notify();
                });
                if read.is_err() {
                    break;
                }
                cx.background_executor().timer(FRAME).await;
            }
        })
        .detach();
    }

    /// Puts up the notices plugins sent since the last draw.
    pub(crate) fn plugin_toasts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::notification::Notification;
        for toast in std::mem::take(&mut self.plugin_ui.toasts) {
            let text = format!("{} · {}", toast.plugin, toast.text);
            let notice = match toast.kind {
                Kind::Info => Notification::info(text),
                Kind::Warning => Notification::warning(text),
                Kind::Error => Notification::error(text),
            };
            window.push_notification(notice, cx);
        }
    }

    /// What `plugin` shows now.
    fn plugin_shown(&self, plugin: &str) -> Option<&Surfaces> {
        (self.plugin_ui.shown.iter())
            .find(|(name, _)| name == plugin)
            .map(|(_, shown)| shown)
    }

    /// Whether `plugin` has drawn its panel yet.
    pub(crate) fn plugin_drew(&self, plugin: &str) -> bool {
        self.plugin_shown(plugin)
            .is_some_and(|shown| shown.panel.is_some())
    }

    /// A chip for each plugin's status, up to `CHIPS`: a click shows its
    /// panel, when it has one.
    pub(crate) fn plugin_chips(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let panels = code::panels();
        (self.plugin_ui.shown.iter())
            .filter(|(_, shown)| !shown.status.is_empty())
            .take(CHIPS)
            .enumerate()
            .map(|(index, (name, shown))| {
                let panel = panels.iter().find(|(plugin, _)| plugin == name);
                let chip = Button::new(("plugin-chip", index))
                    .plain(cx)
                    .xsmall()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        div()
                            .max_w(CHIP_WIDTH)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(shown.status.clone()),
                    );
                match panel {
                    Some(&(plugin, _)) => chip
                        .tooltip(format!("{name} · show its panel"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.show_tab(crate::ui::panel::PanelTab::Plugin(plugin), window, cx)
                        }))
                        .into_any_element(),
                    None => chip.tooltip(name.clone()).into_any_element(),
                }
            })
            .collect()
    }

    /// `plugin`'s tab of the side panel: what it drew, or that it is starting.
    pub(crate) fn plugin_panel(
        &self,
        plugin: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let body = match self
            .plugin_shown(plugin)
            .and_then(|shown| shown.panel.clone())
        {
            Some(tree) => self.plugin_node(plugin, &tree, &mut 0, window, cx),
            None => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(format!("Starting {plugin}…"))
                .into_any_element(),
        };
        div()
            .id(SharedString::from(format!("plugin-panel-{plugin}")))
            .size_full()
            .overflow_y_scroll()
            .p_3()
            .child(body)
            .into_any_element()
    }

    /// One part of a plugin's panel, and what it holds. `count` numbers the
    /// parts, for the ids of the ones that need one.
    fn plugin_node(
        &self,
        plugin: &'static str,
        node: &Node,
        count: &mut usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        *count += 1;
        let at = *count;
        match node {
            Node::Column { children } => {
                let children: Vec<AnyElement> = (children.iter())
                    .map(|child| self.plugin_node(plugin, child, count, window, cx))
                    .collect();
                v_flex()
                    .w_full()
                    .gap_2()
                    .children(children)
                    .into_any_element()
            }
            Node::Row { children } => {
                let children: Vec<AnyElement> = (children.iter())
                    .map(|child| self.plugin_node(plugin, child, count, window, cx))
                    .collect();
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .children(children)
                    .into_any_element()
            }
            Node::Text {
                text,
                muted,
                mono,
                size,
                tone,
            } => {
                let theme = cx.theme();
                let color = match tone {
                    Tone::Success => theme.success,
                    Tone::Warning => theme.warning,
                    Tone::Error => theme.danger,
                    Tone::Default if *muted => theme.muted_foreground,
                    Tone::Default => theme.foreground,
                };
                let text = div().min_w_0().text_color(color).child(text.clone());
                let text = match size {
                    Size::Small => text.text_xs(),
                    Size::Normal => text.text_sm(),
                    Size::Large => text.text_lg().font_weight(FontWeight::SEMIBOLD),
                };
                text.when(*mono, |text| {
                    text.font_family(theme.mono_font_family.clone())
                })
                .into_any_element()
            }
            Node::Button {
                id,
                label,
                primary,
                disabled,
            } => {
                let pressed = id.clone();
                let button = Button::new(SharedString::from(format!("plugin-{plugin}-{at}")))
                    .small()
                    .label(label.clone())
                    .disabled(*disabled)
                    .on_click(move |_, _, _| code::panel_input(plugin, &pressed, Value::Null));
                match primary {
                    true => button.primary(),
                    false => button.outline(),
                }
                .into_any_element()
            }
            Node::Switch { id, label, value } => {
                let (flipped, now) = (id.clone(), !*value);
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(div().flex_1().min_w_0().text_sm().child(label.clone()))
                    .child(
                        crate::ui::settings::KeyedSwitch::new(format!("plugin-{plugin}-{at}"))
                            .checked(*value)
                            .on_click(move |_, _, _| {
                                code::panel_input(plugin, &flipped, Value::Bool(now))
                            }),
                    )
                    .into_any_element()
            }
            Node::Input {
                id,
                placeholder,
                value,
            } => {
                let key = SharedString::from(format!("plugin-input-{plugin}-{id}"));
                let (placeholder, value) = (placeholder.clone(), value.clone());
                let state = window.use_keyed_state(key, cx, |window, cx| {
                    InputState::new(window, cx)
                        .placeholder(placeholder)
                        .default_value(value)
                });
                let (typed, read) = (id.clone(), state.clone());
                div()
                    .w_full()
                    .capture_key_down(move |event: &KeyDownEvent, _, cx| {
                        if event.keystroke.key == "enter" {
                            let text = read.read(cx).value().to_string();
                            code::panel_input(plugin, &typed, Value::String(text));
                        }
                    })
                    .child(Input::new(&state).small())
                    .into_any_element()
            }
            Node::Progress { value } => div()
                .w_full()
                .child(
                    gpui_component::progress::Progress::new(SharedString::from(format!(
                        "plugin-{plugin}-{at}"
                    )))
                    .value(*value as f32 * 100.),
                )
                .into_any_element(),
            Node::Divider => div()
                .w_full()
                .h(px(1.))
                .bg(cx.theme().border)
                .into_any_element(),
            Node::Link { label, url } => {
                let url = url.clone();
                Button::new(SharedString::from(format!("plugin-{plugin}-{at}")))
                    .link()
                    .small()
                    .label(label.clone())
                    .tooltip(url.clone())
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .into_any_element()
            }
        }
    }

    /// `plugin`'s settings, to change on the Plugins page, or None when it
    /// has none or its code does not run.
    pub(crate) fn plugin_settings_form(
        &self,
        plugin: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let settings = code::settings(plugin)
            .ok()
            .filter(|settings| !settings.is_empty())?;
        let rows: Vec<AnyElement> = (settings.into_iter())
            .map(|(setting, value)| self.plugin_setting(plugin, setting, value, window, cx))
            .collect();
        Some(
            v_flex()
                .w_full()
                .gap_2()
                .pt_1()
                .children(rows)
                .into_any_element(),
        )
    }

    /// One of a plugin's settings, with what changes it.
    fn plugin_setting(
        &self,
        plugin: &str,
        setting: Setting,
        value: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = format!("plugin-setting-{plugin}-{}", setting.key);
        let (plugin, key) = (plugin.to_owned(), setting.key.clone());
        let set = move |this: &mut Chat, value: Value, cx: &mut Context<Chat>| {
            if let Err(why) = code::set_setting(&plugin, &key, value) {
                this.plugins.note = Some(Err(why));
            }
            cx.notify();
        };
        let control = match setting.kind {
            SettingKind::Switch => {
                let on = value.as_bool().unwrap_or(false);
                let chat = cx.entity();
                crate::ui::settings::KeyedSwitch::new(id)
                    .checked(on)
                    .on_click(move |_, _, cx| {
                        chat.update(cx, |this, cx| set(this, Value::Bool(!on), cx));
                    })
                    .into_any_element()
            }
            SettingKind::Choice => {
                let chosen = value.as_str().unwrap_or_default().to_owned();
                let buttons: Vec<AnyElement> = (setting.choices.iter().enumerate())
                    .map(|(index, choice)| {
                        let (picked, set) = (choice.clone(), set.clone());
                        Button::new(SharedString::from(format!("{id}-{index}")))
                            .xsmall()
                            .label(choice.clone())
                            .when(*choice == chosen, |button| button.primary())
                            .when(*choice != chosen, |button| button.outline())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                set(this, Value::String(picked.clone()), cx)
                            }))
                            .into_any_element()
                    })
                    .collect();
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .children(buttons)
                    .into_any_element()
            }
            SettingKind::Text => {
                let now = value.as_str().unwrap_or_default().to_owned();
                let state = window.use_keyed_state(SharedString::from(id), cx, |window, cx| {
                    InputState::new(window, cx)
                        .placeholder("Enter to keep")
                        .default_value(now)
                });
                let (read, chat) = (state.clone(), cx.entity());
                div()
                    .w(px(220.))
                    .capture_key_down(move |event: &KeyDownEvent, _, cx| {
                        if event.keystroke.key == "enter" {
                            let text = read.read(cx).value().to_string();
                            let set = set.clone();
                            chat.update(cx, |this, cx| set(this, Value::String(text), cx));
                        }
                    })
                    .child(Input::new(&state).small())
                    .into_any_element()
            }
        };
        h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .gap_3()
            .child(div().flex_1().min_w_0().text_sm().child(setting.label))
            .child(div().flex_shrink_0().child(control))
            .into_any_element()
    }
}
