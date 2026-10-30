//! The panel on the right of the chat: what goes on beside the
//! conversation, a tab each -- the browser, the session's changes, the
//! project's files and the background jobs now; the helpers' activity joins
//! them later.
//!
//! It is one fixed panel, not a layout of docks: there are no zones to
//! drag tabs into and nothing to arrange or break. A tab shows only while
//! it has something to show, only the open tab is drawn -- the others keep
//! their state in their own entities -- and each session remembers its
//! tab. The browser takes half the room beside the sidebar, or the share
//! its edge is dragged to, or all of it but the chat's when made wide; in
//! a room too narrow for both it is drawn over the conversation instead,
//! the composer still under it. The other tabs dock beside the chat in a
//! wide window, where the edge drags them wider or narrower, and float in a
//! narrow one's corner, where they cover the least.
//!
//! While the agent browses, the browser first shows as a preview: a small
//! picture of the page at the chat's corner, above the composer, which
//! takes none of the conversation's room. A click or Enter shows it beside
//! the chat, and the bar there shrinks it back.

use crate::prelude::*;
use crate::ui::browser::{CheckUp, Drawn, PointComment, ToPreview};
use gpui_component::tab::{Tab, TabBar};
use std::collections::{HashMap, HashSet};

actions!(
    panel,
    [
        TogglePanel,
        BrowserTab,
        WideBrowser,
        PreviewBrowser,
        ExpandBrowserPreview,
        CloseBrowserPreview
    ]
);

/// A window at least this wide docks the panel beside the chat -- Letronna's
/// own window opens this wide -- and a narrower one floats it in a corner.
/// The browser goes by the room beside the sidebar instead.
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
/// How much of the room beside the sidebar the browser takes until its
/// edge is dragged: half, the page and the conversation alike.
const BROWSER_SHARE: f32 = 0.5;
/// The least the browser is given beside the chat: about a phone's width,
/// which a page still has a layout for.
const BROWSER_LEAST: Pixels = px(400.);
/// The least the chat keeps beside the browser, however wide that is made:
/// a line of the conversation, and the composer with its controls under it.
const CHAT_LEAST: Pixels = px(420.);
/// How wide the browser's preview is: the page's shape still reads, and the
/// conversation keeps most of the pane. Narrower in a chat pane under
/// PREVIEW_NARROW, so the pane keeps over half.
const PREVIEW_WIDTH: Pixels = px(300.);
const PREVIEW_SMALL: Pixels = px(220.);
const PREVIEW_NARROW: Pixels = px(640.);
/// How far the preview sits from the chat pane's right edge, and above the
/// composer.
const PREVIEW_INSET: Pixels = px(12.);
/// The preview's header, with the page's name, and its footer, with what
/// the agent is doing there: a line of small text each.
const PREVIEW_HEADER: Pixels = px(26.);
const PREVIEW_FOOTER: Pixels = px(22.);

/// A tab of the panel. A new one is a variant here and its arms in the
/// matches below.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PanelTab {
    Browser,
    Changes,
    Files,
    Jobs,
    /// A plugin's panel, by the plugin's name, which its code draws.
    Plugin(&'static str),
}

impl PanelTab {
    /// The window's own tabs, in the order the bar shows them.
    const OWN: [PanelTab; 4] = [
        PanelTab::Browser,
        PanelTab::Changes,
        PanelTab::Files,
        PanelTab::Jobs,
    ];

    /// Every tab, the plugins' after the window's own.
    fn all() -> Vec<PanelTab> {
        let plugins = letronna_core::plugins::code::panels();
        (Self::OWN.into_iter())
            .chain(
                plugins
                    .into_iter()
                    .map(|(plugin, _)| PanelTab::Plugin(plugin)),
            )
            .collect()
    }

    fn label(self) -> String {
        match self {
            PanelTab::Browser => "Browser".into(),
            PanelTab::Changes => "Changes".into(),
            PanelTab::Files => "Files".into(),
            PanelTab::Jobs => "Jobs".into(),
            PanelTab::Plugin(plugin) => (letronna_core::plugins::code::panels().into_iter())
                .find(|(name, _)| *name == plugin)
                .map_or_else(|| plugin.to_owned(), |(_, title)| title),
        }
    }
}

