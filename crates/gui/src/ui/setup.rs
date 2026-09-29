//! First-run setup: how to reach a model, a project folder, and the model to
//! start with. One page, not a wizard, because signing in is already a
//! browser round trip and keys already have their rows in Gateways -- so
//! this only points at what already exists and reads live from it. Nothing
//! is tracked here: reopening it from the account menu shows exactly what
//! is set up by now, and Begin leaves however much of it was done.

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
                h_flex().w_full().justify_center().child(
                    v_flex()
                        .w(COLUMN)
                        .flex_shrink_0()
                        .px_6()
                        .py_8()
                        .gap_8()
                        .child(
                            v_flex()
                                .gap_1()
                                .child(
                                    div()
                                        .font_family(HEADING_FONT)
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_size(px(26.))
                                        .child("Set up Letronna"),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(
                                            "A few questions, then you are talking to it. All \
                                             of it can be changed later.",
                                        ),
                                ),
                        )
                        .child(self.reach_step(&chat, cx))
                        .child(self.folder_step(&chat, cx))
                        .child(self.model_step(&chat, cx)),
                ),
            )
            .vertical_scrollbar(&scroll)
    }

    /// Step 01: sign in, add a key, or point at a local model. Any one of
    /// the three is enough, and none of them is required to move on.
    fn reach_step(&self, chat: &Entity<Chat>, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .child(step_head(1, "How to reach models", cx))
            .child(self.sign_in_row(chat, cx))
            .child(
                v_flex()
                    .gap_2()
                    .child(super::settings::label("Use my own key", cx))
                    .child(super::settings::gateway_list(
                        self.key_gateway_rows(),
                        &self.gateway_pick,
                        &self.endpoint,
                        &self.key,
                        &self.settings_page,
                        chat,
                        cx,
                    )),
            )
            .child(self.local_model_row(cx))
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
                                .child(
                                    "Recommended: free starter credit and hosted models. Your \
                                     own key does not need this.",
                                ),
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

    /// Step 02: the folder the agent's tools read and write in this session.
    fn folder_step(&self, chat: &Entity<Chat>, cx: &mut Context<Self>) -> impl IntoElement {
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
            None => ("No folder chosen".to_owned(), String::new()),
        };
        let go = chat.clone();
        v_flex()
            .gap_3()
            .child(step_head(2, "Project folder", cx))
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
                                        .text_sm()
                                        .font_family(HEADING_FONT)
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(name),
                                )
                                .children((!path.is_empty()).then(|| {
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(path)
                                })),
                        )
                        .child(
                            Button::new("setup-folder")
                                .framed(cx)
                                .small()
                                .icon(Icon::new(AppIcon::Folder))
                                .label(if workspace.is_some() {
                                    "Change"
                                } else {
                                    "Choose a folder"
                                })
                                .on_click(move |_, window, cx| {
                                    go.update(cx, |this, cx| this.attach_workspace(window, cx))
                                }),
                        ),
                ),
            )
    }

    /// Step 03: the model new sessions start on. Every set-up gateway's
    /// models are here, cheapest and priciest alike, with what each costs;
    /// Begin leaves however much of this was picked.
    fn model_step(&self, chat: &Entity<Chat>, cx: &mut Context<Self>) -> impl IntoElement {
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
            .max_h(px(260.))
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
                        "No models yet. Reach one in the step above first."
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
        let begin = chat.clone();
        v_flex()
            .gap_3()
            .child(step_head(3, "Default model", cx))
            .child(list)
            .child(
                h_flex().justify_end().child(
                    Button::new("setup-begin")
                        .primary()
                        .small()
                        .label("Begin")
                        .on_click(move |_, window, cx| {
                            begin.update(cx, |this, cx| this.close_setup(window, cx))
                        }),
                ),
            )
    }
}

/// "01", "02", "03", set apart from a step's title, with a hairline under
/// both, as the site's `.cap-n` marks a numbered step.
fn step_head(n: u8, title: &'static str, cx: &App) -> Div {
    h_flex()
        .gap_3()
        .items_baseline()
        .pb_2()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(
            div()
                .font_family(HEADING_FONT)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child(format!("{n:02}")),
        )
        .child(
            div()
                .text_base()
                .font_family(HEADING_FONT)
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
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
