//! The Wallet page and Settings › Wallet: the wallets the agent pays from,
//! what they hold, what was paid, and the rules it pays by.
//!
//! Everything here drives `letronna_core::wallet`, which keeps its own file
//! rather than settings.json, as `letronna wallet` in the terminal changes it
//! too: each change is made on what is on disk, and the page reads the file
//! again whenever it or the Settings section opens. Balances come from the
//! networks, so they are read off the UI thread when the page opens or
//! Refresh is pressed, never per frame. A new wallet's recovery phrase is
//! shown once, on the page, until the user says it is written down; a
//! revealed one only until they hide it.

use super::PageColumn;
use crate::prelude::*;
use gpui_component::notification::Notification;
use gpui_component::progress::Progress;
use gpui_component::radio::{Radio, RadioGroup};
use letronna_core::wallet::{self, Entry, Holding, Holds, Wallet, X402};

actions!(wallet, [OpenWallet]);

/// How a wallet is being added.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Adding {
    New,
    Import,
    Watch,
}

/// What the Wallet page and its Settings section hold between frames.
pub(crate) struct WalletPage {
    /// The wallet file as last read.
    kept: Wallet,
    /// What each wallet holds, by its id; none while the networks are asked.
    held: Option<Vec<(String, Vec<Holding>)>>,
    /// Every payment, oldest first.
    ledger: Vec<Entry>,
    /// The add form, when it is open.
    adding: Option<Adding>,
    /// A wallet is being made or brought in.
    working: bool,
    /// A wallet made just now and its phrase, until the user has written it
    /// down.
    fresh: Option<(String, String)>,
    /// A wallet's phrase or key the user asked to see, until they hide it.
    shown: Option<(String, String)>,
    name: Entity<InputState>,
    secret: Entity<InputState>,
    address: Entity<InputState>,
    auto_up_to: Entity<InputState>,
    per_payment: Entity<InputState>,
    per_day: Entity<InputState>,
    per_month: Entity<InputState>,
    site: Entity<InputState>,
    /// The network whose RPC address the RPC field sets.
    rpc_chain: String,
    rpc: Entity<InputState>,
    net_name: Entity<InputState>,
    net_id: Entity<InputState>,
    net_rpc: Entity<InputState>,
    net_symbol: Entity<InputState>,
}

impl WalletPage {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Chat>) -> Self {
        let field = |hint: &'static str, window: &mut Window, cx: &mut Context<Chat>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(hint))
        };
        Self {
            kept: Wallet::default(),
            held: None,
            ledger: Vec::new(),
            adding: None,
            working: false,
            fresh: None,
            shown: None,
            name: field("Name (optional)", window, cx),
            secret: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Recovery phrase, or a private key from another wallet app")
                    .masked(true)
            }),
            address: field("0x… or a Solana address", window, cx),
            auto_up_to: field("$", window, cx),
            per_payment: field("$", window, cx),
            per_day: field("$", window, cx),
            per_month: field("$", window, cx),
            site: field("api.example.com", window, cx),
            rpc_chain: wallet::BASE.into(),
            rpc: field("https://… (empty for the public one)", window, cx),
            net_name: field("Name", window, cx),
            net_id: field("Chain id", window, cx),
            net_rpc: field("https://… RPC address", window, cx),
            net_symbol: field("Coin (ETH)", window, cx),
        }
    }
}

