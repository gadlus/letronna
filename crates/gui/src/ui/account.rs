//! The Letronna account, in the window.
//!
//! The terminal and the window share `account.json`, so this only ever reads
//! that file and asks the API about it: sign in with `/login` in one and the
//! other shows it the next time it looks. The network work runs on a thread
//! of its own and reports back, the way the model list does.

use letronna_core::auth::{self, Account};

use crate::prelude::*;

impl Chat {
    /// Re-read the file, then check it with the API. Called when the page is
    /// opened, so a sign-in or sign-out from the terminal shows up here.
    pub(crate) fn refresh_account(&mut self, cx: &mut Context<Self>) {
        self.account = auth::account();
        if self.account.is_none() {
            cx.notify();
            return;
        }
        self.off_thread(auth::refresh, cx);
        self.load_teams(cx);
    }

    /// The teams this account is in, fetched off the window's thread.
    fn load_teams(&mut self, cx: &mut Context<Self>) {
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(auth::teams());
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(teams)) = rx.recv().await {
                let _ = this.update(cx, |this, cx| {
                    this.teams = teams;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Bill hosted models to a team's pool, or (`None`) to your own credit.
    /// Shared with the terminal: `/team` there and this are the same choice.
    pub(crate) fn pick_team(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.signing_in = true;
        self.off_thread(move || auth::use_team(id.as_deref()), cx);
    }

    /// Settings, open on this page.
    pub(crate) fn open_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_page.update(cx, |page, cx| {
            *page = "Account";
            cx.notify();
        });
        self.open_settings(window, cx);
    }

    /// Open the browser and wait for it to hand a token back. Asked again
    /// while it waits, it shows the same page again: the tab may have been
    /// closed, or never opened in the browser that is in use.
    pub(crate) fn sign_in(&mut self, cx: &mut Context<Self>) {
        if let Some(url) = &self.sign_in_url {
            cx.open_url(url);
            return;
        }
        if self.signing_in {
            return;
        }
        let pending = match auth::begin(auth::Client::App) {
            Ok(pending) => pending,
            Err(error) => {
                self.account_note = Some(error);
                cx.notify();
                return;
            }
        };
        cx.open_url(&pending.url);
        self.signing_in = true;
        self.sign_in_url = Some(pending.url.clone());
        self.sign_in_cancel = Some(pending.cancel());
        // The waiting card on the Account page says what to do now.
        self.account_note = None;
        self.off_thread(move || pending.finish(), cx);
    }

    /// Stop waiting for the browser; the port closes at once.
    pub(crate) fn cancel_sign_in(&mut self) {
        if let Some(cancel) = &self.sign_in_cancel {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    pub(crate) fn sign_out(&mut self, cx: &mut Context<Self>) {
        self.signing_in = true;
        self.off_thread(
            || {
                auth::sign_out();
                Err("Signed out. Your chats and settings stay where they are.".into())
            },
            cx,
        );
    }

    /// Do account work off the window's thread and take in what it found.
    fn off_thread(
        &mut self,
        work: impl FnOnce() -> Result<Account, String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(work());
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                let _ = this.update(cx, |this, cx| {
                    this.signing_in = false;
                    // A sign-in that was waiting is over, however it ended.
                    this.sign_in_url = None;
                    this.sign_in_cancel = None;
                    // Whatever happened, the file says who is signed in now.
                    this.account = auth::account();
                    this.account_note = match result {
                        Ok(account) if this.account.is_some() => {
                            this.account = Some(account);
                            None
                        }
                        Ok(_) => None,
                        Err(note) => Some(note),
                    };
                    cx.notify();
                });
            }
        })
        .detach();
    }
}

/// Drawn while the window is itself being drawn, so it is handed what it
/// shows rather than reading the chat, which is mid-update and would panic.
pub(crate) fn account_page(chat: &Entity<Chat>, this: &Chat, cx: &App) -> Div {
    let busy = this.signing_in;
    let muted = cx.theme().muted_foreground;
    let note = this
        .account_note
        .clone()
        .map(|note| div().text_sm().text_color(muted).child(note));

    let Some(account) = this.account.clone() else {
        if let Some(url) = this.sign_in_url.clone() {
            return v_flex()
                .gap_4()
                .child(super::settings::heading(
                    "Account",
                    "Hosted models and cloud runs need a Letronna account. Your own keys do not.",
                    cx,
                ))
                .child(waiting_card(chat, url, cx));
        }
        let chat = chat.clone();
        return v_flex()
            .gap_4()
            .child(super::settings::heading(
                "Account",
                "Hosted models and cloud runs need a Letronna account. Your own keys do not.",
                cx,
            ))
            .child(
                h_flex().child(
                    Button::new("sign-in")
                        .primary()
                        .small()
                        .loading(busy)
                        .disabled(busy)
                        .label("Sign in in the browser")
                        .on_click(move |_, _, cx| chat.update(cx, |this, cx| this.sign_in(cx))),
                ),
            )
            .children(note);
    };

    let row = |name: &'static str, value: String| {
        h_flex()
            .justify_between()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(super::settings::label(name, cx))
            .child(div().text_sm().child(value))
    };
    let plan = match account.plan_name() {
        "" => "Not checked yet".to_owned(),
        plan => plan.to_owned(),
    };
    let (refresh, out) = (chat.clone(), chat.clone());
    v_flex()
        .gap_4()
        .child(super::settings::heading(
            "Account",
            "Signed in here and in the terminal alike; sign out in either and both are.",
            cx,
        ))
        .child(
            v_flex()
                .child(row("Name", account.name.clone()))
                .child(row("Email", account.email.clone()))
                .child(row("Plan", plan))
                .child(row("Credits", account.credit()))
                .child(row(
                    "Team",
                    match &account.team {
                        Some(t) => format!(
                            "{} · pool ${}.{:02}",
                            t.name,
                            t.credits / 100,
                            t.credits.rem_euclid(100)
                        ),
                        None => "None · your own credit".to_owned(),
                    },
                )),
        )
        .child(billing(chat, this, &account, cx))
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("dashboard")
                        .small()
                        .label("Open dashboard")
                        .on_click(|_, _, cx| cx.open_url(&auth::dashboard())),
                )
                .child(
                    Button::new("refresh-account")
                        .small()
                        .loading(busy)
                        .disabled(busy)
                        .label("Refresh")
                        .on_click(move |_, _, cx| {
                            refresh.update(cx, |this, cx| this.refresh_account(cx))
                        }),
                )
                .child(
                    Button::new("sign-out")
                        .small()
                        .disabled(busy)
                        .label("Sign out")
                        .on_click(move |_, _, cx| out.update(cx, |this, cx| this.sign_out(cx))),
                ),
        )
        .children(note)
}

