//! Capabilities › MCP: each MCP server, how it is doing, what it printed,
//! and its tools, which can be switched off one by one or by what they do.
//!
//! The page reads core's status afresh each time it is drawn rather than
//! keeping a copy, so it is never staler than the last redraw. Nothing
//! ticks: while a server is still starting, one wait off the UI thread for
//! the servers to settle asks for the redraw that shows how it went; while
//! one is retrying, that wait begins when its next start is due.
//! The log tail is shown here and nowhere else the model could read it.
//! A tool switched off is a permission rule, read into `off` by the page.
//!
//! A server reached by URL can be signed in to and out of here. While a
//! sign-in waits for the browser, the row says where it was sent and takes
//! the address the browser ended on, for one that cannot reach this
//! computer; the wait is a thread's, so nothing ticks meanwhile either.
//! Finding where to sign in can be cancelled too, and a sign-in the model
//! started shows in its row at the next redraw, for its paste and Cancel.

use crate::prelude::*;
use letronna_core::mcp::{Kind, ServerStatus, State};
use letronna_core::mcp_auth::{self, SignIn};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// How long a wait for starting servers lasts at most: the minute core gives
/// a server to start and list its tools.
const SETTLE_WAIT: std::time::Duration = std::time::Duration::from_secs(60);
/// How long after a retry is due the page waits for it: time for core's
/// timer to fire and the start to begin, so the wait does not end before it.
const RETRY_SLACK: std::time::Duration = std::time::Duration::from_millis(100);
/// Tallest a log gets before it scrolls: about fifteen lines.
const LOG_HEIGHT: Pixels = px(240.);

pub(crate) fn mcp_page(
    chat: &Entity<Chat>,
    off: &[String],
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let servers = letronna_core::mcp::status();
    let trouble = window.use_keyed_state("mcp-trouble", cx, |_, _| None::<String>);
    let waiting = window.use_keyed_state("mcp-waiting", cx, |_, _| false);
    let retry = servers.iter().filter_map(|s| s.retry).map(|r| r.wait).min();
    let busy = retry.is_some() || servers.iter().any(|s| s.state == State::Starting);
    if busy && !*waiting.read(cx) {
        settle(chat, &waiting, retry, cx);
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
                    "No servers yet. Add one above, with `letronna mcp add` in a terminal, \
                     or in {}.",
                    letronna_core::mcp::config_path().display()
                )),
        );
    }
    for server in &servers {
        list = list.child(server_row(
            chat, server, off, &trouble, &waiting, window, cx,
        ));
    }
    // After mcp.json is edited by hand: added servers start, removed ones
    // stop and changed ones start again, the rest untouched.
    let reload = {
        let (chat, trouble, waiting) = (chat.clone(), trouble.clone(), waiting.clone());
        Button::new("mcp-reload")
            .framed(cx)
            .xsmall()
            .label("Reload")
            .on_click(move |_, _, cx| {
                let done = letronna_core::mcp::reload().map(drop);
                acted(done, &chat, &trouble, &waiting, cx);
            })
    };
    v_flex()
        .gap_4()
        .child(
            h_flex()
                .items_start()
                .justify_between()
                .gap_3()
                .child(super::settings::heading(
                    "MCP servers",
                    "Servers listed in mcp.json. Their tools join Letronna's own; what they print stays here.",
                    cx,
                ).flex_1().min_w_0())
                .child(reload),
        )
        .children(problem.map(|problem| {
            div()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(problem)
        }))
        .child(list)
}

/// Wait off the UI thread for the servers still starting, after the soonest
/// retry is due if one is waiting, then redraw.
fn settle(
    chat: &Entity<Chat>,
    waiting: &Entity<bool>,
    retry: Option<std::time::Duration>,
    cx: &mut App,
) {
    waiting.update(cx, |waiting, _| *waiting = true);
    let executor = cx.background_executor().clone();
    let settled = cx.background_executor().spawn(async move {
        if let Some(retry) = retry {
            executor.timer(retry + RETRY_SLACK).await;
        }
        letronna_core::mcp::ready(SETTLE_WAIT)
    });
    let (chat, waiting) = (chat.clone(), waiting.clone());
    cx.spawn(async move |cx| {
        settled.await;
        waiting.update(cx, |waiting, _| *waiting = false);
        chat.update(cx, |_, cx| cx.notify());
    })
    .detach();
}