/// Whether the panel shows, how wide its edge was dragged, and the tab each
/// session had open.
#[derive(Default)]
pub(crate) struct Panel {
    /// Shown or hidden by its keys and the title bar's toggle.
    open: bool,
    /// The docked panel's width, once its edge has been dragged.
    width: Option<Pixels>,
    /// The browser's share of the room beside the sidebar, once its edge
    /// has been dragged: a share, so it suits the window at any size.
    share: Option<f32>,
    /// The tab each session had open last, by session id.
    tabs: HashMap<u64, PanelTab>,
    /// The sessions whose browser shows as the preview at the chat's corner
    /// rather than beside the chat, by session id.
    previews: HashSet<u64>,
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

    /// Drop the tab a deleted session had open, and its preview.
    pub(crate) fn forget(&mut self, session: u64) {
        self.tabs.remove(&session);
        self.previews.remove(&session);
    }

    /// Drop every session's preview, for a profile whose sessions are others.
    pub(crate) fn forget_previews(&mut self) {
        self.previews.clear();
    }
}

/// Where the panel is drawn, for the tab open in it.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Place {
    /// Beside the chat, this wide.
    Docked(Pixels),
    /// In a corner, over the chat.
    Floating,
    /// In place of the conversation, above the composer.
    Over,
}

/// Whether a window this wide docks the panel beside the chat.
pub(crate) fn docks(window: Pixels) -> bool {
    window >= DOCK_WIDTH
}

/// Whether a room this wide, the window's beside the sidebar, is too narrow
/// for the browser and the chat side by side.
fn too_narrow(room: Pixels) -> bool {
    room < BROWSER_LEAST + CHAT_LEAST
}

/// How wide the browser is beside the chat in a room this wide: at its
/// share of it, half until its edge is dragged, or with all of it but the
/// chat's when wide; None when the room is too narrow for both.
fn browser_width(room: Pixels, share: Option<f32>, wide: bool) -> Option<Pixels> {
    if too_narrow(room) {
        return None;
    }
    let most = room - CHAT_LEAST;
    Some(match wide {
        true => most,
        false => (room * share.unwrap_or(BROWSER_SHARE))
            .max(BROWSER_LEAST)
            .min(most),
    })
}