/// Who hosted models bill: your own credit, or one of your teams' pools. One
/// pays for a call, never both; the stake allowance, if any, is spent first.
fn billing(chat: &Entity<Chat>, this: &Chat, account: &Account, cx: &App) -> Div {
    let busy = this.signing_in;
    let current = account.team.as_ref().map(|t| t.id.clone());
    let choice = |key: usize, label: String, id: Option<String>| {
        let chat = chat.clone();
        let on = current == id;
        Button::new(("bill", key))
            .small()
            .when(on, |b| b.primary())
            .disabled(busy || on)
            .label(label)
            .on_click(move |_, _, cx| {
                let id = id.clone();
                chat.update(cx, |this, cx| this.pick_team(id, cx))
            })
    };
    let mut row = h_flex()
        .gap_2()
        .flex_wrap()
        .child(choice(0, "Personal".into(), None));
    for (i, t) in this.teams.iter().enumerate() {
        row = row.child(choice(i + 1, t.name.clone(), Some(t.id.clone())));
    }
    v_flex()
        .gap_2()
        .child(super::settings::label("Bill hosted models to", cx))
        .child(row)
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(if this.teams.is_empty() {
                    "Not in a team. Create or join one at letronna.ai/dashboard/team."
                } else {
                    "One pays for each call, never both. Same choice as /team in the terminal."
                }),
        )
}

/// What to do while the browser has the sign-in: the page may have been
/// closed, may not have opened at all, or may have opened in a browser that
/// is not the one in use -- carrying it across part way fails, so the link
/// is offered to start over in the right one.
fn waiting_card(chat: &Entity<Chat>, url: String, cx: &App) -> Div {
    let (again, copy, cancel) = (chat.clone(), url, chat.clone());
    v_flex()
        .gap_3()
        .p_4()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    gpui_component::spinner::Spinner::new()
                        .small()
                        .color(cx.theme().primary),
                )
                .child(
                    div()
                        .text_sm()
                        .child("Waiting for you to allow Letronna in the browser."),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(
                    "No page opened, or not in the browser you use? Copy the link into the one \
                     you are signed in to, and finish there.",
                ),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("sign-in-again")
                        .primary()
                        .small()
                        .label("Open the page again")
                        .on_click(move |_, _, cx| again.update(cx, |this, cx| this.sign_in(cx))),
                )
                .child(
                    Button::new("sign-in-copy")
                        .outline()
                        .small()
                        .label("Copy link")
                        .on_click(move |_, window, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                            window.push_notification(
                                gpui_component::notification::Notification::success(
                                    "Link copied. Open it in the browser you use.",
                                ),
                                cx,
                            );
                        }),
                )
                .child(
                    Button::new("sign-in-cancel")
                        .ghost()
                        .small()
                        .label("Cancel")
                        .on_click(move |_, _, cx| {
                            cancel.update(cx, |this, cx| {
                                this.cancel_sign_in();
                                cx.notify();
                            })
                        }),
                ),
        )
}