/// One server: its name and state, what it runs, why it failed, its log when
/// asked for, and what can be done with it.
fn server_row(
    chat: &Entity<Chat>,
    server: &ServerStatus,
    off: &[String],
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
    let tools_shown = window.use_keyed_state(key("tools-shown"), cx, |_, _| false);
    let listing = *tools_shown.read(cx) && server.state == State::Connected && server.tools > 0;
    let (state, colour) = match server.state {
        State::Connected => ("Connected", cx.theme().green),
        State::Starting => ("Starting", cx.theme().warning),
        State::Retrying => ("Retrying", cx.theme().warning),
        State::Failed => ("Failed", cx.theme().danger),
        State::Disabled => ("Off", cx.theme().muted_foreground),
        State::NeedsAuth => ("Needs sign-in", cx.theme().warning),
    };
    let what = match server.state {
        State::Connected => format!("{} · {} tools", server.transport, server.tools),
        // No countdown: nothing ticks to keep one true.
        State::Retrying => match server.retry {
            Some(retry) => format!(
                "{} · retry {} of {}",
                server.transport, retry.attempt, retry.attempts
            ),
            None => server.transport.to_owned(),
        },
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
    let tools_button = (server.state == State::Connected && server.tools > 0).then(|| {
        let (shown, redraw) = (tools_shown.clone(), redraw.clone());
        Button::new(key("tools"))
            .framed(cx)
            .xsmall()
            .label(if listing { "Hide tools" } else { "Tools" })
            .on_click(move |_, _, cx| {
                shown.update(cx, |shown, _| *shown = !*shown);
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
        super::capabilities::switch(key("switch"), !on, window, cx, move |_, _, cx| {
            let done = letronna_core::mcp::set_enabled(&name, on);
            acted(done, &chat, &trouble, &waiting, cx);
        })
    };
    let http = server.transport == "http";
    let signing = window.use_keyed_state(key("signing"), cx, |_, _| None::<Signing>);
    let ours = signing.read(cx).clone();
    // One the model started shows here too, for its Cancel and paste.
    let flow = match &ours {
        Some(ours) => ours.sign_in.clone(),
        None => http.then(|| mcp_auth::pending(&name)).flatten(),
    };
    let sign_in = (http && ours.is_none() && flow.is_none() && !on).then(|| {
        let (chat, trouble, waiting, signing, name) = (
            chat.clone(),
            trouble.clone(),
            waiting.clone(),
            signing.clone(),
            name.clone(),
        );
        Button::new(key("sign-in"))
            .framed(cx)
            .xsmall()
            .label(if server.signed_in {
                "Sign in again"
            } else {
                "Sign in"
            })
            .on_click(move |_, _, cx| begin(&name, &chat, &trouble, &waiting, &signing, cx))
    });
    let sign_out = (http && server.signed_in).then(|| {
        let (chat, trouble, waiting, name) =
            (chat.clone(), trouble.clone(), waiting.clone(), name.clone());
        Button::new(key("sign-out"))
            .framed(cx)
            .xsmall()
            .label("Sign out")
            .on_click(move |_, _, cx| {
                let done = mcp_auth::sign_out(&name);
                acted(done, &chat, &trouble, &waiting, cx);
            })
    });
    let stop = ours.as_ref().map(|ours| ours.stop.clone());
    let waiting_panel = match flow {
        Some(flow) => {
            let field = paste_field(&key, &signing, &name, chat, trouble, window, cx);
            Some(waiting_row(&flow, stop, &field, chat, trouble, &key, cx))
        }
        None => stop.map(|stop| finding_row(stop, &name, &signing, chat, &key, cx)),
    };
    let actions = h_flex()
        .flex_shrink_0()
        .items_center()
        .gap_1()
        .children(tools_button)
        .children(log_button)
        .children(sign_in)
        .children(sign_out)
        .children(reconnect)
        .child(div().ml_1().child(switch));
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
        .children(waiting_panel)
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
        .when(listing, |row| {
            row.child(tools_view(chat, &name, off, trouble, window, cx))
        })
}

/// A server's tools, grouped by what their server says they do, each group
/// and each tool with a switch, and a preset that leaves only reading on.
fn tools_view(
    chat: &Entity<Chat>,
    server: &str,
    off: &[String],
    trouble: &Entity<Option<String>>,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let tools = letronna_core::mcp::server_tools(server);
    let is_on = |name: &String| !off.contains(name);
    let tokens: usize = tools
        .iter()
        .filter(|tool| is_on(&tool.name))
        .map(|tool| tool.tokens)
        .sum();
    let names = |kinds: &[Kind]| -> Vec<String> {
        tools
            .iter()
            .filter(|tool| kinds.contains(&tool.kind))
            .map(|tool| tool.name.clone())
            .collect()
    };
    let (reads, changes) = (
        names(&[Kind::Read]),
        names(&[Kind::Write, Kind::Destructive]),
    );
    let read_only = reads.iter().all(is_on) && !changes.iter().any(is_on);
    let preset = {
        let (chat, trouble) = (chat.clone(), trouble.clone());
        Button::new(SharedString::from(format!("mcp-read-only-{server}")))
            .framed(cx)
            .xsmall()
            .label("Read only")
            .disabled(read_only || reads.is_empty())
            .on_click(move |_, _, cx| {
                // Off first: a read tool that `disabled_tools` holds off fails
                // the second switch, and the ones that change things must
                // be off by then.
                let done = letronna_core::toolset::switch(&changes, false)
                    .and_then(|()| letronna_core::toolset::switch(&reads, true));
                switched(done, &chat, &trouble, cx);
            })
    };
    let mut view = v_flex()
        .gap_2()
        .mt_1()
        .pl_3()
        .border_l_1()
        .border_color(cx.theme().border)
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "≈ {} tokens per request",
                            letronna_core::usage::count(tokens as u64)
                        )),
                )
                .child(preset),
        );
    for (kind, title) in [
        (Kind::Read, "Read"),
        (Kind::Write, "Write"),
        (Kind::Destructive, "Destructive"),
    ] {
        let group: Vec<_> = tools.iter().filter(|tool| tool.kind == kind).collect();
        if group.is_empty() {
            continue;
        }
        let names: Vec<String> = group.iter().map(|tool| tool.name.clone()).collect();
        let any_on = names.iter().any(is_on);
        let group_switch = {
            let (chat, trouble) = (chat.clone(), trouble.clone());
            let id = SharedString::from(format!("mcp-group-{server}-{title}"));
            super::capabilities::switch(id, any_on, window, cx, move |_, _, cx| {
                let done = letronna_core::toolset::switch(&names, !any_on);
                switched(done, &chat, &trouble, cx);
            })
        };
        view = view.child(
            h_flex()
                .items_center()
                .justify_between()
                .gap_3()
                .pt_2()
                .child(
                    div()
                        .text_sm()
                        .font_family(HEADING_FONT)
                        .font_weight(FontWeight::MEDIUM)
                        .child(format!("{title} · {}", group.len())),
                )
                .child(group_switch),
        );
        for tool in group {
            let on = is_on(&tool.name);
            let tool_switch = {
                let (chat, trouble) = (chat.clone(), trouble.clone());
                let names = vec![tool.name.clone()];
                let id = SharedString::from(format!("mcp-tool-{}", tool.name));
                super::capabilities::switch(id, on, window, cx, move |_, _, cx| {
                    let done = letronna_core::toolset::switch(&names, !on);
                    switched(done, &chat, &trouble, cx);
                })
            };
            view = view.child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_xs()
                                    .font_family(MONO_FONT)
                                    .when(!on, |this| this.text_color(cx.theme().muted_foreground))
                                    .child(tool.own.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .line_clamp(2)
                                    .text_color(cx.theme().muted_foreground)
                                    .child(tool.description.clone()),
                            ),
                    )
                    .child(tool_switch),
            );
        }
    }
    view
}

