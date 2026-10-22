//! A web search in the conversation, for any model: a card that stays above
//! the answer, saying what was searched and how many pages came back, and
//! listing them -- each opening in the browser -- once clicked.
//!
//! It is never folded into a run's one line (core's `tool_runs` keeps
//! searches out of runs): the pages an answer leans on are worth a click
//! to check. While it runs its glass breathes on the busy clock, which
//! draws only while something is moving.

use crate::prelude::*;
use letronna_core::search::{self, Source};

/// What a search asked for, from its call's arguments.
pub(crate) fn query_of(args: &str) -> String {
    serde_json::from_str::<serde_json::Value>(args)
        .ok()
        .and_then(|args| args["query"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The site a page is on, as people say it: its host without "www.".
fn site(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .map(|host| host.trim_start_matches("www.").to_owned())
        .unwrap_or_default()
}

/// "Searched the web · 5 results", or what stood in for them.
fn told(count: usize) -> String {
    match count {
        0 => "Searched the web · no results".to_owned(),
        1 => "Searched the web · 1 result".to_owned(),
        n => format!("Searched the web · {n} results"),
    }
}

impl Chat {
    /// A search under way, in place of the spinner line other calls get.
    pub(crate) fn searching_line(
        &self,
        args: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let query = query_of(args);
        let breath = crate::ui::busy::breathing(window, cx);
        h_flex()
            .gap_2()
            .items_center()
            .text_xs()
            .child(
                Icon::new(AppIcon::Search)
                    .size_3p5()
                    .text_color(cx.theme().primary.opacity(breath)),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(cx.theme().foreground)
                    .child("Searching the web"),
            )
            .when(!query.is_empty(), |row| {
                row.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("“{query}”")),
                )
            })
            .into_any_element()
    }

    /// A search done: what it asked and found, its pages behind a click.
    pub(crate) fn search_card(
        &self,
        index: usize,
        message: &Message,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = (self.store.conversations[self.store.active].id, index);
        // A failed search starts open, its reason being the news, as a
        // failed call's output does.
        let open = self.thinking_open.contains(&key) != message.is_error;
        let query = query_of(&message.args);
        let sources: Vec<Source> = if message.is_error {
            Vec::new()
        } else {
            search::sources(&message.content)
        };
        let (headline, color) = if message.is_error {
            ("Search failed".to_owned(), cx.theme().danger)
        } else {
            (told(sources.len()), cx.theme().foreground)
        };
        let openable = message.is_error || !sources.is_empty();
        let header = h_flex()
            .id(("search-card", index))
            .w_full()
            .min_w_0()
            .gap_2()
            .items_center()
            .when(openable, |row| {
                row.cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.thinking_open.remove(&key) {
                            this.thinking_open.insert(key);
                        }
                        cx.notify();
                    }))
            })
            .child(
                Icon::new(AppIcon::Search)
                    .size_3p5()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(div().text_color(color).child(headline))
                    .when(!query.is_empty(), |col| {
                        col.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("“{query}”")),
                        )
                    }),
            )
            .when(openable, |row| {
                row.child(
                    Icon::new(if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .size_3()
                    .text_color(cx.theme().muted_foreground),
                )
            });
        let mut card = v_flex()
            .w_full()
            .min_w_0()
            .px_3()
            .py_2()
            .gap_2()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .text_xs()
            .child(header);
        if open && message.is_error {
            card = card.child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(letronna_core::tools::one_line(&message.content)),
            );
        }
        if open && !sources.is_empty() {
            let mut list = v_flex()
                .gap_0p5()
                .pt_1()
                .border_t_1()
                .border_color(cx.theme().border);
            for (n, source) in sources.into_iter().enumerate() {
                let url = source.url.clone();
                list = list.child(
                    h_flex()
                        .id(("search-source", index * 100 + n))
                        .w_full()
                        .min_w_0()
                        .py_0p5()
                        .gap_3()
                        .justify_between()
                        .cursor_pointer()
                        .on_click(move |_, _, cx| cx.open_url(&url))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(cx.theme().link)
                                .child(source.title),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(cx.theme().muted_foreground)
                                .child(site(&source.url)),
                        ),
                );
            }
            card = card.child(list);
        }
        card.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{query_of, site, told};

    #[test]
    fn a_search_says_what_it_asked_and_where_its_pages_are() {
        assert_eq!(
            query_of(r#"{"query": "tokyo weather", "count": 5}"#),
            "tokyo weather"
        );
        assert_eq!(query_of("not json"), "");
        assert_eq!(site("https://www.example.com/a/b"), "example.com");
        assert_eq!(told(1), "Searched the web · 1 result");
        assert_eq!(told(5), "Searched the web · 5 results");
        assert_eq!(told(0), "Searched the web · no results");
    }
}
