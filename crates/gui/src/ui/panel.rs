//! The panel on the right of the chat: what goes on beside the
//! conversation, a tab each -- the browser and the session's changes now;
//! the helpers' activity, the files and the jobs join it later.
//!
//! It is one fixed panel, not a layout of docks: there are no zones to
//! drag tabs into and nothing to arrange or break. A tab shows only while
//! it has something to show, only the open tab is drawn -- the others keep
//! their state in their own entities -- and each session remembers its
//! tab. A wide window docks the panel beside the chat, where its edge
//! drags it wider or narrower; a narrow one floats it in the corner, where
//! it covers the least.

use crate::prelude::*;
use crate::ui::browser::Widen;
use gpui_component::tab::{Tab, TabBar};
use std::collections::HashMap;

actions!(panel, [TogglePanel, BrowserTab]);

/// A window at least this wide docks the panel beside the chat -- Letronna's
/// own window opens this wide -- and a narrower one floats it in a corner.
const DOCK_WIDTH: Pixels = px(1100.);
/// How much of the window the docked panel takes until its edge is dragged,
/// and its bounds: less, and a page's words are too small to follow; more,
/// and the chat's composer is squeezed.
const DOCK_SHARE: f32 = 0.34;
const DOCK_MIN: Pixels = px(340.);
const DOCK_MAX: Pixels = px(640.);
/// What the chat keeps beside the sidebar however wide the panel is
/// dragged: room enough for the composer. Only in the narrowest docking
/// window with the sidebar open does the panel's own minimum come first.
const CHAT_ROOM: Pixels = px(560.);
/// How wide the floating panel is.
const FLOAT_WIDTH: Pixels = px(320.);
/// How far the floating panel sits below the title bar.
const FLOAT_TOP: Pixels = px(48.);
/// What a floating panel leaves clear under it in the window, however
/// short: the title bar, its gap above and the composer at a few lines.
const FLOAT_CLEAR: Pixels = px(240.);

/// A tab of the panel. A new one is a variant here and its arms in the
/// matches below.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PanelTab {
    Browser,
    Changes,
}

impl PanelTab {
    /// Every tab, in the order the bar shows them.
    const ALL: [PanelTab; 2] = [PanelTab::Browser, PanelTab::Changes];

    fn label(self) -> &'static str {
        match self {
            PanelTab::Browser => "Browser",
            PanelTab::Changes => "Changes",
        }
    }
}

/// Whether the panel shows, how wide its edge was dragged, and the tab each
/// session had open.
#[derive(Default)]
pub(crate) struct Panel {
    /// Shown or hidden by its keys and the title bar's toggle.
    open: bool,
    /// The docked panel's width, once its edge has been dragged, and what
    /// it was before the browser's page was turned on its side.
    width: Option<Pixels>,
    upright_width: Option<Pixels>,
    /// The tab each session had open last, by session id.
    tabs: HashMap<u64, PanelTab>,
}

impl Panel {
    /// The tab a session shows: the one it had open while that has
    /// something to show, or else the first that has.
    fn tab(&self, session: u64, shown: &[PanelTab]) -> Option<PanelTab> {
        self.tabs
            .get(&session)
            .copied()
            .filter(|tab| shown.contains(tab))
            .or_else(|| shown.first().copied())
    }

    /// Drop the tab a deleted session had open.
    pub(crate) fn forget(&mut self, session: u64) {
        self.tabs.remove(&session);
    }

    /// Widen the docked panel as far as the chat allows, for a page laid
    /// out as a desktop's, or go back to its width before.
    fn widen(&mut self, wide: bool, window: Pixels) {
        match wide {
            true => {
                self.upright_width = self.width;
                self.width = Some(window - CHAT_ROOM);
            }
            false => self.width = self.upright_width.take(),
        }
    }
}

/// Whether a window this wide docks the panel beside the chat.
pub(crate) fn docks(window: Pixels) -> bool {
    window >= DOCK_WIDTH
}

