//! The Letronna app channel on the Messaging page: the switch that lets the
//! phone app reach this computer, and the one that lets it come straight
//! over Tailscale, pairing a phone with a QR code, and the phones paired.
//!
//! The relay connection, the direct one, the handshakes and each phone's
//! agent are core's (`phone`). The window holds them while the switch is
//! on, shows the code, and asks the user about a phone that used it.

use super::messaging::{field, help, pill, section};
use crate::prelude::*;
use letronna_core::phone::bridge::Agents;
use letronna_core::phone::direct::Direct;
use letronna_core::phone::relay::State;
use letronna_core::phone::switchboard::{Event, Switchboard};
use letronna_core::phone::{self, Caller, Invite, Qr};
use std::collections::HashMap;
use std::time::Instant;

/// The squares of quiet margin round a QR code, as scanners expect.
const QUIET: usize = 4;
/// A QR code square on screen.
const SQUARE: f32 = 4.;

/// What the window holds of the phone app's remote.
#[derive(Default)]
pub(crate) struct Phones {
    board: Option<Switchboard>,
    /// Bumped with each connection opened, so what one already let go of
    /// still says as it winds down is not taken for the new one's.
    generation: u64,
    state: Option<State>,
    /// Whether phones can come straight over Tailscale, once that is known.
    direct: Option<Direct>,
    /// The QR code shown for pairing, and when it was made.
    qr: Option<(Qr, Instant)>,
    /// A phone that used it, waiting on the user's yes.
    asking: Option<Asking>,
    /// The phones connected now, by number, with their keys.
    here: HashMap<u64, String>,
    agents: Agents,
    /// What last went wrong with a phone, for the page.
    trouble: Option<String>,
}

struct Asking {
    phone: u64,
    name: String,
    key: String,
    code: String,
}

