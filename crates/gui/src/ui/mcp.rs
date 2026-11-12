//! Capabilities › MCP: the MCP servers, each one row saying how it is doing,
//! grouped by who has to do something about it: those that need the user
//! first, then those starting, those connected, and those switched off.
//!
//! A row has one line and at most one button, the one that puts it right:
//! Sign in for a server that wants one, Retry for one that failed. Pressed,
//! it shows the rest under it: what it runs and is given, its tools, which
//! can be switched off one by one or by what they do, what it printed, and
//! what else can be done with it.
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

use super::parts::{self, Mark, Row, Segment};
use crate::prelude::*;
use letronna_core::mcp::{Kind, ServerStatus, ServerTool, State};
use letronna_core::mcp_auth::{self, SignIn};
use std::rc::Rc;
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

/// Where a server is listed: by who has to do something about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Group {
    /// The user: to sign in, or to look at why it failed.
    NeedsYou,
    /// Nobody yet: it is starting, or being started again.
    Starting,
    Connected,
    Off,
}

/// The groups in the order the page lists them, each with its label. One
/// that holds no server is left out.
const GROUPS: [(Group, &str); 4] = [
    (Group::NeedsYou, "Needs you"),
    (Group::Starting, "Starting"),
    (Group::Connected, "Connected"),
    (Group::Off, "Off"),
];

fn group_of(state: State) -> Group {
    match state {
        State::NeedsAuth | State::Failed => Group::NeedsYou,
        State::Starting | State::Retrying => Group::Starting,
        State::Connected => Group::Connected,
        State::Disabled => Group::Off,
    }
}

fn mark_of(state: State) -> Mark {
    match state {
        State::Connected => Mark::Fine,
        State::NeedsAuth => Mark::NeedsYou,
        State::Failed => Mark::Failed,
        State::Starting | State::Retrying => Mark::Working,
        State::Disabled => Mark::Off,
    }
}

/// `count` of `thing`, said as it is read: "1 tool", "26 tools".
fn counted(count: usize, thing: &str) -> String {
    match count {
        1 => format!("1 {thing}"),
        count => format!("{count} {thing}s"),
    }
}

/// The line over the list: how many servers are connected, how many wait
/// for the user, and what the tools on offer add to every request, which is
/// the reason to switch any of them off.
fn summary(connected: usize, needing: usize, offered: usize, tokens: usize) -> String {
    let mut said = vec![format!("{connected} connected")];
    match needing {
        0 => {}
        1 => said.push("1 needs you".to_owned()),
        needing => said.push(format!("{needing} need you")),
    }
    if offered > 0 {
        said.push(format!(
            "{} offered · ≈ {} tokens on every request",
            counted(offered, "tool"),
            letronna_core::usage::count(tokens as u64)
        ));
    }
    said.join(" · ")
}

/// How many of `tools` are on, and about how many tokens those add to
/// every request.
fn offered(tools: &[ServerTool], off: &[String]) -> (usize, usize) {
    tools
        .iter()
        .filter(|tool| !off.contains(&tool.name))
        .fold((0, 0), |(on, tokens), tool| (on + 1, tokens + tool.tokens))
}

/// The line of what a server printed that most likely says why it stopped:
/// the last that names an error, else the last with words in it. A stack
/// trace's frames and the object printed after it come after the line that
/// matters and say nothing, so a frame is passed over, and so is a line
/// that is one field of something being dumped.
fn telling(log: &str) -> Option<&str> {
    let worded = || {
        log.lines().rev().map(said_alone).filter(|line| {
            !line.starts_with("at ")
                && !line.ends_with([',', '{', '}', '[', ']'])
                && line.chars().filter(|c| c.is_alphabetic()).count() >= 3
        })
    };
    worded()
        .find(|line| {
            let line = line.to_lowercase();
            [
                "error",
                "failed",
                "cannot",
                "not found",
                "refused",
                "denied",
            ]
            .iter()
            .any(|word| line.contains(word))
        })
        .or_else(|| worded().next())
}

/// A printed line without the process number some servers start each with,
/// `[3024]`, which says nothing of why.
fn said_alone(line: &str) -> &str {
    let mut line = line.trim();
    while let Some((number, rest)) = line.strip_prefix('[').and_then(|l| l.split_once(']'))
        && !number.is_empty()
        && number.chars().all(|c| c.is_ascii_digit())
    {
        line = rest.trim_start();
    }
    line
}