/// How wide the docked panel is in a window this wide, beside a sidebar
/// this wide, with its edge dragged to a width or not; None when the
/// window floats it instead.
fn dock_width(window: Pixels, sidebar: Pixels, dragged: Option<Pixels>) -> Option<Pixels> {
    if !docks(window) {
        return None;
    }
    let most = (window - sidebar - CHAT_ROOM).max(DOCK_MIN);
    let (width, most) = match dragged {
        Some(width) => (width, most),
        None => (window * DOCK_SHARE, most.min(DOCK_MAX)),
    };
    Some(width.max(DOCK_MIN).min(most))
}

/// The tabs that show, with how many things in each wait on the user,
/// given what each has: None for nothing to show.
fn shown_tabs(state: impl Fn(PanelTab) -> Option<usize>) -> Vec<(PanelTab, usize)> {
    PanelTab::ALL
        .into_iter()
        .filter_map(|tab| state(tab).map(|waiting| (tab, waiting)))
        .collect()
}

/// A tab's title, with the count of what waits in it.
fn title(tab: PanelTab, waiting: usize) -> String {
    match waiting {
        0 => tab.label().to_owned(),
        n => format!("{} {n}", tab.label()),
    }
}

/// The docked panel's edge, being dragged.
struct Edge;

impl Chat {
    /// What a tab has for this draw: None when it has nothing to show, or
    /// how many things in it wait on the user.
    fn tab_state(&self, tab: PanelTab, docked: bool, cx: &App) -> Option<usize> {
        match tab {
            PanelTab::Browser => {
                let browser = self.browser.read(cx);
                browser.has_content(docked).then(|| browser.waiting())
            }
            PanelTab::Changes => self.has_changes().then_some(0),
        }
    }

    fn tab_body(&self, tab: PanelTab, cx: &mut Context<Self>) -> AnyElement {
        match tab {
            PanelTab::Browser => self.browser.clone().into_any_element(),
            PanelTab::Changes => self.changes_tab(cx),
        }
    }

    /// The tabs that show for this draw, with their counts, and the one the
    /// session has open among them.
    fn tabs_now(&self, docked: bool, cx: &App) -> (Vec<(PanelTab, usize)>, Option<PanelTab>) {
        let shown = shown_tabs(|tab| self.tab_state(tab, docked, cx));
        let tabs: Vec<PanelTab> = shown.iter().map(|&(tab, _)| tab).collect();
        let open = self.panel.tab(self.session_on_screen(), &tabs);
        (shown, open)
    }

    fn session_on_screen(&self) -> u64 {
        self.store.conversations[self.store.active].id
    }

    /// The panel as it is drawn now: the tabs that show, with their counts,
    /// and the open one; None while it is hidden or has nothing to show.
    fn panel_view(&self, docked: bool, cx: &App) -> Option<(Vec<(PanelTab, usize)>, PanelTab)> {
        if !self.panel.open || self.page != Page::Chat {
            return None;
        }
        let (shown, open) = self.tabs_now(docked, cx);
        Some((shown, open?))
    }

    /// How much of the window the docked panel takes from the chat: none
    /// while it floats or is not drawn.
    pub(crate) fn panel_width(&self, window: &Window, cx: &App) -> Pixels {
        match dock_width(
            window.viewport_size().width,
            self.sidebar_width(),
            self.panel.width,
        ) {
            Some(width) if self.panel_view(true, cx).is_some() => width,
            _ => px(0.),
        }
    }

    /// How much of the window the sidebar takes.
    fn sidebar_width(&self) -> Pixels {
        if self.sidebar_open {
            crate::ui::sidebar::SIDEBAR_WIDTH
        } else {
            px(0.)
        }
    }