impl Chat {
    /// Answer phones while the switch is on, and stop while it is off.
    /// Called at start, by the switches, and on signing in or out, as the
    /// relay's connection is the account's.
    pub(crate) fn restart_phone(&mut self, cx: &mut Context<Self>) {
        let generation = self.phones.generation + 1;
        // Let go first: the lock it holds is the new one's to take.
        self.phones = Phones {
            generation,
            ..Phones::default()
        };
        cx.notify();
        if !self.store.settings.phone {
            return;
        }
        let settings = &self.store.settings;
        let direct = settings.phone_direct.then_some(settings.phone_port);
        let (tx, rx) = async_channel::unbounded();
        match Switchboard::start(phone::this_computer(), direct, tx) {
            Ok(board) => self.phones.board = Some(board),
            Err(why) => {
                self.phones.state = Some(State::Refused(why));
                return;
            }
        }
        cx.spawn(async move |this, cx| {
            while let Ok(event) = rx.recv().await {
                let taken = this.update(cx, |this, cx| {
                    if this.phones.generation == generation {
                        this.on_phone(event, cx);
                    }
                });
                if taken.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn on_phone(&mut self, event: Event, cx: &mut Context<Self>) {
        let phones = &mut self.phones;
        if let Some(board) = &phones.board
            && let Err(why) = phones.agents.take(&event, board)
        {
            phones.trouble = Some(why);
            cx.notify();
        }
        match event {
            // The agent has it; nothing on the page moves.
            Event::Message { .. } => return,
            Event::State(state) => phones.state = Some(state),
            Event::Direct(direct) => phones.direct = Some(direct),
            Event::Arrived {
                phone,
                caller: Caller::New { name, key, code },
            } => {
                phones.qr = None;
                phones.asking = Some(Asking {
                    phone,
                    name,
                    key,
                    code,
                });
            }
            Event::Arrived {
                phone,
                caller: Caller::Paired(known),
            } => {
                phones.here.insert(phone, known.key);
            }
            Event::Gone { phone, why } => {
                phones.here.remove(&phone);
                if phones
                    .asking
                    .as_ref()
                    .is_some_and(|asking| asking.phone == phone)
                {
                    phones.asking = None;
                }
                if why.is_some() {
                    phones.trouble = why;
                }
            }
        }
        cx.notify();
    }

    fn set_phone(&mut self, on: bool, cx: &mut Context<Self>) {
        self.store.settings.phone = on;
        self.save();
        self.restart_phone(cx);
    }

    fn set_phone_direct(&mut self, on: bool, cx: &mut Context<Self>) {
        self.store.settings.phone_direct = on;
        self.save();
        self.restart_phone(cx);
    }

    /// Show a QR code a phone can pair with, for `phone::LIFE`: through the
    /// relay, or over the tailnet when this computer answers there.
    fn pair_phone(&mut self, cx: &mut Context<Self>) {
        let at = match &self.phones.direct {
            Some(Direct::Answering(at)) if self.direct_live() => Some(at.clone()),
            _ => None,
        };
        let Some(board) = &self.phones.board else {
            return;
        };
        let made = Invite::new()
            .map(|invite| match &at {
                Some(at) => invite.reached_at(at),
                None => invite,
            })
            .and_then(|invite| Ok((invite.qr()?, invite)));
        match made {
            Ok((qr, invite)) => {
                board.invite(Some(invite));
                self.phones.qr = Some((qr, Instant::now()));
                self.phones.trouble = None;
            }
            Err(why) => self.phones.trouble = Some(why),
        }
        // Taken down once it runs out, as it is no longer taken.
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(phone::LIFE).await;
            let _ = this.update(cx, |this, cx| {
                if this
                    .phones
                    .qr
                    .as_ref()
                    .is_some_and(|(_, made)| made.elapsed() >= phone::LIFE)
                {
                    this.phones.qr = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn stop_pairing(&mut self, cx: &mut Context<Self>) {
        if let Some(board) = &self.phones.board {
            board.invite(None);
        }
        self.phones.qr = None;
        cx.notify();
    }

    /// Keep the phone waiting on the user's yes, or send it away.
    fn answer_pairing(&mut self, yes: bool, cx: &mut Context<Self>) {
        let (Some(asking), Some(board)) = (self.phones.asking.take(), &self.phones.board) else {
            return;
        };
        match yes {
            true => {
                board.keep(asking.phone);
                self.phones.here.insert(asking.phone, asking.key);
            }
            false => board.refuse(asking.phone),
        }
        cx.notify();
    }

    /// Unpair the phone with key `key`, and cut it off now if it is
    /// connected rather than when it next connects.
    fn remove_phone(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Err(why) = phone::forget(key) {
            self.phones.trouble = Some(why);
        }
        let connected: Vec<u64> = (self.phones.here.iter())
            .filter(|(_, here)| here.as_str() == key)
            .map(|(&number, _)| number)
            .collect();
        for number in connected {
            self.phones.here.remove(&number);
            if let Some(board) = &self.phones.board {
                board.hang_up(number);
            }
        }
        cx.notify();
    }

    /// Whether the switch is on and a phone can reach this computer, through
    /// the relay or over Tailscale.
    pub(crate) fn phone_live(&self) -> bool {
        self.store.settings.phone && (self.relay_live() || self.direct_live())
    }

    fn relay_live(&self) -> bool {
        self.phones.state == Some(State::Connected)
    }

    fn direct_live(&self) -> bool {
        self.store.settings.phone_direct && matches!(self.phones.direct, Some(Direct::Answering(_)))
    }

    pub(crate) fn phone_pane(
        &self,
        channel: &super::messaging::Channel,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let on = self.store.settings.phone;
        let phones = &self.phones;
        let mut pills = h_flex().flex_wrap().gap_1p5().items_center();
        pills = pills.child(match (on, &phones.state) {
            (false, _) => pill("Off", cx.theme().muted_foreground, cx),
            _ if self.phone_live() => pill("Reachable", cx.theme().green, cx),
            (true, Some(State::Refused(_))) => pill("Stopped", cx.theme().red, cx),
            (true, _) => pill("Connecting", cx.theme().warning, cx),
        });
        if on && self.direct_live() {
            pills = pills.child(pill("On your tailnet", cx.theme().green, cx));
        }
        let paired = phone::phones();

        let pairing: AnyElement = if let Some(asking) = &phones.asking {
            v_flex()
                .gap_2()
                .child(field(
                    "Is this your phone?",
                    "Pair it only if the six digits on your phone are these.",
                    cx,
                ))
                .child(
                    div()
                        .text_sm()
                        .child(format!("\u{201c}{}\u{201d}", asking.name)),
                )
                .child(
                    div()
                        .text_xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!("{} {}", &asking.code[..3], &asking.code[3..])),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("pair-yes")
                                .primary()
                                .small()
                                .label("Pair")
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.answer_pairing(true, cx)),
                                ),
                        )
                        .child(
                            Button::new("pair-no")
                                .plain(cx)
                                .small()
                                .label("Not my phone")
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.answer_pairing(false, cx)),
                                ),
                        ),
                )
                .into_any_element()
        } else if let Some((qr, _)) = &phones.qr {
            v_flex()
                .gap_2()
                .child(qr_code(qr))
                .child(help(
                    &format!(
                        "Open the Letronna app on your phone and scan this. It pairs one phone, \
                         within {} minutes, and both screens then show six digits to compare.{}",
                        phone::LIFE.as_secs() / 60,
                        match self.direct_live() {
                            true => " A phone on your tailnet pairs straight over it.",
                            false => "",
                        }
                    ),
                    cx,
                ))
                .child(
                    h_flex().child(
                        Button::new("pair-cancel")
                            .plain(cx)
                            .xsmall()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.stop_pairing(cx))),
                    ),
                )
                .into_any_element()
        } else {
            h_flex()
                .gap_2()
                .items_center()
                .child(help(
                    match (on, &phones.state) {
                        (false, _) => "Turn on the switch below first.",
                        (true, Some(State::Connected)) => {
                            "Pair a phone with a code it scans from this screen."
                        }
                        _ if self.direct_live() => {
                            "Pair a phone with a code it scans from this screen. Until \
                             Letronna's servers answer, it has to be on your tailnet."
                        }
                        (true, Some(State::Refused(why))) => why,
                        (true, _) => "Waiting for Letronna's servers.",
                    },
                    cx,
                ))
                .child(
                    Button::new("pair-phone")
                        .primary()
                        .small()
                        .label("Pair a phone")
                        .disabled(!self.phone_live())
                        .on_click(cx.listener(|this, _, _, cx| this.pair_phone(cx))),
                )
                .into_any_element()
        };

        let list: AnyElement = if paired.is_empty() {
            help("No phones yet.", cx).into_any_element()
        } else {
            let rows = paired.into_iter().enumerate().map(|(index, known)| {
                let connected = phones.here.values().any(|key| *key == known.key);
                let seen = match connected {
                    true => "Connected now".to_owned(),
                    false => format!("Last connected {}", backend::ago(known.seen)),
                };
                let key = known.key.clone();
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_3()
                    .items_center()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().truncate().child(known.name))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(if connected {
                                        cx.theme().green
                                    } else {
                                        cx.theme().muted_foreground
                                    })
                                    .child(seen),
                            ),
                    )
                    .child(
                        Button::new(("phone-remove", index))
                            .plain(cx)
                            .xsmall()
                            .label("Remove")
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.remove_phone(&key, cx)),
                            ),
                    )
                    .into_any_element()
            });
            v_flex().gap_2().children(rows).into_any_element()
        };

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                div()
                    .id("phone-setup")
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
                                            .flex_wrap()
                                            .gap_2p5()
                                            .items_center()
                                            .child(super::messaging::badge(channel))
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
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(section("How it reaches this computer", cx))
                                    .child(help(
                                        "Your phone and this computer each keep a connection to \
                                         Letronna's servers, which pass messages between them. \
                                         They are encrypted end to end, with keys made when you \
                                         pair: the servers cannot read them, and for this nothing \
                                         on this computer listens on the network. A paired phone \
                                         works in Default or Plan mode, and asks you about each \
                                         call that needs a yes.",
                                        cx,
                                    )),
                            )
                            .child(self.tailnet_section(cx))
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(section("Pair a phone", cx))
                                    .child(pairing),
                            )
                            .when_some(phones.trouble.clone(), |this, trouble| {
                                this.child(
                                    div().text_xs().text_color(cx.theme().red).child(trouble),
                                )
                            })
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(section("Paired phones", cx))
                                    .child(list),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2p5()
                    .px_6()
                    .py_3()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        div().flex_shrink_0().cursor_pointer().child(
                            gpui_component::switch::Switch::new("phone-enabled")
                                .checked(on)
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.set_phone(!on, cx)),
                                ),
                        ),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Let your phones reach this computer"),
                    ),
            )
    }
}

