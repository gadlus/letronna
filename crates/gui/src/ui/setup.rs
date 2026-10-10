//! First-run setup, over the whole window: a welcome, then one step at a
//! time -- how to reach a model, the model to start with, a project folder.
//! Each step points at what already exists (sign-in, the Gateways rows, the
//! model list) and reads live from it, so nothing is tracked here but which
//! step is open, and every step can be skipped.

use crate::prelude::*;

/// Whether a launch opens on the setup page: nothing reachable, and no
/// conversation on screen to hide. Someone with history and only Ollama
/// would otherwise lose it behind the page on every launch.
pub(crate) fn needed(signed_in: bool, has_key: bool, has_endpoint: bool, empty_chat: bool) -> bool {
    !signed_in && !has_key && !has_endpoint && empty_chat
}

/// How many models the setup page lists before pointing at the picker.
const SETUP_MODELS: usize = 50;

impl Chat {
    /// Bring the page back, whatever is reachable by now.
    pub(crate) fn open_setup(&mut self, cx: &mut Context<Self>) {
        self.page = Page::Chat;
        self.show_setup = Some(self.store.conversations[self.store.active].id);
        cx.notify();
    }

    /// Leave the page: Begin, or asking anything at all.
    pub(crate) fn close_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_setup = None;
        self.focus_composer(window, cx);
        cx.notify();
    }

    pub(crate) fn setup_page(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let chat = cx.entity();
        let step = window.use_keyed_state("setup-step", cx, |_, _| 0usize);
        let way = window.use_keyed_state("setup-way", cx, |_, _| 0usize);
        let now = (*step.read(cx)).min(STEPS - 1);
        let go = |to: usize| {
            let (step, chat) = (step.clone(), chat.clone());
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                step.update(cx, |s, _| *s = to);
                chat.update(cx, |_, cx| cx.notify());
            }
        };
        let page = match now {
            0 => self.welcome_step(go(1), cx).into_any_element(),
            1 => (self.reach_step(&chat, &way, go(0), go(2), cx)).into_any_element(),
            2 => (self.model_step(&chat, go(1), go(3), cx)).into_any_element(),
            _ => self.folder_step(&chat, go(2), cx).into_any_element(),
        };
        let scroll = window
            .use_keyed_state("setup-scroll", cx, |_, _| ScrollHandle::default())
            .read(cx)
            .clone();
        div()
            .id("setup")
            .relative()
            .flex_1()
            .min_h_0()
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .child(
                v_flex()
                    .min_h_full()
                    .w_full()
                    .items_center()
                    .justify_center()
                    .py_10()
                    .child(
                        v_flex()
                            .w(px(460.))
                            .max_w_full()
                            .px_6()
                            .gap_8()
                            .child(progress(now, cx))
                            .child(page),
                    ),
            )
            .vertical_scrollbar(&scroll)
    }

    /// Who it is and one way on: nothing to read or choose yet.
    fn welcome_step(
        &self,
        next: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .gap_6()
            .child(img("images/letronna.png").size(px(64.)).rounded(px(14.)))
            .child(heading(
                "Welcome to Letronna",
                "An agent that works on your computer: it reads, writes, runs and \
                 browses for you. Connect a model and it is ready.",
                cx,
            ))
            .child(
                h_flex().child(
                    Button::new("setup-start")
                        .primary()
                        .label("Get started")
                        .on_click(next),
                ),
            )
    }

    /// Sign in, add a key, or point at a local model: one picked, its
    /// details under it. Any one is enough to go on.
    fn reach_step(
        &self,
        chat: &Entity<Chat>,
        way: &Entity<usize>,
        back: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
        next: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let picked = *way.read(cx);
        let ollama = self.models.iter().filter(|(g, _)| g == "Ollama").count();
        let ways = [
            (
                "Letronna account",
                "Hosted models with free starter credit. Recommended.",
                self.account.is_some(),
            ),
            (
                "My own API key",
                "OpenAI, Anthropic, OpenRouter, or any compatible server.",
                self.key_gateway_rows().iter().any(|r| r.set_up),
            ),
            (
                "On this computer",
                "Ollama: works offline, no key.",
                ollama > 0,
            ),
        ];
        let mut options = v_flex().gap_2();
        for (i, (title, blurb, done)) in ways.into_iter().enumerate() {
            let (way, chat) = (way.clone(), chat.clone());
            let on = i == picked;
            let ring = if on {
                cx.theme().primary
            } else {
                cx.theme().muted_foreground.opacity(0.6)
            };
            options = options.child(
                h_flex()
                    .id(("setup-way", i))
                    .gap_3()
                    .items_center()
                    .px_4()
                    .py_3()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(if on {
                        cx.theme().primary
                    } else {
                        cx.theme().border
                    })
                    .cursor_pointer()
                    .child(
                        div()
                            .size(px(14.))
                            .flex_shrink_0()
                            .rounded_full()
                            .border_1()
                            .border_color(ring)
                            .when(on, |d| {
                                d.child(
                                    div()
                                        .m(px(3.))
                                        .size(px(6.))
                                        .rounded_full()
                                        .bg(cx.theme().primary),
                                )
                            }),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                div()
                                    .font_family(HEADING_FONT)
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(blurb),
                            ),
                    )
                    .when(done, |d| {
                        d.child(
                            Icon::new(IconName::Check)
                                .size_4()
                                .text_color(cx.theme().green),
                        )
                    })
                    .on_click(move |_, _, cx| {
                        way.update(cx, |w, _| *w = i);
                        chat.update(cx, |_, cx| cx.notify());
                    }),
            );
        }
        let detail = match picked {
            0 => self.sign_in_row(chat, cx).into_any_element(),
            1 => super::settings::gateway_list(
                self.key_gateway_rows(),
                &self.gateway_pick,
                &self.endpoint,
                &self.key,
                &self.settings_page,
                chat,
                cx,
            )
            .into_any_element(),
            _ => self.local_model_row(cx).into_any_element(),
        };
        let ready = self.account.is_some() || !self.models.is_empty();
        v_flex()
            .gap_6()
            .child(heading(
                "Connect a model",
                "Pick one way in. The others stay in Settings for later.",
                cx,
            ))
            .child(options)
            .child(detail)
            .child(footer(
                "setup-reach",
                back,
                next,
                if ready { "Continue" } else { "Skip for now" },
                ready,
                cx,
            ))
    }

    /// Already signed in, waiting on the browser, or not started: the same
    /// three states the Account page shows, in one row here.
    fn sign_in_row(&self, chat: &Entity<Chat>, cx: &mut Context<Self>) -> Div {
        if let Some(account) = &self.account {
            return row_card(cx).child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .font_family(HEADING_FONT)
                            .font_weight(FontWeight::MEDIUM)
                            .child(format!("Signed in as {}", account.name)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Hosted models, charged to your credit."),
                    ),
            );
        }
        if let Some(url) = self.sign_in_url.clone() {
            return super::account::waiting_card(chat, url, cx);
        }
        let go = chat.clone();
        row_card(cx).child(
            h_flex()
                .justify_between()
                .items_center()
                .gap_3()
                .child(
                    v_flex()
                        .min_w_0()
                        .gap_0p5()
                        .child(
                            div()
                                .text_sm()
                                .font_family(HEADING_FONT)
                                .font_weight(FontWeight::MEDIUM)
                                .child("Sign in to Letronna"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Opens your browser, then comes back here."),
                        ),
                )
                .child(
                    Button::new("setup-sign-in")
                        .primary()
                        .small()
                        .loading(self.signing_in)
                        .disabled(self.signing_in)
                        .label("Sign in in the browser")
                        .on_click(move |_, _, cx| go.update(cx, |this, cx| this.sign_in(cx))),
                ),
        )
    }

    /// Ollama needs no key and answers on its own port, so this only says
    /// whether it was found, the same list every gateway's models come from.
    fn local_model_row(&self, cx: &mut Context<Self>) -> Div {
        let found = self.models.iter().filter(|(g, _)| g == "Ollama").count();
        let (dot, status) = if self.loading_models {
            (
                cx.theme().muted_foreground.opacity(0.5),
                "Checking…".to_owned(),
            )
        } else if found > 0 {
            (
                cx.theme().green,
                format!(
                    "Detected · {found} model{}",
                    if found == 1 { "" } else { "s" }
                ),
            )
        } else {
            (
                cx.theme().muted_foreground.opacity(0.5),
                "Not found. Install Ollama and it is picked up automatically.".to_owned(),
            )
        };
        row_card(cx).child(
            h_flex()
                .justify_between()
                .items_center()
                .gap_3()
                .child(
                    v_flex()
                        .min_w_0()
                        .gap_0p5()
                        .child(
                            div()
                                .text_sm()
                                .font_family(HEADING_FONT)
                                .font_weight(FontWeight::MEDIUM)
                                .child("Local model"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Ollama, on this machine. Works offline; no key."),
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
                                .text_color(cx.theme().muted_foreground)
                                .child(status),
                        ),
                ),
        )
    }

    /// Last: the folder its tools work in, which can be skipped.
    fn folder_step(
        &self,
        chat: &Entity<Chat>,
        back: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let workspace = self.store.conversations[self.store.active]
            .workspace
            .clone();
        let (name, path) = match &workspace {
            Some(p) => (
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.display().to_string()),
                p.display().to_string(),
            ),
            None => (
                "No folder yet".to_owned(),
                "Until you pick one it works in Letronna, in your home folder.".to_owned(),
            ),
        };
        let go = chat.clone();
        let done = chat.clone();
        v_flex()
            .gap_6()
            .child(heading(
                "Where should it work?",
                "Pick a project folder for its files and commands. You can change it any time.",
                cx,
            ))
            .child(
                row_card(cx).child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .gap_3()
                        .child(
                            v_flex()
                                .min_w_0()
                                .gap_0p5()
                                .child(
                                    div()
                                        .font_family(HEADING_FONT)
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(name),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(path),
                                ),
                        )
                        .child(
                            Button::new("setup-folder")
                                .framed(cx)
                                .small()
                                .icon(Icon::new(AppIcon::Folder))
                                .label(if workspace.is_some() {
                                    "Change"
                                } else {
                                    "Choose"
                                })
                                .on_click(move |_, window, cx| {
                                    go.update(cx, |this, cx| this.attach_workspace(window, cx))
                                }),
                        ),
                ),
            )
            .child(footer(
                "setup-folder-nav",
                back,
                move |_, window, cx| done.update(cx, |this, cx| this.close_setup(window, cx)),
                "Start using Letronna",
                true,
                cx,
            ))
    }

    /// The model new sessions start on. Every set-up gateway's models are
    /// here, cheapest and priciest alike, with what each costs.
    fn model_step(
        &self,
        chat: &Entity<Chat>,
        back: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
        next: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (on, current) = self.store.model(self.store.active);
        let (on, current) = (on.to_owned(), current.to_owned());
        let custom = self.store.settings.custom_endpoint.clone();
        // Not the picker's search text: its box is not on this page, so a
        // leftover query would hide models with nothing to clear it. And a
        // gateway can list hundreds, each a button built every frame.
        let mut matches = self.models_named("");
        let more = matches.len().saturating_sub(SETUP_MODELS);
        matches.truncate(SETUP_MODELS);
        let mut list = v_flex()
            .id("setup-models")
            .max_h(px(320.))
            .overflow_y_scroll()
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius);
        if matches.is_empty() {
            list = list.child(
                div()
                    .px_4()
                    .py_3()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if self.loading_models {
                        "Loading models…"
                    } else {
                        "No models yet: go back and connect one, or skip and do it later."
                    }),
            );
        }
        for (i, (gw, id)) in matches.into_iter().enumerate() {
            let active = gw == on && id == current;
            let price = letronna_core::models::facts(&base_url(&gw, &custom), &id)
                .price
                .map(|p| price_per_million(&p));
            let (pick_gw, pick) = (gw.clone(), id.clone());
            let pick_chat = chat.clone();
            let row = Button::new(("setup-model", i))
                .plain(cx)
                .small()
                .w_full()
                .justify_start()
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .justify_between()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .when(active, |d| d.text_color(cx.theme().link))
                                .child(id),
                        )
                        .child(
                            h_flex()
                                .flex_shrink_0()
                                .gap_2()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(gw)
                                .children(price),
                        ),
                )
                .on_click(move |_, window, cx| {
                    pick_chat.update(cx, |this, cx| {
                        this.choose_model(pick_gw.clone(), pick.clone(), window, cx)
                    })
                });
            list = list.child(
                div()
                    .when(i > 0, |d| d.border_t_1().border_color(cx.theme().border))
                    .child(row),
            );
        }
        if more > 0 {
            list = list.child(
                div()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{more} more in the model picker.")),
            );
        }
        v_flex()
            .gap_6()
            .child(heading(
                "Pick a model",
                "New sessions start on it. You can switch in any chat.",
                cx,
            ))
            .child(list)
            .child(footer(
                "setup-model",
                back,
                next,
                if self.models.is_empty() {
                    "Skip for now"
                } else {
                    "Continue"
                },
                !self.models.is_empty(),
                cx,
            ))
    }
}