impl Chat {
    /// Go to the Wallet page from anywhere.
    pub(crate) fn open_wallet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_page(Page::Wallet, window, cx);
    }

    /// Read the wallet file and the payments again; with `balances`, ask the
    /// networks what each wallet holds, off the UI thread.
    pub(crate) fn read_wallet(&mut self, balances: bool, cx: &mut Context<Self>) {
        self.wallet.kept = wallet::load();
        self.wallet.ledger = wallet::ledger();
        if !balances {
            return cx.notify();
        }
        self.wallet.held = None;
        let kept = self.wallet.kept.clone();
        cx.spawn(async move |this, cx| {
            let held = cx
                .background_executor()
                .spawn(async move {
                    kept.accounts
                        .iter()
                        .map(|account| (account.id.clone(), wallet::balances(&kept, account)))
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.wallet.held = Some(held);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Change the wallet file with `edit`, then show it as saved, or say why
    /// it could not be.
    fn change_wallet(
        &mut self,
        edit: impl FnOnce(&mut Wallet) -> Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        match wallet::change(edit) {
            Ok(kept) => self.wallet.kept = kept,
            Err(why) => self.error = Some(why),
        }
        cx.notify();
    }

    /// Make, bring in or watch a wallet from the add form, off the UI thread.
    fn add_wallet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(adding) = self.wallet.adding else {
            return;
        };
        let name = self.wallet.name.read(cx).value().trim().to_string();
        let secret = self.wallet.secret.read(cx).value().trim().to_string();
        let address = self.wallet.address.read(cx).value().trim().to_string();
        if adding == Adding::Import && secret.is_empty() {
            self.error = Some("Paste the recovery phrase or the key first.".into());
            return cx.notify();
        }
        if adding == Adding::Watch && address.is_empty() {
            self.error = Some("Paste the address to watch first.".into());
            return cx.notify();
        }
        for field in [&self.wallet.name, &self.wallet.secret, &self.wallet.address] {
            field.update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.wallet.working = true;
        cx.spawn(async move |this, cx| {
            let made = cx
                .background_executor()
                .spawn(async move {
                    match adding {
                        Adding::New => {
                            wallet::create(&name).map(|(made, phrase)| (made, Some(phrase)))
                        }
                        Adding::Import => {
                            wallet::import(&name, &secret, 0).map(|made| (made, None))
                        }
                        Adding::Watch => wallet::watch(&name, &address).map(|made| (made, None)),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.wallet.working = false;
                match made {
                    Ok((made, phrase)) => {
                        this.wallet.adding = None;
                        this.wallet.fresh = phrase.map(|phrase| (made.label.clone(), phrase));
                        this.read_wallet(true, cx);
                    }
                    Err(why) => this.error = Some(why),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Show wallet `label`'s phrase or key once the user has said so on a
    /// card that says what it is.
    fn ask_to_reveal(&mut self, label: String, window: &mut Window, cx: &mut Context<Self>) {
        let chat = cx.entity();
        window.open_alert_dialog(cx, move |dialog, _, cx| {
            let chat = chat.clone();
            let label = label.clone();
            dialog
                .title(div().font_family(HEADING_FONT).child("Show the recovery phrase?"))
                .width(px(420.))
                .confirm()
                .button_props(
                    gpui_component::dialog::DialogButtonProps::default().ok_text("Show it"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "Anyone who sees {label}'s phrase or key can take its money. Make sure nobody is looking at the screen."
                        )),
                )
                .on_ok(move |_, _, cx| {
                    let label = label.clone();
                    chat.update(cx, |this, cx| {
                        match wallet::reveal(&label) {
                            Ok(secret) => this.wallet.shown = Some((label, secret)),
                            Err(why) => this.error = Some(why),
                        }
                        cx.notify();
                    });
                    true
                })
        });
    }

    /// Take the limits typed in Settings › Wallet; an empty field keeps its
    /// cap as it is.
    pub(crate) fn save_wallet_limits(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let fields = [
            self.wallet.auto_up_to.clone(),
            self.wallet.per_payment.clone(),
            self.wallet.per_day.clone(),
            self.wallet.per_month.clone(),
        ];
        let mut typed = Vec::new();
        for field in &fields {
            let text = field.read(cx).value().trim().to_string();
            match text.is_empty() {
                true => typed.push(None),
                false => match wallet::parse_dollars(&text) {
                    Ok(micros) => typed.push(Some(micros)),
                    Err(why) => {
                        self.error = Some(why);
                        return cx.notify();
                    }
                },
            }
        }
        let [auto, payment, day, month] = [typed[0], typed[1], typed[2], typed[3]];
        self.change_wallet(
            |w| {
                let policy = &mut w.policy;
                policy.auto_up_to = auto.unwrap_or(policy.auto_up_to);
                policy.per_payment = payment.unwrap_or(policy.per_payment);
                policy.per_day = day.unwrap_or(policy.per_day);
                policy.per_month = month.unwrap_or(policy.per_month);
                if policy.auto_up_to > policy.per_payment {
                    return Err(
                        "What is paid without asking cannot be more than the cap on one payment."
                            .into(),
                    );
                }
                Ok(())
            },
            cx,
        );
        for field in fields {
            field.update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    /// Pay only the site typed, or never pay it.
    fn list_site(&mut self, allow: bool, window: &mut Window, cx: &mut Context<Self>) {
        let site = self.wallet.site.read(cx).value().trim().to_lowercase();
        let site = site
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_string();
        if site.is_empty() {
            return;
        }
        self.wallet
            .site
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.change_wallet(
            move |w| {
                let (into, out) = match allow {
                    true => (&mut w.policy.sites_allow, &mut w.policy.sites_block),
                    false => (&mut w.policy.sites_block, &mut w.policy.sites_allow),
                };
                out.retain(|s| *s != site);
                if !into.contains(&site) {
                    into.push(site);
                }
                Ok(())
            },
            cx,
        );
    }

    /// Give the chosen network the RPC address typed, or its public one back.
    fn save_rpc(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rpc = self.wallet.rpc.read(cx).value().trim().to_string();
        match wallet::set_network(&self.wallet.rpc_chain, None, Some(&rpc)) {
            Ok(chain) => {
                self.note = Some(format!("{} is read through {}.", chain.name, chain.rpc));
                self.wallet
                    .rpc
                    .update(cx, |input, cx| input.set_value("", window, cx));
            }
            Err(why) => self.error = Some(why),
        }
        self.read_wallet(false, cx);
    }

    /// Add the EVM network typed, once its RPC says it is that chain.
    fn add_network(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = |field: &Entity<InputState>, cx: &Context<Self>| {
            field.read(cx).value().trim().to_string()
        };
        let name = value(&self.wallet.net_name, cx);
        let id = value(&self.wallet.net_id, cx);
        let rpc = value(&self.wallet.net_rpc, cx);
        let symbol = value(&self.wallet.net_symbol, cx);
        let Ok(id) = id.parse::<u64>() else {
            self.error = Some(format!("\"{id}\" is not a chain id."));
            return cx.notify();
        };
        for field in [
            &self.wallet.net_name,
            &self.wallet.net_id,
            &self.wallet.net_rpc,
            &self.wallet.net_symbol,
        ] {
            field.update(cx, |input, cx| input.set_value("", window, cx));
        }
        cx.spawn(async move |this, cx| {
            let added = cx
                .background_executor()
                .spawn(async move { wallet::add_network(&name, id, &rpc, &symbol) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match added {
                    Ok(chain) => this.note = Some(format!("Added {}.", chain.name)),
                    Err(why) => this.error = Some(why),
                }
                this.read_wallet(false, cx);
            });
        })
        .detach();
    }

    pub(crate) fn wallet_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let page = &self.wallet;
        let refresh = Button::new("wallet-refresh")
            .plain(cx)
            .small()
            .icon(Icon::new(AppIcon::Reload))
            .tooltip("Read the balances again")
            .on_click(cx.listener(|this, _, _, cx| this.read_wallet(true, cx)))
            .into_any_element();
        let add = Button::new("wallet-add")
            .framed(cx)
            .small()
            .label("Add wallet")
            .on_click(cx.listener(|this, _, _, cx| {
                this.wallet.adding = match this.wallet.adding {
                    Some(_) => None,
                    None => Some(Adding::New),
                };
                cx.notify();
            }))
            .into_any_element();
        let blurb = "The wallets Letronna pays from: sites that ask to be paid (x402), and money you ask it to send. Keys stay in your system's vault; the agent never sees them.";
        let mut body = v_flex().page_column().pb_6().gap_5();
        if let Some((label, phrase)) = &page.fresh {
            body = body.child(self.phrase_box(
                "fresh",
                format!("{label}'s recovery phrase"),
                "Write it down and keep it offline. It is the only way back to this money if this computer is lost; it is not shown again unless you ask for it.",
                phrase.clone(),
                "I've written it down",
                cx,
            ));
        }
        if let Some((label, secret)) = &page.shown {
            body = body.child(self.phrase_box(
                "shown",
                format!("{label}'s recovery phrase or key"),
                "Anyone who sees this can take the money.",
                secret.clone(),
                "Hide it",
                cx,
            ));
        }
        if page.adding.is_some() {
            body = body.child(self.add_form(cx));
        }
        if page.kept.accounts.is_empty() && page.adding.is_none() && page.fresh.is_none() {
            body = body.child(
                v_flex()
                    .gap_2()
                    .px_3()
                    .py_2p5()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().secondary.opacity(0.3))
                    .child(div().text_sm().child("No wallet yet"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Make a new one, bring in one from MetaMask, Phantom or any wallet app by its recovery phrase or a key, or watch an address. One phrase holds an address for Base, Ethereum and every EVM chain, and one for Solana."),
                    ),
            );
        } else {
            body = body.child(self.spending(cx));
            for account in &page.kept.accounts {
                body = body.child(self.account_row(account, cx));
            }
            body = body.child(self.activity(cx));
        }
        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                v_flex()
                    .page_column()
                    .pt_6()
                    .pb_4()
                    .child(super::page_header("Wallet", blurb, [add, refresh], cx)),
            )
            .child(
                div()
                    .id("wallet-page")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body),
            )
    }

    /// A phrase in a box the user dismisses.
    fn phrase_box(
        &self,
        id: &'static str,
        title: String,
        why: &'static str,
        secret: String,
        done: &'static str,
        cx: &mut Context<Self>,
    ) -> Div {
        let copy = secret.clone();
        v_flex()
            .gap_2()
            .px_3()
            .py_2p5()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().warning)
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(why),
            )
            .child(div().font_family(MONO_FONT).text_sm().child(secret))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new((id, 0usize))
                            .plain(cx)
                            .xsmall()
                            .label("Copy")
                            .on_click(move |_, window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                                window.push_notification(Notification::success("Copied."), cx);
                            }),
                    )
                    .child(
                        Button::new((id, 1usize))
                            .primary()
                            .xsmall()
                            .label(done)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                match id {
                                    "fresh" => this.wallet.fresh = None,
                                    _ => this.wallet.shown = None,
                                }
                                cx.notify();
                            })),
                    ),
            )
    }

    /// The form that adds a wallet.
    fn add_form(&self, cx: &mut Context<Self>) -> Div {
        let page = &self.wallet;
        let adding = page.adding.unwrap_or(Adding::New);
        let kinds = [
            (
                Adding::New,
                "Make a new wallet",
                "A new recovery phrase, shown once for you to write down.",
            ),
            (
                Adding::Import,
                "Bring one in",
                "By its recovery phrase, or a private key exported from MetaMask, Phantom or another app.",
            ),
            (
                Adding::Watch,
                "Watch an address",
                "Its balances only; it cannot pay.",
            ),
        ];
        let choice = RadioGroup::vertical("wallet-adding")
            .selected_index(kinds.iter().position(|(kind, _, _)| *kind == adding))
            .children(kinds.iter().map(|(_, label, about)| {
                Radio::new(*label).small().label(*label).child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(*about),
                )
            }))
            .on_click(cx.listener(move |this, at: &usize, _, cx| {
                this.wallet.adding = Some(kinds[*at].0);
                cx.notify();
            }));
        let go = match adding {
            Adding::New => "Make it",
            Adding::Import => "Bring it in",
            Adding::Watch => "Watch it",
        };
        v_flex()
            .gap_3()
            .px_3()
            .py_2p5()
            .rounded(cx.theme().radius)
            .bg(cx.theme().secondary.opacity(0.3))
            .child(choice)
            .child(Input::new(&page.name).small())
            .when(adding == Adding::Import, |this| {
                this.child(Input::new(&page.secret).small().mask_toggle())
            })
            .when(adding == Adding::Watch, |this| {
                this.child(Input::new(&page.address).small())
            })
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("wallet-add-go")
                            .primary()
                            .small()
                            .label(if page.working { "Working…" } else { go })
                            .disabled(page.working)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.add_wallet(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("wallet-add-cancel")
                            .plain(cx)
                            .small()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.wallet.adding = None;
                                cx.notify();
                            })),
                    ),
            )
    }

    /// What the agent has spent against the caps, and whether it may pay.
    fn spending(&self, cx: &mut Context<Self>) -> Div {
        let policy = &self.wallet.kept.policy;
        let spent = wallet::spent(&self.wallet.ledger, chrono::Utc::now().timestamp());
        let meter = |id: &'static str, what: &'static str, spent: u64, cap: u64, cx: &App| {
            let share = if cap == 0 {
                100.
            } else {
                (spent as f32 / cap as f32 * 100.).min(100.)
            };
            h_flex()
                .gap_3()
                .items_center()
                .child(div().w(px(110.)).flex_shrink_0().text_sm().child(what))
                .child(
                    div()
                        .w(px(160.))
                        .flex_shrink_0()
                        .child(Progress::new(id).value(share)),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "{} of {}",
                            wallet::dollars(spent),
                            wallet::dollars(cap)
                        )),
                )
        };
        let x402 = match policy.x402 {
            X402::Off => "Sites that ask to be paid are never paid.".to_owned(),
            X402::Ask => "You see each price a site asks for.".to_owned(),
            X402::Auto => format!(
                "Prices up to {} are paid without asking; above that you see them.",
                wallet::dollars(policy.auto_up_to)
            ),
        };
        let paying = !policy.paused;
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(super::settings::label("SPENT BY THE AGENT", cx))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(if paying { "May pay" } else { "Paused" })
                            .child(
                                super::settings::KeyedSwitch::new("wallet-paying")
                                    .checked(paying)
                                    .on_click({
                                        let chat = cx.entity();
                                        move |on, _, cx| {
                                            let on = *on;
                                            chat.update(cx, |this, cx| {
                                                this.change_wallet(
                                                    |w| {
                                                        w.policy.paused = !on;
                                                        Ok(())
                                                    },
                                                    cx,
                                                )
                                            })
                                        }
                                    }),
                            ),
                    ),
            )
            .child(meter(
                "wallet-day",
                "Last 24 hours",
                spent.day,
                policy.per_day,
                cx,
            ))
            .child(meter(
                "wallet-month",
                "Last 30 days",
                spent.month,
                policy.per_month,
                cx,
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{x402} One payment is at most {}. Change this in Settings › Wallet.",
                        wallet::dollars(policy.per_payment)
                    )),
            )
    }

    /// One wallet: its addresses, what it holds, and what can be done to it.
    fn account_row(&self, account: &wallet::Account, cx: &mut Context<Self>) -> Div {
        let page = &self.wallet;
        let default = account.id == page.kept.default;
        let kind = match account.holds {
            Holds::Phrase => "recovery phrase",
            Holds::Key => "key",
            Holds::Watch => "watched only",
        };
        let muted = cx.theme().muted_foreground;
        let address_row = |which: &'static str, address: &str, cx: &mut Context<Self>| {
            let copy = address.to_owned();
            h_flex()
                .gap_2()
                .items_center()
                .min_w_0()
                .child(
                    div()
                        .w(px(56.))
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(muted)
                        .child(which),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_family(MONO_FONT)
                        .text_xs()
                        .child(address.to_owned()),
                )
                .child(
                    Button::new(SharedString::from(format!("copy-{which}-{address}")))
                        .plain(cx)
                        .xsmall()
                        .icon(Icon::new(IconName::Copy))
                        .tooltip("Copy the address")
                        .on_click(move |_, window, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                            window.push_notification(Notification::success("Copied."), cx);
                        }),
                )
        };
        let mut addresses = v_flex().gap_1();
        if !account.evm.is_empty() {
            addresses = addresses.child(address_row("EVM", &account.evm, cx));
        }
        if !account.solana.is_empty() {
            addresses = addresses.child(address_row("Solana", &account.solana, cx));
        }
        let held: Vec<AnyElement> = match &page.held {
            None => vec![
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("Reading balances…")
                    .into_any_element(),
            ],
            Some(all) => {
                let mine = all
                    .iter()
                    .find(|(id, _)| *id == account.id)
                    .map(|(_, held)| held.as_slice())
                    .unwrap_or_default();
                // Only what is there, and what could not be read: a row of
                // noughts for every network says nothing.
                let some: Vec<&Holding> = mine
                    .iter()
                    .filter(|holding| !holding.error.is_empty() || holding.amount != "0")
                    .collect();
                if some.is_empty() {
                    let networks = mine
                        .iter()
                        .map(|h| &h.chain)
                        .collect::<std::collections::HashSet<_>>()
                        .len();
                    vec![
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!(
                                "Nothing held on the {networks} networks that are on."
                            ))
                            .into_any_element(),
                    ]
                } else {
                    some.into_iter()
                        .map(|holding| holding_row(holding, muted))
                        .collect()
                }
            }
        };
        let label = account.label.clone();
        let reveal = account.label.clone();
        let remove = account.label.clone();
        v_flex()
            .gap_2()
            .px_3()
            .py_2p5()
            .rounded(cx.theme().radius)
            .bg(cx.theme().secondary.opacity(0.3))
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .min_w_0()
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).truncate().child(account.label.clone()))
                            .child(div().text_xs().text_color(muted).child(kind))
                            .when(default, |this| {
                                this.child(div().text_xs().text_color(cx.theme().primary).child("pays by default"))
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .flex_shrink_0()
                            .when(!default && account.holds != Holds::Watch, |this| {
                                this.child(
                                    Button::new(SharedString::from(format!("default-{}", account.id)))
                                        .plain(cx)
                                        .xsmall()
                                        .label("Make default")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if let Err(why) = wallet::make_default(&label) {
                                                this.error = Some(why);
                                            }
                                            this.read_wallet(false, cx);
                                        })),
                                )
                            })
                            .when(account.holds != Holds::Watch, |this| {
                                this.child(
                                    Button::new(SharedString::from(format!("reveal-{}", account.id)))
                                        .plain(cx)
                                        .xsmall()
                                        .label("Show phrase")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.ask_to_reveal(reveal.clone(), window, cx)
                                        })),
                                )
                            })
                            .child(
                                Button::new(SharedString::from(format!("remove-{}", account.id)))
                                    .plain(cx)
                                    .xsmall()
                                    .icon(Icon::new(AppIcon::Trash))
                                    .tooltip("Remove from Letronna")
                                    .on_click({
                                        let chat = cx.entity();
                                        move |_, window, cx| {
                                            let label = remove.clone();
                                            let chat = chat.clone();
                                            super::confirm_delete(
                                                "Remove this wallet?",
                                                format!("{label} and its key leave Letronna. Its money stays on the chain, reachable only with its recovery phrase or key written down."),
                                                window,
                                                cx,
                                                move |_, cx| {
                                                    let label = label.clone();
                                                    chat.update(cx, |this, cx| {
                                                        if let Err(why) = wallet::remove(&label) {
                                                            this.error = Some(why);
                                                        }
                                                        this.read_wallet(true, cx);
                                                    })
                                                },
                                            )
                                        }
                                    }),
                            ),
                    ),
            )
            .child(addresses)
            .child(v_flex().gap_0p5().children(held))
    }

    /// The payments, newest first.
    fn activity(&self, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let rows: Vec<AnyElement> = self
            .wallet
            .ledger
            .iter()
            .rev()
            .take(50)
            .enumerate()
            .map(|(at, entry)| {
                let link = entry.link.clone();
                h_flex()
                    .gap_2()
                    .py_1()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .child(wallet::entry_line(entry)),
                    )
                    .when(!link.is_empty(), |this| {
                        this.child(
                            Button::new(("wallet-tx", at))
                                .plain(cx)
                                .xsmall()
                                .label("Transaction")
                                .on_click(move |_, _, cx| cx.open_url(&link)),
                        )
                    })
                    .into_any_element()
            })
            .collect();
        v_flex()
            .gap_1()
            .child(super::settings::label("PAYMENTS", cx))
            .when(rows.is_empty(), |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("Nothing has been paid yet."),
                )
            })
            .children(rows)
    }
}

