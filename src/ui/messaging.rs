//! The Messaging page: a channel list beside the setup for the selected one.

use crate::core::messaging;
use crate::prelude::*;

/// Name, one-line description, badge colour, and whether it is built yet.
/// The ones that are not are listed so the page says plainly what it does and
/// does not do, rather than looking like Telegram is the only idea anyone had.
const CHANNELS: &[(&str, &str, u32, bool)] = &[
    (
        "Telegram",
        "Message Letronna from a Telegram chat, without exposing anything to the internet.",
        0x2aabee,
        true,
    ),
    (
        "Discord",
        "Direct messages, channels and threads.",
        0x5865f2,
        false,
    ),
    ("Slack", "Direct messages and channels.", 0x611f69, false),
    ("Email", "Send a message, get a reply.", 0xea4335, false),
    (
        "WhatsApp",
        "Message Letronna from your phone.",
        0x25d366,
        false,
    ),
];

impl Chat {
    fn save_messaging(&mut self, cx: &mut Context<Self>) {
        messaging::save(&self.messaging);
        self.restart_messaging(cx);
        cx.notify();
    }

    /// Check the token with Telegram before saving it, so a typo is caught
    /// here rather than by silence later.
    fn connect_messaging(&mut self, cx: &mut Context<Self>) {
        let token = self.token_input.read(cx).value().trim().to_owned();
        if token.is_empty() {
            self.error = Some("Paste the bot token from @BotFather first.".into());
            cx.notify();
            return;
        }
        let entity = cx.entity();
        cx.spawn(async move |_, cx| {
            let check = cx
                .background_executor()
                .spawn(async move { messaging::check(&token).map(|name| (name, token)) })
                .await;
            let _ = entity.update(cx, |this, cx| {
                match check {
                    Ok((name, token)) => {
                        this.messaging.token = token;
                        this.messaging.enabled = true;
                        this.bot_name = Some(name);
                        this.save_messaging(cx);
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn channel_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows: Vec<AnyElement> = CHANNELS
            .iter()
            .enumerate()
            .map(|(index, (name, _, colour, ready))| {
                let selected = self.channel == index;
                h_flex()
                    .id(("channel", index))
                    .w_full()
                    .h(px(42.))
                    .px_2()
                    .gap_2p5()
                    .items_center()
                    .rounded(cx.theme().radius)
                    .when(selected, |this| this.bg(cx.theme().sidebar_accent))
                    .when(*ready, |this| {
                        this.cursor_pointer()
                            .hover(|s| s.bg(cx.theme().sidebar_accent.opacity(0.6)))
                    })
                    .child(badge(name, *colour))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .when(!*ready, |d| d.text_color(cx.theme().muted_foreground))
                            .child(*name),
                    )
                    .child(if *ready {
                        div()
                            .size(px(6.))
                            .rounded_full()
                            .bg(if self.messaging.ready() {
                                cx.theme().green
                            } else {
                                cx.theme().muted_foreground.opacity(0.5)
                            })
                            .into_any_element()
                    } else {
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground.opacity(0.7))
                            .child("Soon")
                            .into_any_element()
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if CHANNELS[index].3 {
                            this.channel = index;
                            cx.notify();
                        }
                    }))
                    .into_any_element()
            })
            .collect();

        v_flex()
            .w(px(240.))
            .flex_shrink_0()
            .h_full()
            .gap_0p5()
            .p_2()
            .border_r_1()
            .border_color(cx.theme().border)
            .children(rows)
    }

    /// The setup pane for Telegram, the one channel that exists.
    fn telegram_pane(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let config = &self.messaging;
        let has_token = !config.token.trim().is_empty();
        let enabled = config.enabled;
        let allow_tools = config.allow_tools;

        let mut pills = h_flex().gap_1p5().items_center();
        pills = pills.child(pill(
            if enabled && has_token {
                "Enabled"
            } else {
                "Disabled"
            },
            if enabled && has_token {
                cx.theme().green
            } else {
                cx.theme().muted_foreground
            },
            cx,
        ));
        if !has_token {
            pills = pills.child(pill("Needs setup", cx.theme().muted_foreground, cx));
        } else if config.chat.is_none() {
            pills = pills.child(pill("Waiting for a chat", cx.theme().warning, cx));
        } else if config.ready() {
            pills = pills.child(pill("Listening", cx.theme().green, cx));
        }

        let chat_block = match (self.pairing.clone(), config.chat) {
            (Some((chat, from)), _) => v_flex()
                .gap_2()
                .child(help(
                    &format!("{from} messaged your bot from chat {chat}."),
                    cx,
                ))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("allow-chat")
                                .primary()
                                .small()
                                .label("Allow this chat")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.messaging.chat = Some(chat);
                                    this.pairing = None;
                                    this.save_messaging(cx);
                                })),
                        )
                        .child(
                            Button::new("ignore-chat")
                                .ghost()
                                .small()
                                .label("Ignore")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.pairing = None;
                                    cx.notify();
                                })),
                        ),
                )
                .into_any_element(),
            (None, Some(chat)) => h_flex()
                .gap_2()
                .items_center()
                .child(help(
                    &format!("Answering chat {chat}, and nothing else."),
                    cx,
                ))
                .child(
                    Button::new("forget-chat")
                        .ghost()
                        .xsmall()
                        .label("Forget")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.messaging.chat = None;
                            this.save_messaging(cx);
                        })),
                )
                .into_any_element(),
            (None, None) => help(
                if has_token {
                    "Send your bot a message and it will appear here to allow."
                } else {
                    "Add a bot token first."
                },
                cx,
            )
            .into_any_element(),
        };

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                div()
                    .id("telegram-setup")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .w_full()
                            .min_w_0()
                            .px_6()
                            .py_5()
                            .gap_5()
                            .child(
                                v_flex()
                                    .gap_1p5()
                                    .child(
                                        h_flex()
                                            .gap_2p5()
                                            .items_center()
                                            .child(badge("Telegram", CHANNELS[0].2))
                                            .child(
                                                div()
                                                    .text_base()
                                                    .font_family(HEADING_FONT)
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .child("Telegram"),
                                            )
                                            .child(pills),
                                    )
                                    .child(help(CHANNELS[0].1, cx)),
                            )
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(section("Get your credentials", cx))
                                    .child(help(
                                        "Message @BotFather on Telegram, send /newbot, and copy \
                                         the token it gives you. Letronna polls Telegram for \
                                         messages, so nothing here has to be reachable from the \
                                         internet.",
                                        cx,
                                    ))
                                    .child(
                                        h_flex().child(
                                            Button::new("botfather")
                                                .outline()
                                                .xsmall()
                                                .label("Open @BotFather")
                                                .on_click(|_, _, cx| {
                                                    cx.open_url("https://t.me/BotFather")
                                                }),
                                        ),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(section("Required", cx))
                                    .child(field("Bot token", "The token @BotFather gave you. It is kept on this machine.", cx))
                                    .child(Input::new(&self.token_input).small().w_full())
                                    .when_some(self.bot_name.clone(), |this, name| {
                                        this.child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().green)
                                                .child(format!("Connected as @{name}")),
                                        )
                                    }),
                            )
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(section("Allowed chat", cx))
                                    .child(field(
                                        "One chat, approved by you",
                                        "A bot is public the moment it exists, so Letronna \
                                         answers a single chat and ignores everything else.",
                                        cx,
                                    ))
                                    .child(chat_block),
                            )
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(section("Recommended", cx))
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .min_w_0()
                                            .items_center()
                                            .justify_between()
                                            .gap_4()
                                            .child(field(
                                                "Let messages use tools",
                                                "Off, a message can read and search but not write \
                                                 or run anything, because nobody on the other end \
                                                 can answer an approval card.",
                                                cx,
                                            ))
                                            .child(
                                                div().cursor_pointer().child(
                                                    gpui_component::switch::Switch::new(
                                                        "allow-remote-tools",
                                                    )
                                                    .checked(allow_tools)
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.messaging.allow_tools =
                                                                !allow_tools;
                                                            this.save_messaging(cx);
                                                        },
                                                    )),
                                                ),
                                            ),
                                    ),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .px_6()
                    .py_3()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .gap_2p5()
                            .items_center()
                            .child(
                                div().cursor_pointer().child(
                                    gpui_component::switch::Switch::new("messaging-enabled")
                                        .checked(enabled)
                                        .disabled(!has_token)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.messaging.enabled = !enabled;
                                            this.save_messaging(cx);
                                        })),
                                ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if has_token {
                                        "Listen for messages"
                                    } else {
                                        "Add a token to enable"
                                    }),
                            ),
                    )
                    .child(
                        Button::new("save-messaging")
                            .primary()
                            .small()
                            .label("Save changes")
                            .on_click(cx.listener(|this, _, _, cx| this.connect_messaging(cx))),
                    ),
            )
    }

    pub(crate) fn messaging_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .flex_1()
            .min_h_0()
            .child(self.channel_list(cx))
            .child(self.telegram_pane(cx))
    }
}