impl Chat {
    /// The switch that lets a phone on the same tailnet connect straight to
    /// this computer, and how that is going.
    fn tailnet_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let on = self.store.settings.phone;
        let direct = self.store.settings.phone_direct;
        let (said, trouble) = match (on && direct, &self.phones.direct) {
            (false, _) => (None, false),
            (true, None) => (
                Some("Looking for Tailscale on this computer.".to_owned()),
                false,
            ),
            (true, Some(Direct::Answering(at))) => (
                Some(format!(
                    "Listening at {at}, on your tailnet only. A paired phone learns this the \
                     next time it connects, and tries it first after that."
                )),
                false,
            ),
            (true, Some(Direct::NoTailnet)) => (
                Some(
                    "Tailscale isn't running on this computer, or isn't signed in. Letronna \
                     keeps looking, and answers on your tailnet once it is."
                        .to_owned(),
                ),
                false,
            ),
            (true, Some(Direct::Failed(why))) => (Some(why.clone()), true),
        };
        v_flex()
            .gap_2()
            .child(section("Over Tailscale", cx))
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    // The hint takes what the switch leaves, or it shrinks
                    // to a letter per line.
                    .child(div().flex_1().min_w_0().child(field(
                        "Let phones connect straight here",
                        "When your phone and this computer are on the same Tailscale network, \
                         the phone connects to this computer directly, which still works when \
                         Letronna's servers cannot be reached. Only devices on your tailnet can \
                         reach it, and it is encrypted end to end just the same.",
                        cx,
                    )))
                    .child(
                        div().flex_shrink_0().cursor_pointer().child(
                            gpui_component::switch::Switch::new("phone-direct")
                                .checked(direct)
                                .disabled(!on)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.set_phone_direct(!direct, cx)
                                })),
                        ),
                    ),
            )
            .when_some(said, |this, said| {
                this.child(match trouble {
                    true => div().text_xs().text_color(cx.theme().red).child(said),
                    false => help(&said, cx),
                })
            })
    }
}

/// A QR code, dark on white whatever the theme, as scanners read it best.
fn qr_code(qr: &Qr) -> impl IntoElement {
    let side = qr.size + QUIET * 2;
    let dark: Vec<(usize, usize)> = (0..qr.size)
        .flat_map(|y| (0..qr.size).map(move |x| (x, y)))
        .filter(|&(x, y)| qr.dark(x, y))
        .collect();
    div()
        .size(px(side as f32 * SQUARE))
        .flex_shrink_0()
        .rounded(px(6.))
        .bg(gpui::white())
        .child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    for &(x, y) in &dark {
                        let at = point(
                            bounds.left() + px((x + QUIET) as f32 * SQUARE),
                            bounds.top() + px((y + QUIET) as f32 * SQUARE),
                        );
                        window.paint_quad(gpui::fill(
                            Bounds::new(at, size(px(SQUARE), px(SQUARE))),
                            gpui::black(),
                        ));
                    }
                },
            )
            .size_full(),
        )
}