/// Why a server failed, for its row: core's words, and for one that only
/// "stopped", what it printed that says why.
fn why(error: Option<&str>, log: &str) -> Option<String> {
    let error = error?.trim();
    Some(match telling(log) {
        Some(printed) if error.ends_with("stopped.") => format!("{error} It printed: {printed}"),
        _ => error.to_owned(),
    })
}

/// How a server is doing, in the one line its row has. `on` of its tools
/// are offered, adding `tokens` to a request; `signing` while a sign-in to
/// it waits for the browser.
fn line(server: &ServerStatus, on: usize, tokens: usize, signing: bool) -> String {
    match server.state {
        State::Connected if server.tools == 0 => "Connected, with no tools".to_owned(),
        State::Connected => {
            let tools = match on == server.tools {
                true => counted(on, "tool"),
                false => format!("{on} of {} on", counted(server.tools, "tool")),
            };
            let tokens = letronna_core::usage::count(tokens as u64);
            format!("{tools} · ≈ {tokens} tokens per request")
        }
        State::Starting => "Starting…".to_owned(),
        // No countdown: nothing ticks to keep one true.
        State::Retrying => match server.retry {
            Some(retry) => format!(
                "Could not start. Trying again, {} of {}",
                retry.attempt, retry.attempts
            ),
            None => "Could not start. Trying again".to_owned(),
        },
        State::Failed => {
            why(server.error.as_deref(), &server.log).unwrap_or_else(|| "Failed".to_owned())
        }
        State::NeedsAuth if signing => "Waiting for your browser to finish the sign-in".to_owned(),
        State::NeedsAuth => "Needs a sign-in before it lists its tools".to_owned(),
        State::Disabled => "Off".to_owned(),
    }
}

pub(crate) fn mcp_page(
    chat: &Entity<Chat>,
    off: &[String],
    added: Option<String>,
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
    // Each connected server's tools, read once for its row and the summary.
    let tools: Vec<Vec<ServerTool>> = servers
        .iter()
        .map(|server| match server.state {
            State::Connected => letronna_core::mcp::server_tools(&server.name),
            _ => Vec::new(),
        })
        .collect();
    let (on, tokens) = tools
        .iter()
        .map(|tools| offered(tools, off))
        .fold((0, 0), |(on, tokens), (more, cost)| {
            (on + more, tokens + cost)
        });
    let connected = servers
        .iter()
        .filter(|server| server.state == State::Connected)
        .count();
    let needing = servers
        .iter()
        .filter(|server| group_of(server.state) == Group::NeedsYou)
        .count();
    let add = {
        let chat = chat.clone();
        Button::new("mcp-add")
            .primary()
            .small()
            .icon(Icon::new(IconName::Plus))
            .label("Add server")
            .on_click(move |_, window, cx| {
                chat.update(cx, |this, cx| this.open_server_adder(window, cx));
            })
    };
    // After mcp.json is edited by hand: added servers start, removed ones
    // stop and changed ones start again, the rest untouched.
    let reload = {
        let (chat, trouble, waiting) = (chat.clone(), trouble.clone(), waiting.clone());
        Button::new("mcp-reload")
            .framed(cx)
            .small()
            .icon(Icon::new(AppIcon::Reload))
            .tooltip("Read mcp.json again")
            .on_click(move |_, _, cx| {
                let done = letronna_core::mcp::reload().map(drop);
                acted(done, &chat, &trouble, &waiting, cx);
            })
    };
    let mut page = v_flex().gap_5().child(
        h_flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .when(!servers.is_empty(), |this| {
                        this.child(summary(connected, needing, on, tokens))
                    }),
            )
            .child(reload)
            .child(add),
    );
    page = page
        .children(
            problem.map(|problem| div().text_sm().text_color(cx.theme().danger).child(problem)),
        )
        .children(added.map(|added| {
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(added)
        }));
    if servers.is_empty() {
        let first = {
            let chat = chat.clone();
            Button::new("mcp-add-first")
                .primary()
                .small()
                .label("Add a server")
                .on_click(move |_, window, cx| {
                    chat.update(cx, |this, cx| this.open_server_adder(window, cx));
                })
        };
        return page.child(parts::empty(
            AppIcon::Plugs,
            "No servers yet",
            format!(
                "An MCP server gives Letronna more tools: a tracker, a database, a design \
                 file. Paste what its README gives, or use `letronna mcp add` in a terminal. \
                 They are kept in {}.",
                letronna_core::mcp::config_path().display()
            ),
            Some(first.into_any_element()),
            cx,
        ));
    }
    for (group, title) in GROUPS {
        let rows: Vec<AnyElement> = servers
            .iter()
            .zip(&tools)
            .filter(|(server, _)| group_of(server.state) == group)
            .map(|(server, tools)| {
                server_row(chat, server, tools, off, &trouble, &waiting, window, cx)
            })
            .collect();
        if rows.is_empty() {
            continue;
        }
        page = page.child(
            v_flex()
                .gap_2()
                .child(parts::group_label(title, rows.len(), cx))
                .child(parts::list(rows, cx)),
        );
    }
    page.child(
        div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child("What a server prints stays in its row, under Log, and is never shown to the model."),
    )
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