/// Show why a switch failed, if it did, and read the switches again.
fn switched(
    done: Result<(), String>,
    chat: &Entity<Chat>,
    trouble: &Entity<Option<String>>,
    cx: &mut App,
) {
    trouble.update(cx, |trouble, _| *trouble = done.err());
    chat.update(cx, |this, cx| {
        this.capabilities.off = letronna_core::tools::turned_off();
        cx.notify();
    });
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
        settle(chat, waiting, None, cx);
    }
    chat.update(cx, |_, cx| cx.notify());
}

/// A sign-in this page started for one server.
#[derive(Clone)]
struct Signing {
    /// Set by Cancel, so how it ends is not reported as a failure.
    stop: Arc<AtomicBool>,
    /// None while it is still finding where to sign in.
    sign_in: Option<Arc<SignIn>>,
}

/// Start signing in to the server `name`: find where off the UI thread, open
/// the page, then wait on a thread of its own for the browser or a paste,
/// and redraw as each step ends.
fn begin(
    name: &str,
    chat: &Entity<Chat>,
    trouble: &Entity<Option<String>>,
    waiting: &Entity<bool>,
    signing: &Entity<Option<Signing>>,
    cx: &mut App,
) {
    let stop = Arc::new(AtomicBool::new(false));
    signing.update(cx, |signing, _| {
        *signing = Some(Signing {
            stop: stop.clone(),
            sign_in: None,
        });
    });
    trouble.update(cx, |trouble, _| *trouble = None);
    chat.update(cx, |_, cx| cx.notify());
    let (found, begun) = async_channel::bounded(1);
    let (name, stopped) = (name.to_owned(), stop.clone());
    std::thread::spawn(move || {
        found.send_blocking(mcp_auth::begin(&name, &|| stopped.load(Ordering::Relaxed)))
    });
    let (chat, trouble, waiting, signing) = (
        chat.clone(),
        trouble.clone(),
        waiting.clone(),
        signing.clone(),
    );
    cx.spawn(async move |cx| {
        let begun = begun
            .recv()
            .await
            .unwrap_or_else(|_| Err(mcp_auth::CANCELLED.into()));
        let finished = match begun {
            Ok(sign_in) if !stop.load(Ordering::Relaxed) => {
                let url = sign_in.url.clone();
                cx.background_executor()
                    .spawn(async move { mcp_auth::open_browser(&url) })
                    .detach();
                let shown = Some(sign_in.clone());
                signing.update(cx, |signing, _| {
                    if let Some(ours) = signing
                        .as_mut()
                        .filter(|ours| Arc::ptr_eq(&ours.stop, &stop))
                    {
                        ours.sign_in = shown;
                    }
                });
                chat.update(cx, |_, cx| cx.notify());
                let (done, finished) = async_channel::bounded(1);
                std::thread::spawn(move || done.send_blocking(sign_in.finish()));
                finished
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err(mcp_auth::CANCELLED.into()))
            }
            // Cancelled just as it was found.
            Ok(sign_in) => {
                sign_in.cancel();
                Err(mcp_auth::CANCELLED.into())
            }
            Err(error) => Err(error),
        };
        cx.update(|cx| {
            // Only this sign-in is cleared: one started again since stays.
            signing.update(cx, |signing, _| {
                if signing
                    .as_ref()
                    .is_some_and(|ours| Arc::ptr_eq(&ours.stop, &stop))
                {
                    *signing = None;
                }
            });
            match finished {
                // Given up on by the user: what was asked for, not a failure.
                Err(_) if stop.load(Ordering::Relaxed) => {
                    trouble.update(cx, |trouble, _| *trouble = None);
                    chat.update(cx, |_, cx| cx.notify());
                }
                // A success starts the server again: wait for it to settle.
                done => acted(done, &chat, &trouble, &waiting, cx),
            }
        });
    })
    .detach();
}