/// The share of a room this wide that the browser's edge dragged to this
/// width gives it, as far as the browser and the chat each can go.
fn share_of(width: Pixels, room: Pixels) -> f32 {
    width.max(BROWSER_LEAST).min(room - CHAT_LEAST) / room
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

/// How wide the browser's preview is over a chat pane this wide.
fn preview_width(pane: Pixels) -> Pixels {
    if pane < PREVIEW_NARROW {
        PREVIEW_SMALL
    } else {
        PREVIEW_WIDTH
    }
}

/// The tabs that show, with how many things in each wait on the user,
/// given what each has: None for nothing to show.
fn shown_tabs(
    tabs: Vec<PanelTab>,
    state: impl Fn(PanelTab) -> Option<usize>,
) -> Vec<(PanelTab, usize)> {
    tabs.into_iter()
        .filter_map(|tab| state(tab).map(|waiting| (tab, waiting)))
        .collect()
}

/// A tab's title, with the count of what waits in it.
fn title(tab: PanelTab, waiting: usize) -> String {
    match waiting {
        0 => tab.label(),
        n => format!("{} {n}", tab.label()),
    }
}

/// The docked panel's edge, being dragged.
struct Edge;

impl Chat {
    /// What a tab has for this draw: None when it has nothing to show, or
    /// how many things in it wait on the user.
    fn tab_state(&self, tab: PanelTab, cx: &App) -> Option<usize> {
        match tab {
            PanelTab::Browser => {
                let browser = self.browser.read(cx);
                browser.is_open().then(|| browser.waiting())
            }
            // Or while it is the tab asked for, so the status bar's chip
            // opens it even with nothing to list.
            PanelTab::Changes => {
                let chosen = self.panel.tabs.get(&self.session_on_screen()) == Some(&tab);
                (chosen || self.has_changes()).then_some(0)
            }
            PanelTab::Files => self.has_files().then_some(0),
            // Once there is a job, or while it is the tab asked for, which
            // is where a first job of your own is started. Nothing in it
            // waits on you.
            PanelTab::Jobs => {
                let chosen = self.panel.tabs.get(&self.session_on_screen()) == Some(&tab);
                (chosen || !letronna_core::jobs::list().is_empty()).then_some(0)
            }
            // Once it has drawn, or while it is the tab asked for, which is
            // how it is first started: its chip, the palette or its page.
            PanelTab::Plugin(plugin) => {
                let chosen = self.panel.tabs.get(&self.session_on_screen()) == Some(&tab);
                (chosen || self.plugin_drew(plugin)).then_some(0)
            }
        }
    }

    fn tab_body(&self, tab: PanelTab, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match tab {
            PanelTab::Browser => self.browser.clone().into_any_element(),
            PanelTab::Changes => self.changes_tab(cx),
            PanelTab::Files => self.files_tab(window, cx),
            PanelTab::Jobs => self.jobs_tab(window, cx),
            PanelTab::Plugin(plugin) => self.plugin_panel(plugin, window, cx),
        }
    }

    /// The tabs that show for this draw, with their counts, and the one the
    /// session has open among them.
    fn tabs_now(&self, cx: &App) -> (Vec<(PanelTab, usize)>, Option<PanelTab>) {
        let shown = shown_tabs(PanelTab::all(), |tab| self.tab_state(tab, cx));
        let tabs: Vec<PanelTab> = shown.iter().map(|&(tab, _)| tab).collect();
        let open = self.panel.tab(self.session_on_screen(), &tabs);
        (shown, open)
    }

    pub(crate) fn session_on_screen(&self) -> u64 {
        self.store.conversations[self.store.active].id
    }

    /// The panel as it is drawn now: the tabs that show, with their counts,
    /// and the open one; None while it is hidden or has nothing to show.
    fn panel_view(&self, cx: &App) -> Option<(Vec<(PanelTab, usize)>, PanelTab)> {
        if !self.panel.open || self.page != Page::Chat {
            return None;
        }
        let (shown, open) = self.tabs_now(cx);
        Some((shown, open?))
    }

    /// The tab drawn now, if the panel is.
    pub(crate) fn tab_on_screen(&self, _window: &Window, cx: &App) -> Option<PanelTab> {
        self.panel_view(cx).map(|(_, open)| open)
    }

    /// Where the panel goes with this tab open, in the window as it is now.
    fn place_of(&self, open: PanelTab, window: &Window, cx: &App) -> Place {
        let (width, sidebar) = (window.viewport_size().width, self.sidebar_width());
        match open {
            PanelTab::Browser => {
                let wide = self.browser.read(cx).wide;
                browser_width(width - sidebar, self.panel.share, wide)
                    .map_or(Place::Over, Place::Docked)
            }
            _ => {
                dock_width(width, sidebar, self.panel.width).map_or(Place::Floating, Place::Docked)
            }
        }
    }

    /// How much of the window the docked panel takes from the chat: none
    /// while it floats, is drawn over the conversation, or is not drawn.
    pub(crate) fn panel_width(&self, window: &Window, cx: &App) -> Pixels {
        match self.panel_view(cx) {
            Some((_, open)) => match self.place_of(open, window, cx) {
                Place::Docked(width) => width,
                _ => px(0.),
            },
            None => px(0.),
        }
    }

    /// Whether the panel is drawn in place of the conversation: the browser
    /// in a room too narrow for it and the chat side by side.
    pub(crate) fn panel_covers(&self, window: &Window, cx: &App) -> bool {
        self.panel_view(cx)
            .is_some_and(|(_, open)| self.place_of(open, window, cx) == Place::Over)
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
        if self.panel_view(cx).is_some() {
            return self.hide_panel(window, cx);
        }
        let (_, tab) = self.tabs_now(cx);
        self.show_tab(tab.unwrap_or(PanelTab::Browser), window, cx);
    }

    /// Show the panel on a tab, and remember it as the session's.
    pub(crate) fn show_tab(&mut self, tab: PanelTab, window: &mut Window, cx: &mut Context<Self>) {
        self.page = Page::Chat;
        self.panel.open = true;
        self.panel.tabs.insert(self.session_on_screen(), tab);
        // Beside the chat now, not at its corner.
        if tab == PanelTab::Browser {
            self.panel.previews.remove(&self.session_on_screen());
        }
        // An open browser is left as it is: asking again would reload it
        // and take the keys from wherever they were.
        if tab == PanelTab::Browser && !self.browser.read(cx).is_open() {
            self.browser.update(cx, |view, cx| view.ask(window, cx));
        }
        if tab == PanelTab::Files {
            self.focus_files(window, cx);
        }
        if tab == PanelTab::Jobs {
            self.focus_jobs(window, cx);
        }
        // It starts if need be, and draws on hearing it was opened.
        if let PanelTab::Plugin(plugin) = tab {
            letronna_core::plugins::code::panel_opened(plugin);
        }
        cx.notify();
    }

    /// Hide the panel. The browser stops sending pictures, and stays closed
    /// for the rest of this turn -- unless it shows as the preview, which
    /// stays.
    fn hide_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.panel.open = false;
        if !self.panel.previews.contains(&self.session_on_screen()) {
            self.browser.update(cx, |view, cx| view.close(cx));
        }
        self.focus_composer(window, cx);
        cx.notify();
    }

    /// Show the browser when a call is about to use it, before it runs or
    /// while it waits to be allowed: as the preview at the chat's corner,
    /// which takes none of the conversation's room, or beside the chat where
    /// it is already -- unless the agent is to browse unseen, or the browser
    /// was closed this turn.
    pub(crate) fn show_browser(&mut self, tool: &str, cx: &mut Context<Self>) {
        if !matches!(tool, "browse" | "use_page") || self.store.settings.browse_unseen {
            return;
        }
        self.browser.update(cx, |view, cx| view.open(cx));
        if !self.browser.read(cx).is_open() {
            return;
        }
        let session = self.streaming_id.unwrap_or(self.session_on_screen());
        let beside = self.panel.open && self.panel.tabs.get(&session) == Some(&PanelTab::Browser);
        if !beside {
            self.panel.previews.insert(session);
        }
    }

    /// Whether the browser shows as the preview now: the session on screen
    /// has it so, its conversation is on screen, the browser is not beside
    /// it already, no tab floats over the corner it would be at, and no
    /// card waits on the user there, which the preview could cover.
    pub(crate) fn browser_preview_shown(&self, window: &Window, cx: &App) -> bool {
        let session = self.session_on_screen();
        self.page == Page::Chat
            && !self.settings_open
            && self.panel.previews.contains(&session)
            && self.browser.read(cx).is_open()
            && self.panel_view(cx).is_none_or(|(_, open)| {
                open != PanelTab::Browser && self.place_of(open, window, cx) != Place::Floating
            })
            && self.transcript_shown()
            && !self.needs_you(session)
    }

    /// How much the end of the conversation keeps clear, footer and all, so
    /// its newest lines show above the preview: none without one, or where
    /// the column's text stops short of it.
    pub(crate) fn browser_preview_room(&self, column: Pixels, window: &Window, cx: &App) -> Pixels {
        if !self.browser_preview_shown(window, cx) {
            return px(0.);
        }
        let pane =
            window.viewport_size().width - self.sidebar_width() - self.panel_width(window, cx);
        let width = preview_width(pane);
        // The column is centred in the pane, its text inside a 24px margin.
        let text_right = (pane + column) / 2. - px(24.);
        if text_right <= pane - PREVIEW_INSET - width {
            return px(0.);
        }
        let picture = self.browser.read(cx).thumbnail_height(width - px(2.));
        PREVIEW_HEADER + picture + PREVIEW_FOOTER + px(2.)
    }

    /// Show the browser of the session on screen as the preview at the
    /// chat's corner, unless it is beside the chat already.
    pub(crate) fn open_browser_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_browser_preview_at(None, window, cx);
    }

    /// As `open_browser_preview`, opening `home` if the session's page is
    /// gone with its browser: the address a browse row was on.
    pub(crate) fn open_browser_preview_at(
        &mut self,
        home: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tab_on_screen(window, cx) == Some(PanelTab::Browser) {
            return;
        }
        self.page = Page::Chat;
        // A tab floating over the corner the preview is drawn at makes way.
        if let Some(tab) = self.tab_on_screen(window, cx)
            && self.place_of(tab, window, cx) == Place::Floating
        {
            self.panel.open = false;
        }
        self.panel.previews.insert(self.session_on_screen());
        // Even when open: one not drawn just now, as after a switch of
        // session, may find this session's page gone, and opens `home`.
        self.browser.update(cx, |view, cx| view.reopen(home, cx));
        cx.notify();
    }

    /// Show the browser beside the chat in place of its preview, the keys
    /// the preview had going to the composer.
    pub(crate) fn expand_browser_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let had_keys = self.browser.read(cx).preview_focus.is_focused(window);
        self.show_tab(PanelTab::Browser, window, cx);
        if had_keys {
            self.focus_composer(window, cx);
        }
    }

    /// Close the preview: as when the panel is hidden, the browser stops
    /// sending pictures and stays closed for the rest of this turn.
    pub(crate) fn close_browser_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let had_keys = self.browser.read(cx).preview_focus.is_focused(window);
        self.panel.previews.remove(&self.session_on_screen());
        self.browser.update(cx, |view, cx| view.close(cx));
        if had_keys {
            self.focus_composer(window, cx);
        }
        cx.notify();
    }

    /// Shrink the browser beside the chat to its preview, or show the
    /// preview, or a browser not shown at all, beside the chat.
    pub(crate) fn toggle_browser_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab_on_screen(window, cx) != Some(PanelTab::Browser) {
            return self.expand_browser_preview(window, cx);
        }
        // The panel goes with it; keys it had go to the composer.
        let had_keys = self.panel_focus.contains_focused(window, cx);
        self.panel.open = false;
        self.open_browser_preview(window, cx);
        if had_keys {
            self.focus_composer(window, cx);
        }
    }

    /// The browser's preview at the chat's corner, while it shows: the page
    /// as a small picture under its name, which a click or Enter shows
    /// beside the chat, and what the agent does there while it does.
    pub(crate) fn browser_preview(
        &self,
        pane: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.browser_preview_shown(window, cx) {
            return None;
        }
        let width = preview_width(pane);
        // Inside its 1px edge.
        let picture = self
            .browser
            .update(cx, |view, cx| view.thumbnail(width - px(2.), cx));
        let browser = self.browser.read(cx);
        let name = browser.page_name();
        let streaming_here = self.busy && self.streaming_id == Some(self.session_on_screen());
        let doing = browser.doing().filter(|_| streaming_here);
        let focus = browser.preview_focus.clone();
        let focused = focus.is_focused(window);
        let theme = cx.theme();
        let header = h_flex()
            .h(PREVIEW_HEADER)
            .flex_shrink_0()
            .pl_2()
            .pr_1()
            .gap_1p5()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .text_xs()
            .child(
                Icon::new(IconName::Globe)
                    .size_3p5()
                    .text_color(theme.muted_foreground),
            )
            .child(div().flex_1().min_w_0().truncate().map(|title| match name {
                Some(name) => title.child(name),
                None => title.text_color(theme.muted_foreground).child("Browser"),
            }))
            .child(
                Button::new("preview-expand")
                    .plain(cx)
                    .xsmall()
                    .icon(Icon::new(IconName::PanelRight))
                    .tooltip_with_action("Show beside the chat", &PreviewBrowser, Some("Chat"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        // Not the card's own click as well.
                        cx.stop_propagation();
                        this.expand_browser_preview(window, cx);
                    })),
            )
            .child(
                Button::new("preview-close")
                    .plain(cx)
                    .xsmall()
                    .icon(Icon::new(IconName::Close))
                    .tooltip_with_action(
                        "Close the preview",
                        &CloseBrowserPreview,
                        Some("BrowserPreview"),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_browser_preview(window, cx);
                    })),
            );
        let footer = doing.map(|doing| {
            h_flex()
                .h(PREVIEW_FOOTER)
                .flex_shrink_0()
                .px_2()
                .items_center()
                .border_t_1()
                .border_color(theme.border)
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(div().min_w_0().truncate().child(doing))
        });
        Some(
            v_flex()
                .id("browser-preview")
                .absolute()
                .right(PREVIEW_INSET)
                .bottom(PREVIEW_INSET)
                .w(width)
                .overflow_hidden()
                .occlude()
                .track_focus(&focus)
                .key_context("BrowserPreview")
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
                .cursor_pointer()
                // As on a folded run: a press leaves the keys where they
                // were, and Tab is what picks the preview out.
                .on_any_mouse_down(|_, window, _| window.prevent_default())
                // gpui also clicks a focused element when Enter comes back
                // up, which after ExpandBrowserPreview would come to nothing.
                .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                    if !event.is_keyboard() {
                        this.expand_browser_preview(window, cx);
                    }
                }))
                .on_action(cx.listener(|this, _: &ExpandBrowserPreview, window, cx| {
                    this.expand_browser_preview(window, cx)
                }))
                .on_action(cx.listener(|this, _: &CloseBrowserPreview, window, cx| {
                    this.close_browser_preview(window, cx)
                }))
                .child(header)
                .child(picture)
                .children(footer)
                // Over it, so nothing moves, as F6's ring is.
                .when(focused, |card| {
                    card.child(
                        div()
                            .absolute()
                            .inset_0()
                            .rounded(theme.radius)
                            .border_2()
                            .border_color(theme.ring),
                    )
                })
                .into_any_element(),
        )
    }

    /// Give the browser all the room but the chat's, or put it back beside
    /// the chat at its share; from anywhere else, show it so.
    pub(crate) fn toggle_wide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab_on_screen(window, cx) != Some(PanelTab::Browser) {
            self.browser.update(cx, |view, _| view.wide = true);
            return self.show_tab(PanelTab::Browser, window, cx);
        }
        // Over the conversation it has all the room there is already.
        if self.panel_covers(window, cx) {
            return;
        }
        self.browser.update(cx, |view, cx| {
            view.wide = !view.wide;
            cx.notify();
        });
    }

    /// Tell the browser how it is drawn now -- beside the chat, over it, or
    /// not at all -- once this draw is done: it watches only while drawn.
    /// Not from the draw itself, which would not be drawn again for it.
    pub(crate) fn keep_browser_seen(&self, window: &Window, cx: &mut Context<Self>) {
        let drawn = match self.panel_view(cx) {
            // Settings covers the whole window, the browser too.
            Some((_, PanelTab::Browser)) if !self.settings_open => {
                Some(match self.place_of(PanelTab::Browser, window, cx) {
                    Place::Over => Drawn::Over,
                    _ => Drawn::Beside,
                })
            }
            _ if self.browser_preview_shown(window, cx) => Some(Drawn::Preview),
            _ => None,
        };
        if self.browser.read(cx).is_drawn_as(drawn) {
            return;
        }
        let browser = self.browser.clone();
        cx.defer(move |cx| browser.update(cx, |view, cx| view.set_drawn(drawn, cx)));
    }

    /// The panel, docked beside the chat or floating over it; None while it
    /// is hidden, or drawn in place of the conversation.
    pub(crate) fn right_panel(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let view = self.panel_view(cx);
        let place = view
            .as_ref()
            .map(|&(_, open)| self.place_of(open, window, cx));
        // In from the right edge, or up into its corner, as it opens.
        let beside = place.is_some_and(|place| place != Place::Over);
        let t = (self.motion.panel).track(beside, crate::ui::motion::STANDARD, window, cx);
        let ((shown, open), place) = (view?, place?);
        if place == Place::Over {
            return None;
        }
        let panel = self.panel(shown, open, window, cx);
        Some(match place {
            Place::Docked(width) => panel
                .relative()
                .when(t < 1., |panel| crate::ui::motion::slide(panel, t, 1.))
                .h_full()
                .w(width)
                .flex_shrink_0()
                .border_l_1()
                .on_drag_move(
                    cx.listener(move |this, event: &DragMoveEvent<Edge>, window, cx| {
                        let right = window.viewport_size().width;
                        let width = right - event.event.position.x;
                        match open {
                            // As a share of the room, so it suits the window
                            // at any size; no longer wide, once dragged.
                            PanelTab::Browser => {
                                let room = right - this.sidebar_width();
                                this.panel.share = Some(share_of(width, room));
                                this.browser.update(cx, |view, _| view.wide = false);
                            }
                            _ => this.panel.width = Some(width),
                        }
                        cx.notify();
                    }),
                )
                .child(edge(open, cx))
                .into_any_element(),
            _ => panel
                .absolute()
                // Top right, clear of the composer and the newest messages.
                .top(FLOAT_TOP + px(crate::ui::motion::RISE * (1. - t)))
                .opacity(t)
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

    /// The panel in place of the conversation, when it is drawn there.
    pub(crate) fn covering_panel(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (shown, open) = self.panel_view(cx)?;
        if self.place_of(open, window, cx) != Place::Over {
            return None;
        }
        let panel = self.panel(shown, open, window, cx);
        Some(panel.relative().flex_1().min_h_0().into_any_element())
    }

    /// The tab bar over the open tab, which alone is drawn.
    fn panel(
        &self,
        shown: Vec<(PanelTab, usize)>,
        open: PanelTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
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
        v_flex()
            .track_focus(&self.panel_focus)
            // As on the sidebar: a click leaves the keys where they were.
            .on_any_mouse_down(|_, window, _| window.prevent_default())
            .bg(cx.theme().background)
            .border_color(cx.theme().border)
            .child(bar)
            .child(self.tab_body(open, window, cx))
            .children(self.ring(crate::ui::keys::Region::Panel, cx))
    }

    /// Shrink the browser to its preview when its bar asks.
    pub(crate) fn follow_browser_shrink(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe_in(
            &self.browser,
            window,
            |this, _, _: &ToPreview, window, cx| this.toggle_browser_preview(window, cx),
        )
    }

    /// Show the browser beside the chat when the agent waits on a site's
    /// check: the preview is a picture, and the check wants clicking.
    pub(crate) fn follow_browser_checks(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe_in(&self.browser, window, |this, _, _: &CheckUp, window, cx| {
            if this.tab_on_screen(window, cx) != Some(PanelTab::Browser) {
                this.show_tab(PanelTab::Browser, window, cx);
            }
        })
    }

    /// Put what Point mode found, with its comment, in the composer.
    pub(crate) fn follow_point_comments(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe_in(
            &self.browser,
            window,
            |this, _, PointComment(text), window, cx| {
                this.insert(text, window, cx);
                this.focus_composer(window, cx);
                // Setting a many-line box's text puts its cursor at the
                // start, and what is typed next goes on after the reference.
                this.composer.update(cx, |input, cx| {
                    use gpui_component::input::RopeExt;
                    let end = input.text().offset_to_position(input.value().len());
                    input.set_cursor_position(end, window, cx);
                });
                cx.notify();
            },
        )
    }
}

/// The docked panel's left edge, which drags it wider or narrower, and
/// back to its own width when clicked twice: the browser to half.
fn edge(open: PanelTab, cx: &mut Context<Chat>) -> impl IntoElement {
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
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                if event.click_count == 2 {
                    match open {
                        PanelTab::Browser => {
                            this.panel.share = None;
                            this.browser.update(cx, |view, _| view.wide = false);
                        }
                        _ => this.panel.width = None,
                    }
                    cx.notify();
                }
            }),
        )
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{
        Panel, PanelTab, browser_width, dock_width, preview_width, share_of, shown_tabs, title,
    };
    use gpui::px;

    #[test]
    fn a_tab_shows_only_with_something_to_show_and_counts_what_waits() {
        let own = || PanelTab::OWN.to_vec();
        assert!(shown_tabs(own(), |_| None).is_empty());
        assert_eq!(
            shown_tabs(own(), |tab| (tab == PanelTab::Browser).then_some(1)),
            vec![(PanelTab::Browser, 1)]
        );
        assert_eq!(
            shown_tabs(own(), |_| Some(0)),
            vec![
                (PanelTab::Browser, 0),
                (PanelTab::Changes, 0),
                (PanelTab::Files, 0),
                (PanelTab::Jobs, 0)
            ]
        );
        assert_eq!(
            shown_tabs(own(), |tab| (tab == PanelTab::Files).then_some(0)),
            vec![(PanelTab::Files, 0)]
        );
        // A plugin's tab comes after the window's own, under its plugin's
        // name while no panel of that name is loaded.
        let mut all = own();
        all.push(PanelTab::Plugin("pomodoro"));
        let shown = shown_tabs(all, |tab| matches!(tab, PanelTab::Plugin(_)).then_some(0));
        assert_eq!(shown, vec![(PanelTab::Plugin("pomodoro"), 0)]);
        assert_eq!(title(PanelTab::Plugin("pomodoro"), 0), "pomodoro");
        assert_eq!(title(PanelTab::Browser, 0), "Browser");
        assert_eq!(title(PanelTab::Browser, 1), "Browser 1");
        assert_eq!(title(PanelTab::Jobs, 0), "Jobs");
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
    fn the_browser_takes_half_the_room_beside_the_sidebar() {
        // Letronna's own window, 1100 wide, with the sidebar open and shut.
        assert_eq!(browser_width(px(850.), None, false), Some(px(425.)));
        assert_eq!(browser_width(px(1100.), None, false), Some(px(550.)));
    }

    #[test]
    fn the_browser_and_the_chat_each_keep_their_least() {
        // Half of 820 would leave the chat 410.
        assert_eq!(browser_width(px(820.), None, false), Some(px(400.)));
        assert_eq!(browser_width(px(1000.), Some(0.1), false), Some(px(400.)));
        assert_eq!(browser_width(px(1000.), Some(0.9), false), Some(px(580.)));
    }

    #[test]
    fn a_dragged_share_is_kept_and_suits_the_window_at_any_size() {
        let share = share_of(px(1000.), px(1600.));
        assert_eq!(share, 0.625);
        assert_eq!(
            browser_width(px(1600.), Some(share), false),
            Some(px(1000.))
        );
        assert_eq!(browser_width(px(1200.), Some(share), false), Some(px(750.)));
        // Dragged past what either side can give, it keeps what was seen.
        assert_eq!(share_of(px(100.), px(1000.)), 0.4);
        assert_eq!(share_of(px(990.), px(1000.)), 0.58);
    }

    #[test]
    fn wide_leaves_the_chat_its_least_and_goes_back_to_the_share() {
        assert_eq!(browser_width(px(1500.), Some(0.625), true), Some(px(1080.)));
        assert_eq!(
            browser_width(px(1500.), Some(0.625), false),
            Some(px(937.5))
        );
        assert_eq!(browser_width(px(1500.), None, true), Some(px(1080.)));
        assert_eq!(browser_width(px(1500.), None, false), Some(px(750.)));
    }

    #[test]
    fn the_preview_is_narrower_over_a_narrow_chat() {
        assert_eq!(preview_width(px(900.)), px(300.));
        assert_eq!(preview_width(px(640.)), px(300.));
        assert_eq!(preview_width(px(639.)), px(220.));
    }

    #[test]
    fn a_forgotten_session_keeps_no_preview() {
        let mut panel = Panel::default();
        panel.previews.insert(1);
        panel.previews.insert(2);
        panel.forget(1);
        assert!(!panel.previews.contains(&1) && panel.previews.contains(&2));
        panel.forget_previews();
        assert!(panel.previews.is_empty());
    }

    #[test]
    fn a_narrow_room_has_the_browser_over_the_chat_and_the_other_tabs_as_before() {
        // A window 1000 wide leaves 750 beside the sidebar: too little for
        // 400 and 420 side by side, wide or not.
        assert_eq!(browser_width(px(750.), None, false), None);
        assert_eq!(browser_width(px(819.), None, true), None);
        // The other tabs still go by the window, and float under 1100,
        // where the browser beside a closed sidebar docks at half.
        assert_eq!(dock_width(px(1000.), px(250.), None), None);
        assert_eq!(dock_width(px(1000.), px(0.), None), None);
        assert_eq!(browser_width(px(1000.), None, false), Some(px(500.)));
    }
}