/// Which of what is under an opened row is shown.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shown {
    Tools,
    Log,
}

/// One server: its row, and under it, once pressed, what it runs, its tools
/// or its log, and what else can be done with it. A sign-in under way shows
/// under the row whether it is opened or not: it is what the row waits on.
#[allow(clippy::too_many_arguments)]
fn server_row(
    chat: &Entity<Chat>,
    server: &ServerStatus,
    tools: &[ServerTool],
    off: &[String],
    trouble: &Entity<Option<String>>,
    waiting: &Entity<bool>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let name = server.name.clone();
    let key = |what: &str| SharedString::from(format!("mcp-{what}-{name}"));
    let opened = window.use_keyed_state(key("opened"), cx, |_, _| false);
    let shown = window.use_keyed_state(key("shown"), cx, |_, _| Shown::Tools);
    let scroll = window
        .use_keyed_state(key("log-scroll"), cx, |_, _| ScrollHandle::new())
        .read(cx)
        .clone();
    let open = *opened.read(cx);
    let has_tools = server.state == State::Connected && !tools.is_empty();
    let has_log = !server.log.trim().is_empty();
    // The tools if it has any, else the log; and the other when asked for.
    let showing = match *shown.read(cx) {
        Shown::Tools if has_tools => Some(Shown::Tools),
        Shown::Log if has_log => Some(Shown::Log),
        _ if has_tools => Some(Shown::Tools),
        _ if has_log => Some(Shown::Log),
        _ => None,
    };
    let redraw = {
        let chat = chat.clone();
        move |cx: &mut App| chat.update(cx, |_, cx| cx.notify())
    };
    let disabled = server.state == State::Disabled;
    let switch = {
        let (chat, trouble, waiting, name) =
            (chat.clone(), trouble.clone(), waiting.clone(), name.clone());
        super::capabilities::switch(key("switch"), !disabled, window, cx, move |_, _, cx| {
            let done = letronna_core::mcp::set_enabled(&name, disabled);
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
    let idle = ours.is_none() && flow.is_none();
    let sign_in = |id: &str, primary: bool, label: &'static str, cx: &App| {
        let (chat, trouble, waiting, signing, name) = (
            chat.clone(),
            trouble.clone(),
            waiting.clone(),
            signing.clone(),
            name.clone(),
        );
        let button = match primary {
            true => Button::new(key(id)).primary().small(),
            false => Button::new(key(id)).framed(cx).xsmall(),
        };
        button
            .label(label)
            .on_click(move |_, _, cx| begin(&name, &chat, &trouble, &waiting, &signing, cx))
    };
    let reconnect = |id: &str, primary: bool, label: &'static str, cx: &App| {
        let (chat, trouble, waiting, name) =
            (chat.clone(), trouble.clone(), waiting.clone(), name.clone());
        let button = match primary {
            true => Button::new(key(id)).primary().small(),
            false => Button::new(key(id)).framed(cx).xsmall(),
        };
        button.label(label).on_click(move |_, _, cx| {
            let done = letronna_core::mcp::reconnect(&name);
            acted(done, &chat, &trouble, &waiting, cx);
        })
    };
    // The one button that puts the row right, for a row something is wrong
    // with: everything else is under it.
    let fix = match server.state {
        State::NeedsAuth if http && idle => Some(sign_in("sign-in", true, "Sign in", cx)),
        State::Failed => Some(reconnect("retry", true, "Retry", cx)),
        _ => None,
    };
    let stop = ours.as_ref().map(|ours| ours.stop.clone());
    let signing_now = !idle;
    let waiting_panel = match flow {
        Some(flow) => {
            let field = paste_field(&key, &signing, &name, chat, trouble, window, cx);
            Some(waiting_row(&flow, stop, &field, chat, trouble, &key, cx))
        }
        None => stop.map(|stop| finding_row(stop, &name, &signing, chat, &key, cx)),
    };
    let (on, tokens) = offered(tools, off);
    let colour = match server.state {
        State::Failed => Some(cx.theme().danger),
        State::NeedsAuth | State::Retrying => Some(cx.theme().warning),
        _ => None,
    };
    let details = open.then(|| {
        let muted = cx.theme().muted_foreground;
        let given = |label: &'static str, names: &[String], cx: &App| {
            (!names.is_empty()).then(|| {
                h_flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .child(div().mr_1().text_xs().text_color(muted).child(label))
                    .children(names.iter().map(|name| parts::chip(name.clone(), cx)))
            })
        };
        let pick = |what: Shown| -> parts::Press {
            let (shown, scroll, redraw) = (shown.clone(), scroll.clone(), redraw.clone());
            Rc::new(move |_, _, cx| {
                shown.update(cx, |shown, _| *shown = what);
                // Opened at its end, where the newest lines are.
                scroll.scroll_to_item(1);
                redraw(cx);
            })
        };
        let mut views = Vec::new();
        if has_tools {
            views.push(Segment {
                label: "Tools",
                count: Some(tools.len()),
                picked: showing == Some(Shown::Tools),
                pick: pick(Shown::Tools),
            });
        }
        if has_log {
            views.push(Segment {
                label: "Log",
                count: None,
                picked: showing == Some(Shown::Log),
                pick: pick(Shown::Log),
            });
        }
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
        // A server that wants a sign-in has the button on its row.
        let again = (http && idle && !disabled && server.state != State::NeedsAuth).then(|| {
            let label = match server.signed_in {
                true => "Sign in again",
                false => "Sign in",
            };
            sign_in("sign-in-again", false, label, cx)
        });
        // One that failed has Retry on its row, which is this.
        let restart = (!disabled && server.state != State::Failed)
            .then(|| reconnect("reconnect", false, "Reconnect", cx));
        v_flex()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .font_family(MONO_FONT)
                    .text_color(muted)
                    .child(server.target.clone()),
            )
            .children(given("Variables", &server.env, cx))
            .children(given("Headers", &server.headers, cx))
            .child(
                h_flex()
                    .items_center()
                    .gap_1()
                    .pt_1()
                    .when(!views.is_empty(), |this| {
                        this.child(parts::segments(&key("views"), views, cx))
                    })
                    .child(div().flex_1())
                    .children(again)
                    .children(sign_out)
                    .children(restart),
            )
            .when(showing == Some(Shown::Log), |this| {
                this.child(
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
            .when(showing == Some(Shown::Tools), |this| {
                this.child(tools_view(chat, &name, tools, off, trouble, window, cx))
            })
    });
    let under = (waiting_panel.is_some() || details.is_some())
        .then(|| v_flex().gap_3().children(waiting_panel).children(details));
    let toggle = {
        let (opened, redraw) = (opened.clone(), redraw.clone());
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            opened.update(cx, |opened, _| *opened = !*opened);
            redraw(cx);
        }
    };
    Row::new(key("row"), mark_of(server.state), name.clone())
        .tag(server.transport)
        .line(line(server, on, tokens, signing_now), colour)
        .fix(fix)
        .control(switch)
        .opens(open, toggle)
        .under(under)
        .into_any_element()
}

/// A server's tools, grouped by what their server says they do, each group
/// and each tool with a switch, and a preset that leaves only reading on.
fn tools_view(
    chat: &Entity<Chat>,
    server: &str,
    tools: &[ServerTool],
    off: &[String],
    trouble: &Entity<Option<String>>,
    window: &mut Window,
    cx: &mut App,
) -> Div {
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
    let mut view = v_flex().gap_2().child(
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

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{GROUPS, Group, group_of, line, mark_of, summary, telling, why};
    use crate::ui::parts::Mark;
    use letronna_core::mcp::{Retry, ServerStatus, State};

    fn server(state: State, tools: usize) -> ServerStatus {
        ServerStatus {
            name: "linear".into(),
            transport: "stdio",
            target: "npx -y mcp-remote https://mcp.linear.app/sse".into(),
            state,
            tools,
            error: None,
            env: Vec::new(),
            headers: Vec::new(),
            log: String::new(),
            retry: None,
            signed_in: false,
        }
    }

    #[test]
    fn servers_are_listed_by_who_has_to_do_something() {
        assert_eq!(group_of(State::NeedsAuth), Group::NeedsYou);
        assert_eq!(group_of(State::Failed), Group::NeedsYou);
        // Being started again is nobody's to fix yet.
        assert_eq!(group_of(State::Retrying), Group::Starting);
        assert_eq!(group_of(State::Starting), Group::Starting);
        assert_eq!(group_of(State::Connected), Group::Connected);
        assert_eq!(group_of(State::Disabled), Group::Off);
        // Those that need the user lead, and those switched off come last.
        let order: Vec<Group> = GROUPS.iter().map(|(group, _)| *group).collect();
        assert_eq!(order.first(), Some(&Group::NeedsYou));
        assert_eq!(order.last(), Some(&Group::Off));
    }

    #[test]
    fn each_state_has_a_mark_of_its_own_shape() {
        assert_eq!(mark_of(State::Connected), Mark::Fine);
        assert_eq!(mark_of(State::NeedsAuth), Mark::NeedsYou);
        assert_eq!(mark_of(State::Failed), Mark::Failed);
        assert_eq!(mark_of(State::Starting), Mark::Working);
        assert_eq!(mark_of(State::Retrying), Mark::Working);
        assert_eq!(mark_of(State::Disabled), Mark::Off);
    }

    #[test]
    fn the_summary_says_who_needs_the_user_and_what_the_tools_cost() {
        assert_eq!(
            summary(2, 2, 27, 4_152),
            "2 connected · 2 need you · 27 tools offered · ≈ 4.2k tokens on every request"
        );
        assert_eq!(
            summary(1, 1, 1, 52),
            "1 connected · 1 needs you · 1 tool offered · ≈ 52 tokens on every request"
        );
        // Nothing waiting and nothing offered are not worth a word.
        assert_eq!(summary(0, 0, 0, 0), "0 connected");
    }

    #[test]
    fn a_row_says_how_its_server_is_doing_in_one_line() {
        let connected = server(State::Connected, 26);
        assert_eq!(
            line(&connected, 26, 4_100, false),
            "26 tools · ≈ 4.1k tokens per request"
        );
        // With some switched off, how many are left on.
        assert_eq!(
            line(&connected, 20, 3_000, false),
            "20 of 26 tools on · ≈ 3.0k tokens per request"
        );
        assert_eq!(
            line(&server(State::Connected, 0), 0, 0, false),
            "Connected, with no tools"
        );
        let mut retrying = server(State::Retrying, 0);
        retrying.retry = Some(Retry {
            wait: std::time::Duration::from_secs(4),
            attempt: 2,
            attempts: 5,
        });
        assert_eq!(
            line(&retrying, 0, 0, false),
            "Could not start. Trying again, 2 of 5"
        );
        let asking = server(State::NeedsAuth, 0);
        assert_eq!(
            line(&asking, 0, 0, false),
            "Needs a sign-in before it lists its tools"
        );
        assert_eq!(
            line(&asking, 0, 0, true),
            "Waiting for your browser to finish the sign-in"
        );
        assert_eq!(line(&server(State::Disabled, 0), 0, 0, false), "Off");
    }

    /// What mcp-remote printed when Linear's address answered 404.
    const STOPPED: &str = "[3024] Using transport strategy: sse-only\n\
        [3024] Connection error: SseError: SSE error: Non-200 status code (404)\n    \
        at _eventSource.onerror (file:///home/me/.npm/_npx/1/node_modules/sse.js:12:3)\n    \
        at ClientRequest.onError (file:///home/me/.npm/_npx/1/node_modules/es.js:9:1) {\n  \
        code: 404,\n  \
        event: {\n    \
        type: 'error',\n    \
        message: 'Non-200 status code (404)',\n  \
        }\n\
        }\n";

    #[test]
    fn a_server_that_only_stopped_says_what_it_printed_about_why() {
        // Past the frames and the object dumped after them, to the line
        // that names the error, without the process number before it.
        assert_eq!(
            telling(STOPPED),
            Some("Connection error: SseError: SSE error: Non-200 status code (404)")
        );
        assert_eq!(
            why(Some("linear stopped."), STOPPED).as_deref(),
            Some(
                "linear stopped. It printed: Connection error: SseError: SSE error: \
                 Non-200 status code (404)"
            )
        );
        // With no line that names an error, the last it printed in words.
        assert_eq!(
            telling("listening\nshutting down\n}\n"),
            Some("shutting down")
        );
        assert_eq!(telling("\n}\n"), None);
        // Core's own reason stands as it is; the log is not put after it.
        assert_eq!(
            why(Some("Could not start `npx`: not found."), STOPPED).as_deref(),
            Some("Could not start `npx`: not found.")
        );
        assert_eq!(why(None, STOPPED), None);
        let mut failed = server(State::Failed, 0);
        failed.error = Some("linear stopped.".into());
        failed.log = STOPPED.into();
        assert!(line(&failed, 0, 0, false).ends_with("Non-200 status code (404)"));
    }
}
