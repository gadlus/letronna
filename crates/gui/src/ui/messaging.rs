//! The Messaging page: a channel list beside the setup for the selected one.

use crate::prelude::*;
use letronna_core::messaging;

struct Channel {
    name: &'static str,
    blurb: &'static str,
    icon: &'static str,
    colour: u32,
    built: bool,
}

/// The ones that do not exist yet are listed and marked, so the page says
/// plainly what it does and does not do rather than looking like Telegram was
/// the only idea anyone had.
const CHANNELS: &[Channel] = &[
    Channel {
        name: "Telegram",
        blurb: "Message Letronna from a Telegram chat, and get answers back.",
        icon: "icons/brands/telegram.svg",
        colour: 0x2aabee,
        built: true,
    },
    Channel {
        name: "Discord",
        blurb: "Talk to Letronna through your bot, in a DM or a server, or post results through a webhook.",
        icon: "icons/brands/discord.svg",
        colour: 0x5865f2,
        built: true,
    },
    Channel {
        name: "Slack",
        blurb: "Direct messages and channels.",
        icon: "icons/brands/slack.svg",
        colour: 0x611f69,
        built: false,
    },
    Channel {
        name: "WhatsApp",
        blurb: "Message Letronna from your phone.",
        icon: "icons/brands/whatsapp.svg",
        colour: 0x25d366,
        built: false,
    },
    Channel {
        name: "Email",
        blurb: "Send a message, get a reply.",
        icon: "icons/envelope.svg",
        colour: 0xea4335,
        built: false,
    },
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
        self.connecting = true;
        cx.notify();
        let entity = cx.entity();
        cx.spawn(async move |_, cx| {
            let check = cx
                .background_executor()
                .spawn(async move { messaging::check(&token).map(|name| (name, token)) })
                .await;
            let _ = entity.update(cx, |this, cx| {
                this.connecting = false;
                match check {
                    Ok((name, token)) => {
                        this.messaging.telegram.token = token;
                        this.messaging.telegram.enabled = true;
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
            .map(|(index, channel)| {
                let selected = self.channel == index;
                let ready = &channel.built;
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
                    .child(badge(channel))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .when(!*ready, |d| d.text_color(cx.theme().muted_foreground))
                            .child(channel.name),
                    )
                    .child(if *ready {
                        div()
                            .size(px(6.))
                            .rounded_full()
                            .bg(if self.live(index) {
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
                        if CHANNELS[index].built {
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
        let has_token = !config.telegram.token.trim().is_empty();
        let enabled = config.telegram.enabled;
        let allow_tools = config.telegram.allow_tools;

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
        } else if config.telegram.chat.is_none() {
            pills = pills.child(pill("Waiting for a chat", cx.theme().warning, cx));
        } else if config.telegram.ready() {
            pills = pills.child(pill("Listening", cx.theme().green, cx));
        }

        let chat_block = match (self.pairing.clone(), config.telegram.chat) {
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
                                    this.messaging.telegram.chat = Some(chat);
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
                            this.messaging.telegram.chat = None;
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
                                            .child(badge(&CHANNELS[0]))
                                            .child(
                                                div()
                                                    .text_base()
                                                    .font_family(HEADING_FONT)
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .child("Telegram"),
                                            )
                                            .child(pills),
                                    )
                                    .child(help(CHANNELS[0].blurb, cx)),
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
                                    .child(
                                        Input::new(&self.token_input)
                                            .small()
                                            .w_full()
                                            .mask_toggle(),
                                    )
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
                                                            this.messaging.telegram.allow_tools =
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
                                            this.messaging.telegram.enabled = !enabled;
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
                            .loading(self.connecting)
                            .disabled(self.connecting)
                            .on_click(cx.listener(|this, _, _, cx| this.connect_messaging(cx))),
                    ),
            )
    }

    /// Whether the channel at `index` is set up and switched on.
    fn live(&self, index: usize) -> bool {
        match CHANNELS[index].name {
            "Telegram" => self.messaging.telegram.ready(),
            "Discord" => self.messaging.discord.ready() || self.messaging.discord.bot_ready(),
            _ => false,
        }
    }

    pub(crate) fn messaging_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let pane = match CHANNELS[self.channel].name {
            "Discord" => self.discord_pane(cx).into_any_element(),
            _ => self.telegram_pane(cx).into_any_element(),
        };
        h_flex()
            .flex_1()
            .min_h_0()
            .child(self.channel_list(cx))
            .child(pane)
    }
}

/// The channel's own mark on its own colour. The marks are single-path and
/// monochrome, so they take the tint rather than needing per-brand artwork.
fn badge(channel: &Channel) -> impl IntoElement {
    div()
        .size(px(26.))
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .rounded(px(6.))
        .bg(rgb(channel.colour))
        .child(
            svg()
                .path(channel.icon)
                .size(px(15.))
                .text_color(gpui::white()),
        )
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

impl Chat {
    /// Check the webhook and the bot before keeping them: the bot token with
    /// Discord, and the user ID by opening the DM it will read, which fails
    /// if the two share no server. A typo is caught here rather than by
    /// silence later.
    fn connect_discord(&mut self, cx: &mut Context<Self>) {
        let webhook = self.webhook_input.read(cx).value().trim().to_owned();
        let token = self.discord_token_input.read(cx).value().trim().to_owned();
        let owner = self.discord_owner_input.read(cx).value().trim().to_owned();
        let servers: Vec<String> = self
            .discord_servers_input
            .read(cx)
            .value()
            .split(',')
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty())
            .collect();
        if webhook.is_empty() && token.is_empty() {
            self.error = Some("Add a bot token, a webhook URL, or both.".into());
            cx.notify();
            return;
        }
        if !token.is_empty() && owner.is_empty() {
            self.error = Some("Add your Discord user ID, so the bot knows who to answer.".into());
            cx.notify();
            return;
        }
        self.connecting = true;
        cx.notify();
        let entity = cx.entity();
        cx.spawn(async move |_, cx| {
            let check = cx
                .background_executor()
                .spawn(async move {
                    let hook = match webhook.is_empty() {
                        true => None,
                        false => Some(messaging::discord_check(&webhook)?),
                    };
                    let bot = match token.is_empty() {
                        true => None,
                        false => {
                            let name = messaging::discord_bot_check(&token)?;
                            messaging::discord_open_dm(&token, &owner)?;
                            Some(name)
                        }
                    };
                    Ok::<_, String>((hook, bot, webhook, token, owner))
                })
                .await;
            let _ = entity.update(cx, |this, cx| {
                this.connecting = false;
                match check {
                    Ok((hook, bot, webhook, token, owner)) => {
                        let discord = &mut this.messaging.discord;
                        discord.webhook = webhook;
                        discord.bot_token = token;
                        discord.owner = owner;
                        discord.servers = servers;
                        discord.enabled = true;
                        this.hook_name = hook;
                        this.discord_bot_name = bot;
                        this.save_messaging(cx);
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn discord_pane(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let channel = &CHANNELS[1];
        let config = &self.messaging.discord;
        let has_hook = !config.webhook.trim().is_empty();
        let has_bot = !config.bot_token.trim().is_empty();
        let enabled = config.enabled;
        let live = config.ready() || config.bot_ready();
        let allow_tools = config.allow_tools;

        let pills = h_flex()
            .gap_1p5()
            .items_center()
            .child(pill(
                if live { "Enabled" } else { "Disabled" },
                if live {
                    cx.theme().green
                } else {
                    cx.theme().muted_foreground
                },
                cx,
            ))
            .child(if config.bot_ready() {
                pill("Listening", cx.theme().green, cx)
            } else if has_hook {
                pill("Sends only", cx.theme().muted_foreground, cx)
            } else {
                pill("Needs setup", cx.theme().muted_foreground, cx)
            });

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                div()
                    .id("discord-setup")
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
                                            .child(badge(channel))
                                            .child(
                                                div()
                                                    .text_base()
                                                    .font_family(HEADING_FONT)
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .child(channel.name),
                                            )
                                            .child(pills),
                                    )
                                    .child(help(channel.blurb, cx)),
                            )
                            .child(v_flex().gap_2().child(section("What this does", cx)).child(
                                help(
                                    "With a bot, you talk to Letronna in a direct message, or \
                                         in a server you list by mentioning the bot or replying \
                                         to it, and it answers in the same place. It only ever \
                                         hears you: anyone else, and any server not listed, is \
                                         ignored. A webhook only posts into a channel. Scheduled \
                                         jobs report to both.",
                                    cx,
                                ),
                            ))
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(section("Get your credentials", cx))
                                    .child(help(
                                        "Create an application in the Discord Developer Portal, \
                                         add a bot, and copy its token. Invite the bot to a \
                                         server you are in: Discord only lets a bot DM people it \
                                         shares a server with. For your user ID, turn on \
                                         Developer Mode, then right-click yourself and Copy User \
                                         ID.",
                                        cx,
                                    ))
                                    .child(
                                        h_flex().child(
                                            Button::new("discord-portal")
                                                .outline()
                                                .xsmall()
                                                .label("Open the Developer Portal")
                                                .on_click(|_, _, cx| {
                                                    cx.open_url(
                                                        "https://discord.com/developers/applications",
                                                    )
                                                }),
                                        ),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(section("Bot", cx))
                                    .child(field(
                                        "Bot token",
                                        "It is kept on this machine.",
                                        cx,
                                    ))
                                    .child(
                                        Input::new(&self.discord_token_input)
                                            .small()
                                            .w_full()
                                            .mask_toggle(),
                                    )
                                    .child(field(
                                        "Your user ID",
                                        "The only person the bot answers.",
                                        cx,
                                    ))
                                    .child(Input::new(&self.discord_owner_input).small().w_full())
                                    .child(field(
                                        "Servers",
                                        "Where a mention reaches Letronna. Right-click a server \
                                         and Copy Server ID. Leave empty for DMs only.",
                                        cx,
                                    ))
                                    .child(
                                        Input::new(&self.discord_servers_input).small().w_full(),
                                    )
                                    .when_some(self.discord_bot_name.clone(), |this, name| {
                                        this.child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().green)
                                                .child(format!("Connected as {name}")),
                                        )
                                    })
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
                                                        "discord-allow-tools",
                                                    )
                                                    .checked(allow_tools)
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.messaging.discord.allow_tools =
                                                                !allow_tools;
                                                            this.save_messaging(cx);
                                                        },
                                                    )),
                                                ),
                                            ),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(section("Webhook, optional", cx))
                                    .child(field(
                                        "Webhook URL",
                                        "Anyone holding this URL can post to that channel, so \
                                         it stays on this machine.",
                                        cx,
                                    ))
                                    .child(Input::new(&self.webhook_input).small().w_full())
                                    .when_some(self.hook_name.clone(), |this, name| {
                                        this.child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().green)
                                                .child(format!("Posting as {name}")),
                                        )
                                    }),
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
                                    gpui_component::switch::Switch::new("discord-enabled")
                                        .checked(enabled)
                                        .disabled(!has_hook && !has_bot)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.messaging.discord.enabled = !enabled;
                                            this.save_messaging(cx);
                                        })),
                                ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if has_bot {
                                        "Listen for messages"
                                    } else if has_hook {
                                        "Report scheduled jobs here"
                                    } else {
                                        "Add a bot or a webhook to enable"
                                    }),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .when(live, |this| {
                                this.child(
                                    Button::new("test-discord")
                                        .outline()
                                        .small()
                                        .label("Send a test")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            let config = this.messaging.discord.clone();
                                            cx.background_executor()
                                                .spawn(async move {
                                                    let text = "Letronna is connected.";
                                                    if config.ready() {
                                                        let _ = messaging::discord_send(
                                                            &config.webhook,
                                                            text,
                                                        );
                                                    }
                                                    if config.bot_ready() {
                                                        let _ = messaging::discord_open_dm(
                                                            &config.bot_token,
                                                            &config.owner,
                                                        )
                                                        .and_then(|(channel, _)| {
                                                            messaging::discord_post(
                                                                &config.bot_token,
                                                                &channel,
                                                                text,
                                                            )
                                                        });
                                                    }
                                                })
                                                .detach();
                                        })),
                                )
                            })
                            .child(
                                Button::new("save-discord")
                                    .primary()
                                    .small()
                                    .label("Save changes")
                                    .loading(self.connecting)
                                    .disabled(self.connecting)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.connect_discord(cx)),
                                    ),
                            ),
                    ),
            )
    }
}