    /// Show the panel on the session's tab, or hide it. With nothing else
    /// to show, it opens on the browser, ready for an address.
    pub(crate) fn toggle_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let docked = docks(window.viewport_size().width);
        if self.panel_view(docked, cx).is_some() {
            return self.hide_panel(window, cx);
        }
        let (_, tab) = self.tabs_now(docked, cx);
        self.show_tab(tab.unwrap_or(PanelTab::Browser), window, cx);
    }

    /// Show the panel on a tab, and remember it as the session's.
    pub(crate) fn show_tab(&mut self, tab: PanelTab, window: &mut Window, cx: &mut Context<Self>) {
        self.page = Page::Chat;
        self.panel.open = true;
        self.panel.tabs.insert(self.session_on_screen(), tab);
        // An open browser is left as it is: asking again would reload it
        // and take the keys from wherever they were.
        if tab == PanelTab::Browser && !self.browser.read(cx).is_open() {
            self.browser.update(cx, |view, cx| view.ask(window, cx));
        }
        cx.notify();
    }

    /// Hide the panel. The browser stops sending pictures, and stays closed
    /// for the rest of this turn.
    fn hide_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.panel.open = false;
        self.browser.update(cx, |view, cx| view.close(cx));
        self.focus_composer(window, cx);
        cx.notify();
    }

    /// Show the browser when a call is about to use it: before it runs, or
    /// while it waits to be allowed, so the page can be seen first -- unless
    /// the agent is to browse unseen, or the panel was hidden this turn.
    pub(crate) fn show_browser(&mut self, tool: &str, cx: &mut Context<Self>) {
        if !matches!(tool, "browse" | "use_page") || self.store.settings.browse_unseen {
            return;
        }
        let was_open = self.browser.read(cx).is_open();
        self.browser.update(cx, |view, cx| view.open(cx));
        if !was_open && self.browser.read(cx).is_open() {
            self.panel.open = true;
            let session = self.streaming_id.unwrap_or(self.session_on_screen());
            self.panel.tabs.insert(session, PanelTab::Browser);
        }
    }

    /// The panel, docked beside the chat or floating over it, with a tab
    /// bar over the open tab, which alone is drawn.
    pub(crate) fn right_panel(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let width = dock_width(
            window.viewport_size().width,
            self.sidebar_width(),
            self.panel.width,
        );
        let (shown, open) = self.panel_view(width.is_some(), cx)?;
        let selected = shown.iter().position(|&(tab, _)| tab == open);
        let tabs = shown.into_iter().map(|(tab, waiting)| {
            Tab::new()
                .label(title(tab, waiting))
                .on_click(cx.listener(move |this, _, window, cx| this.show_tab(tab, window, cx)))
        });
        let bar = TabBar::new("panel-tabs")
            .underline()
            .small()
            .pl_3()
            .pr_2()
            .when_some(selected, |bar, index| bar.selected_index(index))
            .children(tabs)
            .suffix(
                Button::new("hide-panel")
                    .plain(cx)
                    .xsmall()
                    .icon(Icon::new(IconName::Close))
                    .tooltip_with_action("Hide the panel", &TogglePanel, Some("Chat"))
                    .on_click(cx.listener(|this, _, window, cx| this.hide_panel(window, cx))),
            );
        let panel = v_flex()
            .bg(cx.theme().background)
            .border_color(cx.theme().border)
            .child(bar)
            .child(self.tab_body(open, cx));
        Some(match width {
            Some(width) => panel
                .relative()
                .h_full()
                .w(width)
                .flex_shrink_0()
                .border_l_1()
                .on_drag_move(
                    cx.listener(|this, event: &DragMoveEvent<Edge>, window, cx| {
                        let right = window.viewport_size().width;
                        this.panel.width = Some(right - event.event.position.x);
                        cx.notify();
                    }),
                )
                .child(edge(cx))
                .into_any_element(),
            None => panel
                .absolute()
                // Top right, clear of the composer and the newest messages.
                .top(FLOAT_TOP)
                .right(px(16.))
                .w(FLOAT_WIDTH)
                .max_h(window.viewport_size().height - FLOAT_CLEAR)
                .rounded(cx.theme().radius_lg)
                .border_1()
                .overflow_hidden()
                .occlude()
                .into_any_element(),
        })
    }

    /// Widen the panel when the browser's page is turned on its side, and
    /// narrow it back.
    pub(crate) fn follow_browser_layout(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe_in(&self.browser, window, |this, _, Widen(wide), window, cx| {
            this.panel.widen(*wide, window.viewport_size().width);
            cx.notify();
        })
    }
}