/// How many steps: welcome, connect, model, folder.
const STEPS: usize = 4;

/// A thin bar per step, the ones done and the one open filled in.
fn progress(now: usize, cx: &App) -> Div {
    h_flex().gap_1p5().children((0..STEPS).map(|i| {
        div().h(px(3.)).flex_1().rounded_full().bg(if i <= now {
            cx.theme().primary
        } else {
            cx.theme().border
        })
    }))
}

/// A step's title and what it is for.
fn heading(title: &'static str, blurb: &'static str, cx: &App) -> Div {
    v_flex()
        .gap_2()
        .child(
            div()
                .font_family(HEADING_FONT)
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(px(24.))
                .child(title),
        )
        .child(
            div()
                .text_color(cx.theme().muted_foreground)
                .line_height(relative(1.5))
                .child(blurb),
        )
}

/// Back on the left, on to the next step on the right.
fn footer(
    id: &'static str,
    back: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    next: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    label: &'static str,
    ready: bool,
    cx: &App,
) -> Div {
    // Skipping is quieter than going on with something set up.
    let on = Button::new((id, 1usize)).label(label).on_click(next);
    h_flex()
        .justify_between()
        .items_center()
        .child(
            Button::new((id, 0usize))
                .plain(cx)
                .label("Back")
                .on_click(back),
        )
        .child(if ready { on.primary() } else { on.framed(cx) })
}

