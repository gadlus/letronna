//! The Messaging page: reaching Letronna from Telegram.

use crate::core::messaging;
use crate::prelude::*;

impl Chat {
    fn save_messaging(&mut self, cx: &mut Context<Self>) {
        messaging::save(&self.messaging);
        self.restart_messaging(cx);
        cx.notify();
    }

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

    pub(crate) fn messaging_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let config = &self.messaging;
        let connected = config.ready();

        let pairing = self.pairing.clone().map(|(chat, from)| {
            row(cx)
                .child(
                    v_flex()
                        .min_w_0()
                        .gap_0p5()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(format!("{from} messaged your bot")),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(
                                    "Chat {chat}. Allow it and Letronna will answer it, and \
                                     only it."
                                )),
                        ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("allow-chat")
                                .primary()
                                .small()
                                .label("Allow")
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
        });

        let paired = config.chat.map(|chat| {
            row(cx)
                .child(
                    v_flex()
                        .min_w_0()
                        .gap_0p5()
                        .child(div().text_sm().child("Answering one chat"))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("Chat {chat}. Nothing else is answered.")),
                        ),
                )
                .child(
                    Button::new("forget-chat")
                        .ghost()
                        .small()
                        .label("Forget")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.messaging.chat = None;
                            this.save_messaging(cx);
                        })),
                )
        });

        let allow_tools = config.allow_tools;
        let tools = row(cx)
            .child(
                v_flex()
                    .min_w_0()
                    .gap_0p5()
                    .child(div().text_sm().child("Let messages use tools"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "Off, a message can read and search but not write or run \
                                 anything, because no one on the other end can answer an \
                                 approval card. Turn it on only for a chat you control.",
                            ),
                    ),
            )
            .child(
                div().cursor_pointer().child(
                    gpui_component::switch::Switch::new("allow-remote-tools")
                        .checked(allow_tools)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.messaging.allow_tools = !allow_tools;
                            this.save_messaging(cx);
                        })),
                ),
            );

        v_flex()
            .flex_1()
            .min_h_0()
            .px_8()
            .py_6()
            .gap_4()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xl()
                            .font_family(HEADING_FONT)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Messaging"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "Message Letronna from Telegram. Create a bot with @BotFather, \
                                 paste its token, then message the bot and allow the chat. \
                                 Nothing is exposed to the internet: Letronna polls Telegram.",
                            ),
                    ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().flex_1().child(Input::new(&self.token_input).small()))
                            .child(
                                Button::new("connect-bot")
                                    .primary()
                                    .small()
                                    .label(if connected { "Reconnect" } else { "Connect" })
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.connect_messaging(cx)),
                                    ),
                            ),
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
            .children(pairing)
            .children(paired)
            .when(connected, |this| this.child(tools))
            .when(
                connected && config.chat.is_none() && self.pairing.is_none(),
                |this| {
                    this.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "Now send your bot a message, and it will appear here to allow.",
                            ),
                    )
                },
            )
    }
}

fn row(cx: &App) -> Div {
    h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap_3()
        .p_3()
        .rounded(cx.theme().radius)
        .bg(cx.theme().secondary.opacity(0.3))
}