/// One coin a wallet holds on one network, or why it could not be read.
fn holding_row(holding: &Holding, muted: Hsla) -> AnyElement {
    let worth = holding
        .usd
        .filter(|usd| *usd > 0)
        .map(|usd| format!("  {}", wallet::dollars(usd)))
        .unwrap_or_default();
    h_flex()
        .gap_2()
        .text_xs()
        .child(
            div()
                .w(px(110.))
                .flex_shrink_0()
                .text_color(muted)
                .child(holding.chain.clone()),
        )
        .child(match holding.error.is_empty() {
            true => div().child(format!("{} {}{worth}", holding.amount, holding.symbol)),
            false => div()
                .text_color(muted)
                .truncate()
                .child(holding.error.clone()),
        })
        .into_any_element()
}

/// Settings › Wallet: whether and how much the agent may pay, the networks,
/// and the sites.
pub(crate) fn wallet_settings(chat: &Entity<Chat>, page: &WalletPage, cx: &App) -> Div {
    let policy = &page.kept.policy;
    let change = |edit: fn(&mut Wallet, bool), on: bool| {
        std::rc::Rc::new(move |this: &mut Chat| {
            match wallet::change(|w| {
                edit(w, on);
                Ok(())
            }) {
                Ok(kept) => this.wallet.kept = kept,
                Err(why) => this.error = Some(why),
            }
        }) as std::rc::Rc<dyn Fn(&mut Chat)>
    };
    let paying = super::settings::toggle_row(
        "wallet-paying-setting",
        "Let the agent pay",
        "Off, nothing is paid or sent by the agent, asked or not.",
        !policy.paused,
        chat,
        cx,
        change(|w, on| w.policy.paused = on, !policy.paused),
    );
    let modes = [
        (
            X402::Off,
            "Never pay a site",
            "A site that asks to be paid is left unpaid.",
        ),
        (
            X402::Ask,
            "Ask each time",
            "You see each price on a card and decide.",
        ),
        (
            X402::Auto,
            "Pay small prices on my behalf",
            "Up to the amount below without asking, within the caps; above it you are asked.",
        ),
    ];
    let set = chat.clone();
    let x402 = RadioGroup::vertical("wallet-x402")
        .selected_index(modes.iter().position(|(mode, _, _)| *mode == policy.x402))
        .children(modes.iter().map(|(_, label, about)| {
            Radio::new(*label).small().label(*label).child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(*about),
            )
        }))
        .on_click(move |at, _, cx| {
            let mode = modes[*at].0;
            set.update(cx, |this, cx| {
                this.change_wallet(
                    |w| {
                        w.policy.x402 = mode;
                        Ok(())
                    },
                    cx,
                )
            })
        });
    let limit = |title: &'static str, now: u64, field: &Entity<InputState>| {
        h_flex()
            .gap_3()
            .items_center()
            .child(div().w(px(220.)).flex_shrink_0().text_sm().child(title))
            .child(div().w(px(120.)).child(Input::new(field).small()))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("now {}", wallet::dollars(now))),
            )
    };
    let save = chat.clone();
    let limits = v_flex()
        .gap_2()
        .px_3()
        .py_2p5()
        .rounded(cx.theme().radius)
        .bg(cx.theme().secondary.opacity(0.3))
        .child(limit("Paid without asking, up to", policy.auto_up_to, &page.auto_up_to))
        .child(limit("Most for one payment", policy.per_payment, &page.per_payment))
        .child(limit("Most in 24 hours", policy.per_day, &page.per_day))
        .child(limit("Most in 30 days", policy.per_month, &page.per_month))
        .child(
            h_flex()
                .gap_3()
                .items_center()
                .child(
                    Button::new("wallet-limits-save")
                        .primary()
                        .small()
                        .label("Save")
                        .on_click(move |_, window, cx| {
                            save.update(cx, |this, cx| this.save_wallet_limits(window, cx))
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("The caps hold even after you say yes to a payment. What you send yourself is not counted."),
                ),
        );
    let saved_only = super::settings::toggle_row(
        "wallet-saved-only",
        "Only send to saved addresses",
        "The agent can send only to addresses saved with `letronna wallet save`.",
        policy.saved_only,
        chat,
        cx,
        change(|w, on| w.policy.saved_only = on, !policy.saved_only),
    );
    let testnets = super::settings::toggle_row(
        "wallet-testnets",
        "Show test networks",
        "Base Sepolia, Sepolia, Solana Devnet and the rest, for trying payments with test money.",
        policy.testnets,
        chat,
        cx,
        change(|w, on| w.policy.testnets = on, !policy.testnets),
    );
    let networks = wallet::networks(&page.kept);
    let mut network_rows = v_flex().gap_1();
    for chain in networks
        .iter()
        .filter(|chain| !chain.testnet || policy.testnets)
    {
        let chat = chat.clone();
        let name = chain.id.clone();
        let own = page
            .kept
            .networks
            .get(&chain.id)
            .is_some_and(|n| !n.rpc.is_empty());
        network_rows = network_rows.child(
            h_flex()
                .justify_between()
                .items_center()
                .gap_3()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .min_w_0()
                        .child(div().text_sm().child(chain.name.clone()))
                        .child(
                            div()
                                .text_xs()
                                .truncate()
                                .text_color(cx.theme().muted_foreground)
                                .child(if own {
                                    format!("{} · your RPC", chain.id)
                                } else {
                                    chain.id.clone()
                                }),
                        ),
                )
                .child(
                    super::settings::KeyedSwitch::new(SharedString::from(format!(
                        "net-{}",
                        chain.id
                    )))
                    .checked(chain.on)
                    .on_click(move |on, _, cx| {
                        let on = *on;
                        let name = name.clone();
                        chat.update(cx, |this, cx| {
                            if let Err(why) = wallet::set_network(&name, Some(on), None) {
                                this.error = Some(why);
                            }
                            this.read_wallet(false, cx);
                        })
                    }),
                ),
        );
    }
    let picked = networks
        .iter()
        .find(|chain| chain.id == page.rpc_chain)
        .map(|chain| chain.name.clone())
        .unwrap_or_else(|| "Base".into());
    let menu_chat = chat.clone();
    let menu_networks: Vec<(String, String)> = networks
        .iter()
        .map(|chain| (chain.id.clone(), chain.name.clone()))
        .collect();
    let chosen = page.rpc_chain.clone();
    let save_rpc = chat.clone();
    let rpc = v_flex()
        .gap_2()
        .child(super::settings::label("YOUR OWN RPC", cx))
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    Button::new("wallet-rpc-chain")
                        .plain(cx)
                        .small()
                        .child(
                            h_flex()
                                .gap_1()
                                .child(picked)
                                .child(Icon::new(IconName::ChevronDown).size_3()),
                        )
                        .dropdown_menu(move |menu, _, _| {
                            menu_networks.iter().fold(menu, |menu, (id, name)| {
                                let chat = menu_chat.clone();
                                let id = id.clone();
                                menu.item(
                                    PopupMenuItem::new(name.clone())
                                        .checked(id == chosen)
                                        .on_click(move |_, _, cx| {
                                            let id = id.clone();
                                            chat.update(cx, |this, cx| {
                                                this.wallet.rpc_chain = id;
                                                cx.notify();
                                            })
                                        }),
                                )
                            })
                        }),
                )
                .child(div().flex_1().child(Input::new(&page.rpc).small()))
                .child(
                    Button::new("wallet-rpc-save")
                        .primary()
                        .small()
                        .label("Save")
                        .on_click(move |_, window, cx| {
                            save_rpc.update(cx, |this, cx| this.save_rpc(window, cx))
                        }),
                ),
        );
    let add = chat.clone();
    let add_network = v_flex()
        .gap_2()
        .child(super::settings::label("ADD AN EVM NETWORK", cx))
        .child(
            h_flex()
                .gap_2()
                .child(div().flex_1().child(Input::new(&page.net_name).small()))
                .child(div().w(px(100.)).child(Input::new(&page.net_id).small()))
                .child(
                    div()
                        .w(px(100.))
                        .child(Input::new(&page.net_symbol).small()),
                ),
        )
        .child(
            h_flex()
                .gap_2()
                .child(div().flex_1().child(Input::new(&page.net_rpc).small()))
                .child(
                    Button::new("wallet-network-add")
                        .primary()
                        .small()
                        .label("Add")
                        .on_click(move |_, window, cx| {
                            add.update(cx, |this, cx| this.add_network(window, cx))
                        }),
                ),
        );
    let allow = chat.clone();
    let block = chat.clone();
    let listed = |title: &'static str, sites: &[String], cx: &App| {
        div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(format!(
                "{title}: {}",
                if sites.is_empty() {
                    "none".to_owned()
                } else {
                    sites.join(", ")
                }
            ))
    };
    let sites = v_flex()
        .gap_2()
        .child(super::settings::label("SITES", cx))
        .child(
            h_flex()
                .gap_2()
                .child(div().flex_1().child(Input::new(&page.site).small()))
                .child(
                    Button::new("wallet-site-allow")
                        .plain(cx)
                        .small()
                        .label("Pay only listed sites")
                        .on_click(move |_, window, cx| {
                            allow.update(cx, |this, cx| this.list_site(true, window, cx))
                        }),
                )
                .child(
                    Button::new("wallet-site-block")
                        .plain(cx)
                        .small()
                        .label("Never pay")
                        .on_click(move |_, window, cx| {
                            block.update(cx, |this, cx| this.list_site(false, window, cx))
                        }),
                ),
        )
        .child(listed("Paid only", &policy.sites_allow, cx))
        .child(listed("Never paid", &policy.sites_block, cx));
    v_flex()
        .gap_5()
        .child(super::settings::heading(
            "Wallet",
            "What the agent may pay for and how much. The wallets themselves are on the Wallet page.",
            cx,
        ))
        .child(paying)
        .child(
            v_flex()
                .gap_2()
                .child(super::settings::label("WHEN A SITE ASKS TO BE PAID (X402)", cx))
                .child(x402),
        )
        .child(limits)
        .child(saved_only)
        .child(
            v_flex()
                .gap_2()
                .child(super::settings::label("NETWORKS", cx))
                .child(testnets)
                .child(network_rows),
        )
        .child(rpc)
        .child(add_network)
        .child(sites)
}