/// A sign-in under way: where the browser was sent, and a field for the
/// address it ended on, for a browser that cannot reach this computer.
/// `stop` is this page's own sign-in's, None for one the model started.
fn waiting_row(
    flow: &Arc<SignIn>,
    stop: Option<Arc<AtomicBool>>,
    field: &Entity<InputState>,
    chat: &Entity<Chat>,
    trouble: &Entity<Option<String>>,
    key: &dyn Fn(&str) -> SharedString,
    cx: &mut App,
) -> Div {
    let paste = {
        let (flow, field, chat, trouble) =
            (flow.clone(), field.clone(), chat.clone(), trouble.clone());
        Button::new(key("paste-use"))
            .framed(cx)
            .xsmall()
            .label("Use address")
            .on_click(move |_, window, cx| submit(&flow, &field, &chat, &trouble, window, cx))
    };
    let copy = {
        let url = flow.url.clone();
        Button::new(key("copy"))
            .framed(cx)
            .xsmall()
            .label("Copy address")
            .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(url.clone())))
    };
    let cancel = {
        let (flow, chat) = (flow.clone(), chat.clone());
        Button::new(key("cancel"))
            .framed(cx)
            .xsmall()
            .label("Cancel")
            .on_click(move |_, _, cx| {
                if let Some(stop) = &stop {
                    stop.store(true, Ordering::Relaxed);
                }
                flow.cancel();
                chat.update(cx, |_, cx| cx.notify());
            })
    };
    v_flex()
        .gap_1p5()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Finish signing in in your browser. If it cannot reach this computer, paste the address it ended on."),
        )
        .child(
            div()
                .text_xs()
                .font_family(MONO_FONT)
                .text_color(cx.theme().muted_foreground)
                .truncate()
                .child(flow.url.clone()),
        )
        .child(
            h_flex()
                .gap_1()
                .child(Input::new(field).small().flex_1())
                .child(paste)
                .child(copy)
                .child(cancel),
        )
}