/// The docked panel's left edge, which drags it wider or narrower, and
/// back to its own width when clicked twice.
fn edge(cx: &mut Context<Chat>) -> impl IntoElement {
    div()
        .id("panel-edge")
        .absolute()
        .top_0()
        .bottom_0()
        .left(px(-3.))
        .w(px(6.))
        .cursor(CursorStyle::ResizeLeftRight)
        .on_drag(Edge, |_, _, _, cx| cx.new(|_| Empty))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, event: &MouseDownEvent, _, cx| {
                if event.click_count == 2 {
                    this.panel.width = None;
                    cx.notify();
                }
            }),
        )
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{Panel, PanelTab, dock_width, shown_tabs, title};
    use gpui::px;

    #[test]
    fn a_tab_shows_only_with_something_to_show_and_counts_what_waits() {
        assert!(shown_tabs(|_| None).is_empty());
        assert_eq!(
            shown_tabs(|tab| (tab == PanelTab::Browser).then_some(1)),
            vec![(PanelTab::Browser, 1)]
        );
        assert_eq!(
            shown_tabs(|_| Some(0)),
            vec![(PanelTab::Browser, 0), (PanelTab::Changes, 0)]
        );
        assert_eq!(title(PanelTab::Browser, 0), "Browser");
        assert_eq!(title(PanelTab::Browser, 1), "Browser 1");
    }

    #[test]
    fn a_wide_window_docks_the_panel_at_a_third_within_its_bounds() {
        let no_sidebar = px(0.);
        assert_eq!(dock_width(px(1099.), no_sidebar, None), None);
        let third = dock_width(px(1500.), no_sidebar, None).map(f32::from);
        assert!(third.is_some_and(|width| (width - 510.).abs() < 0.01));
        // Never under 340 nor, until dragged, over 640.
        assert_eq!(
            dock_width(px(1100.), no_sidebar, Some(px(100.))),
            Some(px(340.))
        );
        assert_eq!(dock_width(px(2400.), no_sidebar, None), Some(px(640.)));
        // Dragged, it may pass 640 but always leaves the chat its room.
        assert_eq!(
            dock_width(px(1600.), no_sidebar, Some(px(800.))),
            Some(px(800.))
        );
        assert_eq!(
            dock_width(px(1200.), no_sidebar, Some(px(900.))),
            Some(px(640.))
        );
    }

    #[test]
    fn the_chat_keeps_its_room_beside_the_sidebar() {
        let sidebar = px(250.);
        // Dragged as wide as it goes, the panel stops where the chat has
        // 560 left beside the sidebar.
        assert_eq!(
            dock_width(px(1600.), sidebar, Some(px(1200.))),
            Some(px(790.))
        );
        // A page on its side asks for the room the whole window allows.
        assert_eq!(
            dock_width(px(1600.), sidebar, Some(px(1600. - 560.))),
            Some(px(790.))
        );
        // In the narrowest docking window the panel's minimum comes first.
        assert_eq!(dock_width(px(1100.), sidebar, None), Some(px(340.)));
        assert_eq!(
            dock_width(px(1100.), sidebar, Some(px(900.))),
            Some(px(340.))
        );
    }

    #[test]
    fn each_session_keeps_its_tab_while_that_has_something_to_show() {
        let mut panel = Panel::default();
        panel.tabs.insert(1, PanelTab::Browser);
        assert_eq!(panel.tab(1, &[PanelTab::Browser]), Some(PanelTab::Browser));
        // A session that chose none opens on the first tab that shows.
        assert_eq!(panel.tab(2, &[PanelTab::Browser]), Some(PanelTab::Browser));
        assert_eq!(panel.tab(1, &[]), None);
    }

    #[test]
    fn a_page_on_its_side_widens_the_panel_and_upright_again_narrows_it_back() {
        let mut panel = Panel {
            width: Some(px(400.)),
            ..Panel::default()
        };
        panel.widen(true, px(1500.));
        assert_eq!(panel.width, Some(px(940.)));
        panel.widen(false, px(1500.));
        assert_eq!(panel.width, Some(px(400.)));
    }
}
