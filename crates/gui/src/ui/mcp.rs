//! Settings › MCP: each MCP server, how it is doing, and what it printed.
//!
//! The page reads core's status afresh each time it is drawn rather than
//! keeping a copy, so it is never staler than the last redraw. Nothing
//! ticks: while a server is still starting, one wait off the UI thread for
//! the servers to settle asks for the redraw that shows how it went.
//! The log tail is shown here and nowhere else the model could read it.

use crate::prelude::*;
use letronna_core::mcp::{ServerStatus, State};

/// How long a wait for starting servers lasts at most: the minute core gives
/// a server to start and list its tools.
const SETTLE_WAIT: std::time::Duration = std::time::Duration::from_secs(60);
/// Tallest a log gets before it scrolls: about fifteen lines.
const LOG_HEIGHT: Pixels = px(240.);

pub(crate) fn mcp_page(chat: &Entity<Chat>, window: &mut Window, cx: &mut App) -> Div {
    let servers = letronna_core::mcp::status();
    let trouble = window.use_keyed_state("mcp-trouble", cx, |_, _| None::<String>);
    let waiting = window.use_keyed_state("mcp-waiting", cx, |_, _| false);
    if servers.iter().any(|s| s.state == State::Starting) && !*waiting.read(cx) {
        settle(chat, &waiting, cx);
    }
    let problem = letronna_core::mcp::unreadable().or_else(|| trouble.read(cx).clone());
    let mut list = v_flex();
    if servers.is_empty() {
        list = list.child(
            div()
                .py_3()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(format!(
                    "No servers yet. Add one with `letronna mcp add` in a terminal, or in {}.",
                    letronna_core::mcp::config_path().display()
                )),
        );
    }
    for server in &servers {
        list = list.child(server_row(chat, server, &trouble, &waiting, window, cx));
    }
    v_flex()
        .gap_4()
        .child(super::settings::heading(
            "MCP servers",
            "Servers listed in mcp.json. Their tools join Letronna's own; what they print stays here.",
            cx,
        ))
        .children(problem.map(|problem| {
            div()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(problem)
        }))
        .child(list)
}

/// Wait off the UI thread for the servers still starting, then redraw.
fn settle(chat: &Entity<Chat>, waiting: &Entity<bool>, cx: &mut App) {
    waiting.update(cx, |waiting, _| *waiting = true);
    let settled = cx
        .background_executor()
        .spawn(async { letronna_core::mcp::ready(SETTLE_WAIT) });
    let (chat, waiting) = (chat.clone(), waiting.clone());
    cx.spawn(async move |cx| {
        settled.await;
        let _ = waiting.update(cx, |waiting, _| *waiting = false);
        let _ = chat.update(cx, |_, cx| cx.notify());
    })
    .detach();
}

/// One server: its name and state, what it runs, why it failed, its log when
/// asked for, and what can be done with it.
fn server_row(
    chat: &Entity<Chat>,
    server: &ServerStatus,
    trouble: &Entity<Option<String>>,
    waiting: &Entity<bool>,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let name = server.name.clone();
    let key = |what: &str| SharedString::from(format!("mcp-{what}-{name}"));
    let shown = window.use_keyed_state(key("log-shown"), cx, |_, _| false);
    let scroll = window
        .use_keyed_state(key("log-scroll"), cx, |_, _| ScrollHandle::new())
        .read(cx)
        .clone();
    let open = *shown.read(cx) && !server.log.trim().is_empty();
    let (state, colour) = match server.state {
        State::Connected => ("Connected", cx.theme().green),
        State::Starting => ("Starting", cx.theme().warning),
        State::Failed => ("Failed", cx.theme().danger),
        State::Disabled => ("Off", cx.theme().muted_foreground),
    };
    let what = match server.state {
        State::Connected => format!("{} · {} tools", server.transport, server.tools),
        _ => server.transport.to_owned(),
    };
    let head = h_flex()
        .flex_1()
        .min_w_0()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .child(name.clone()),
        )
        .child(super::messaging::pill(state, colour, cx))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(what),
        );
    let redraw = {
        let chat = chat.clone();
        move |cx: &mut App| chat.update(cx, |_, cx| cx.notify())
    };
    let log_button = (!server.log.trim().is_empty()).then(|| {
        let (shown, scroll, redraw) = (shown.clone(), scroll.clone(), redraw.clone());
        Button::new(key("log"))
            .framed(cx)
            .xsmall()
            .label(if open { "Hide log" } else { "Log" })
            .on_click(move |_, _, cx| {
                shown.update(cx, |shown, _| *shown = !*shown);
                // Opened at its end, where the newest lines are.
                scroll.scroll_to_item(1);
                redraw(cx);
            })
    });
    let reconnect = (server.state != State::Disabled).then(|| {
        let (chat, trouble, waiting, name) =
            (chat.clone(), trouble.clone(), waiting.clone(), name.clone());
        Button::new(key("reconnect"))
            .framed(cx)
            .xsmall()
            .label("Reconnect")
            .on_click(move |_, _, cx| {
                let done = letronna_core::mcp::reconnect(&name);
                acted(done, &chat, &trouble, &waiting, cx);
            })
    });
    let on = server.state == State::Disabled;
    let switch = {
        let (chat, trouble, waiting, name) =
            (chat.clone(), trouble.clone(), waiting.clone(), name.clone());
        Button::new(key("switch"))
            .framed(cx)
            .xsmall()
            .label(if on { "Turn on" } else { "Turn off" })
            .on_click(move |_, _, cx| {
                let done = letronna_core::mcp::set_enabled(&name, on);
                acted(done, &chat, &trouble, &waiting, cx);
            })
    };
    let actions = h_flex()
        .flex_shrink_0()
        .gap_1()
        .children(log_button)
        .children(reconnect)
        .child(switch);
    v_flex()
        .gap_1p5()
        .py_3()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(head)
                .child(actions),
        )
        .child(
            div()
                .text_xs()
                .font_family(MONO_FONT)
                .text_color(cx.theme().muted_foreground)
                .truncate()
                .child(server.target.clone()),
        )
        .children(
            server
                .error
                .clone()
                .map(|error| div().text_xs().text_color(cx.theme().danger).child(error)),
        )
        .when(open, |row| {
            row.child(
                div()
                    .id(key("log-view"))
                    .track_scroll(&scroll)
                    .max_h(LOG_HEIGHT)
                    .overflow_y_scroll()
                    .p_2()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().secondary)
                    .font_family(MONO_FONT)
                    .text_xs()
                    .child(server.log.clone())
                    // The end, which opening the log scrolls to.
                    .child(div()),
            )
        })
}

/// Show why an action failed, or wait for the server it started; then redraw.
fn acted(
    done: Result<(), String>,
    chat: &Entity<Chat>,
    trouble: &Entity<Option<String>>,
    waiting: &Entity<bool>,
    cx: &mut App,
) {
    let failed = done.err();
    let started = failed.is_none();
    trouble.update(cx, |trouble, _| *trouble = failed);
    if started && !*waiting.read(cx) {
        settle(chat, waiting, cx);
    }
    chat.update(cx, |_, cx| cx.notify());
}