/// A step's own row: a hairline around it, on the page colour, rather than
/// a card standing off the page in a shadow.
fn row_card(cx: &App) -> Div {
    div()
        .px_4()
        .py_3()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
}

/// What a model costs, per the site's model pages: "$3.00 in · $15.00 out /M".
fn price_per_million(price: &letronna_core::models::Price) -> String {
    let per = |usd: f64| {
        let per_m = usd * 1_000_000.;
        if per_m >= 1. {
            format!("${per_m:.2}")
        } else {
            format!("${per_m:.3}")
        }
    };
    format!("{} in · {} out /M", per(price.input), per(price.output))
}

#[cfg(test)]
mod tests {
    use super::{needed, price_per_million};
    use letronna_core::models::Price;

    #[test]
    fn a_launch_over_a_conversation_does_not_open_setup() {
        assert!(needed(false, false, false, true));
        assert!(!needed(false, false, false, false));
        assert!(!needed(true, false, false, true));
        assert!(!needed(false, true, false, true));
        assert!(!needed(false, false, true, true));
    }

    #[test]
    fn a_price_reads_as_dollars_per_million_tokens() {
        let price = Price {
            input: 0.000_003,
            output: 0.000_015,
            ..Default::default()
        };
        assert_eq!(price_per_million(&price), "$3.00 in · $15.00 out /M");
    }

    #[test]
    fn a_fraction_under_a_dollar_keeps_three_places() {
        let price = Price {
            input: 0.000_000_25,
            output: 0.000_001,
            ..Default::default()
        };
        assert_eq!(price_per_million(&price), "$0.250 in · $1.00 out /M");
    }
}