/// A square colour chip with the channel's initial, so each row is
/// recognisable without shipping brand artwork.
fn badge(name: &str, colour: u32) -> impl IntoElement {
    let initial: String = name.chars().next().unwrap_or('?').to_uppercase().collect();
    div()
        .size(px(26.))
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .rounded(px(6.))
        .bg(rgb(colour))
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(gpui::white())
        .child(initial)
}

fn pill(text: &'static str, colour: Hsla, cx: &App) -> impl IntoElement {
    h_flex()
        .gap_1p5()
        .items_center()
        .px_2()
        .py_0p5()
        .rounded_full()
        .bg(cx.theme().secondary.opacity(0.6))
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(div().size(px(5.)).rounded_full().bg(colour))
        .child(text)
}

fn section(text: &'static str, cx: &App) -> impl IntoElement {
    div()
        .text_xs()
        .font_family(HEADING_FONT)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(cx.theme().muted_foreground)
        .child(text.to_uppercase())
}

fn help(text: &str, cx: &App) -> Div {
    div()
        .min_w_0()
        .max_w(px(620.))
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text.to_owned())
}

fn field(label: &'static str, hint: &'static str, cx: &App) -> impl IntoElement {
    v_flex()
        .min_w_0()
        .gap_0p5()
        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(label))
        .child(help(hint, cx))
}