/// The field the address a browser ended on is pasted into: Enter uses it
/// as the button does, on whichever sign-in the row shows then.
fn paste_field(
    key: &dyn Fn(&str) -> SharedString,
    signing: &Entity<Option<Signing>>,
    name: &str,
    chat: &Entity<Chat>,
    trouble: &Entity<Option<String>>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputState> {
    let field = window.use_keyed_state(key("paste"), cx, |window, cx| {
        InputState::new(window, cx)
            .placeholder("Paste the address the browser ended on")
            .masked(true)
    });
    let (signing, name, chat, trouble) = (
        signing.clone(),
        name.to_owned(),
        chat.clone(),
        trouble.clone(),
    );
    // Kept while the field is drawn, and dropped with it.
    window.use_keyed_state(key("paste-enter"), cx, |window, cx| {
        window.subscribe(&field, cx, move |field, event: &InputEvent, window, cx| {
            if !matches!(event, InputEvent::PressEnter { .. }) {
                return;
            }
            let ours = signing
                .read(cx)
                .as_ref()
                .and_then(|ours| ours.sign_in.clone());
            if let Some(flow) = ours.or_else(|| mcp_auth::pending(&name)) {
                submit(&flow, &field, &chat, &trouble, window, cx);
            }
        })
    });
    field
}

/// Give `flow` the pasted address, emptying the field once it is taken.
fn submit(
    flow: &SignIn,
    field: &Entity<InputState>,
    chat: &Entity<Chat>,
    trouble: &Entity<Option<String>>,
    window: &mut Window,
    cx: &mut App,
) {
    let text = field.read(cx).value().trim().to_owned();
    let done = flow.paste(&text);
    if done.is_ok() {
        field.update(cx, |field, cx| field.set_value("", window, cx));
    }
    trouble.update(cx, |trouble, _| *trouble = done.err());
    chat.update(cx, |_, cx| cx.notify());
}

/// A sign-in still finding where to go, with a Cancel that gives it up.
fn finding_row(
    stop: Arc<AtomicBool>,
    name: &str,
    signing: &Entity<Option<Signing>>,
    chat: &Entity<Chat>,
    key: &dyn Fn(&str) -> SharedString,
    cx: &mut App,
) -> Div {
    let cancel = {
        let (signing, chat) = (signing.clone(), chat.clone());
        Button::new(key("cancel"))
            .framed(cx)
            .xsmall()
            .label("Cancel")
            .on_click(move |_, _, cx| {
                stop.store(true, Ordering::Relaxed);
                signing.update(cx, |signing, _| *signing = None);
                chat.update(cx, |_, cx| cx.notify());
            })
    };
    h_flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(format!("Finding where {name} signs in…")),
        )
        .child(cancel)
}
