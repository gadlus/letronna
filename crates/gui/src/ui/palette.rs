//! The command palette: Ctrl+K lists what the window can do and where it can
//! go. Every action shows its key, so the palette is also where the keys are
//! learnt. The rows are built from what the window already holds when it
//! opens, so they are there on the first frame, and dropped when it closes.

use crate::prelude::*;
use gpui_component::{
    IndexPath,
    list::{List, ListDelegate, ListItem, ListState},
};
use letronna_core::palette::{self, Rank};
use std::{path::Path, rc::Rc};

actions!(palette, [OpenPalette]);

const FOLDERS: &str = "Folders";

/// What picking a row does.
#[derive(Clone)]
enum Pick {
    Action(Rc<dyn Action>),
    Page(Page),
    Settings(&'static str),
    Session(u64),
    Skill(String),
    Model(String, String),
    Folder(PathBuf),
    /// An appearance setting's value: "" Night, "paper" or "system".
    Look(&'static str),
    /// Pick the project folder of the session on screen.
    ChooseFolder,
    /// Sign in to a Letronna account in the browser.
    SignIn,
}

#[derive(Clone)]
struct Row {
    icon: AppIcon,
    label: SharedString,
    /// Shown muted after the label, and searched with it.
    detail: SharedString,
    /// Shown at the end of the row and not searched: a session's age, or
    /// what picking a skill does.
    note: SharedString,
    key: Option<Keystroke>,
    /// The session's own model, or the appearance in use, ticked.
    current: bool,
    pick: Pick,
}

impl Row {
    fn new(icon: AppIcon, label: impl Into<SharedString>, pick: Pick) -> Self {
        Self {
            icon,
            label: label.into(),
            detail: SharedString::default(),
            note: SharedString::default(),
            key: None,
            current: false,
            pick,
        }
    }

    /// How well the row answers the typing. A pasted session id and a typed
    /// folder name their row outright, whatever its text.
    fn rank(&self, query: &str, pasted: Option<u64>, typed: Option<&Path>) -> Option<Rank> {
        match &self.pick {
            Pick::Session(id) if pasted == Some(*id) => Some(Rank::Exact),
            Pick::Folder(folder) if typed == Some(folder.as_path()) => Some(Rank::Exact),
            _ => palette::best(query, [self.label.as_ref(), self.detail.as_ref()]),
        }
    }
}

fn folder_row(folder: PathBuf) -> Row {
    let name = folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| folder.display().to_string());
    Row {
        detail: folder.display().to_string().into(),
        ..Row::new(AppIcon::Folder, name, Pick::Folder(folder))
    }
}

/// The window's commands as the palette names them. The key beside each is
/// looked up in the bindings `main.rs` makes, not written again here, so the
/// two cannot drift apart.
fn commands() -> Vec<(&'static str, Rc<dyn Action>)> {
    vec![
        ("New session", Rc::new(NewSession)),
        ("Focus the composer", Rc::new(FocusComposer)),
        ("Show or hide the sidebar", Rc::new(ToggleSidebar)),
        ("Show or hide the panel", Rc::new(super::panel::TogglePanel)),
        ("Show the browser", Rc::new(super::panel::BrowserTab)),
        (
            "Give the browser the most room, or put it back",
            Rc::new(super::panel::WideBrowser),
        ),
        (
            "Browser: shrink to preview / show beside the chat",
            Rc::new(super::panel::PreviewBrowser),
        ),
        ("Show the files", Rc::new(super::files::FilesTab)),
        ("Show the changes", Rc::new(super::changes::ChangesTab)),
        ("Show the jobs", Rc::new(super::jobs::JobsTab)),
        ("Switch Crew on or off", Rc::new(ToggleCrew)),
        ("Open settings", Rc::new(OpenSettings)),
        ("Open usage", Rc::new(super::usage::OpenUsage)),
        ("Open activity", Rc::new(OpenActivity)),
        (
            "Show what is running",
            Rc::new(super::activity::ShowRunning),
        ),
        ("Show the keys", Rc::new(ShowKeys)),
        ("Send now, into the reply", Rc::new(SendNow)),
        // Escape's action: it stops the reply only once nothing else is
        // open to back out of, such as Settings or an edit.
        ("Back out, or stop the reply", Rc::new(Dismiss)),
        (
            "Switch to a recent session",
            Rc::new(super::switcher::SwitchSession),
        ),
    ]
}

/// The pages the sidebar goes to, with its icons.
const PAGES: &[(Page, AppIcon)] = &[
    (Page::Capabilities, AppIcon::Sparkles),
    (Page::Plugins, AppIcon::Puzzle),
    (Page::Messaging, AppIcon::MessageSquare),
    (Page::Artifacts, AppIcon::File),
    (Page::Activity, AppIcon::Robot),
    (Page::Jobs, AppIcon::Clock),
    (Page::Workflows, AppIcon::Workflow),
];

/// The appearances the palette offers, as the setting stores them.
const LOOKS: [(&str, &str); 3] = [
    ("", "Night appearance"),
    ("paper", "Paper appearance"),
    ("system", "Follow the system's appearance"),
];

/// The action that opens `page` from anywhere, for the pages that have a
/// key.
fn page_action(page: Page) -> Option<Rc<dyn Action>> {
    match page {
        Page::Capabilities => Some(Rc::new(super::capabilities::OpenCapabilities)),
        Page::Activity => Some(Rc::new(OpenActivity)),
        _ => None,
    }
}

/// The groups with rows that answer `query`, the group of the best answer
/// first, each with those rows best first.
fn sift(groups: &[(&'static str, Vec<Row>)], query: &str) -> Vec<(&'static str, Vec<Row>)> {
    let pasted = palette::session_id(query);
    let typed = palette::typed_folder(query);
    let mut shown = Vec::new();
    for (title, rows) in groups {
        let mut rows = rows.clone();
        if *title == FOLDERS
            && let Some(folder) = &typed
            && !rows
                .iter()
                .any(|row| matches!(&row.pick, Pick::Folder(f) if f == folder))
        {
            rows.insert(0, folder_row(folder.clone()));
        }
        let ranks: Vec<Option<Rank>> = rows
            .iter()
            .map(|row| row.rank(query, pasted, typed.as_deref()))
            .collect();
        let order = palette::order(&ranks);
        let Some(best) = order.first().and_then(|&i| ranks[i]) else {
            continue;
        };
        let rows = order.into_iter().map(|i| rows[i].clone()).collect();
        shown.push((best, *title, rows));
    }
    // Stable, so groups that answer alike keep their listed order.
    shown.sort_by_key(|(best, _, _)| *best);
    shown
        .into_iter()
        .map(|(_, title, rows)| (title, rows))
        .collect()
}

/// The line under the list saying which keys work it.
fn hint(cx: &App) -> Div {
    h_flex()
        .h(px(32.))
        .px_3()
        .gap_3()
        .items_center()
        .border_t_1()
        .border_color(cx.theme().border)
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child("Up and down to move")
        .child("Enter to go")
        .child("Esc to close")
}

/// The palette's rows, as a gpui-component list's delegate.
pub(crate) struct Palette {
    chat: WeakEntity<Chat>,
    /// Every row, by group, in the order they are listed before any typing.
    groups: Vec<(&'static str, Vec<Row>)>,
    /// What `sift` made of them for the typing.
    shown: Vec<(&'static str, Vec<Row>)>,
    selected: Option<IndexPath>,
}

impl Palette {
    fn row(&self, ix: IndexPath) -> Option<&Row> {
        self.shown.get(ix.section)?.1.get(ix.row)
    }
}

impl ListDelegate for Palette {
    type Item = ListItem;

    fn perform_search(
        &mut self,
        query: &str,
        window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        // In memory and at once, so the rows change in the frame the key
        // was typed in.
        self.shown = sift(&self.groups, query);
        // The list then selects by the rows it last drew, so after a search
        // that found none it would select nothing and Enter would do
        // nothing; this runs after it and selects by the rows found now.
        let first = (!self.shown.is_empty()).then(IndexPath::default);
        cx.spawn_in(window, async move |list, cx| {
            let _ = list.update_in(cx, |list, window, cx| {
                list.set_selected_index(first, window, cx)
            });
        })
    }

    fn sections_count(&self, _: &App) -> usize {
        self.shown.len()
    }

    fn items_count(&self, section: usize, _: &App) -> usize {
        self.shown.get(section).map_or(0, |(_, rows)| rows.len())
    }

    fn render_section_header(
        &mut self,
        section: usize,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<impl IntoElement> {
        let (title, _) = self.shown.get(section)?;
        Some(
            div()
                .h(px(28.))
                .px_3()
                .pt_2()
                .text_xs()
                .font_family(HEADING_FONT)
                .font_weight(FontWeight::MEDIUM)
                .text_color(cx.theme().muted_foreground)
                .child(*title),
        )
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<ListItem> {
        let row = self.row(ix)?;
        let muted = cx.theme().muted_foreground;
        Some(
            ListItem::new(("palette-row", ix.row))
                .h(px(32.))
                .text_sm()
                .rounded(cx.theme().radius)
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .gap_2()
                        .items_center()
                        .child(Icon::new(row.icon).size_4().text_color(muted))
                        .child(div().flex_shrink_0().child(row.label.clone()))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(muted)
                                .child(row.detail.clone()),
                        )
                        .when(row.current, |this| {
                            this.child(Icon::new(IconName::Check).size_4().text_color(muted))
                        })
                        .when(!row.note.is_empty(), |this| {
                            this.child(
                                div()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(row.note.clone()),
                            )
                        })
                        .when_some(row.key.clone(), |this, key| {
                            this.child(Kbd::new(key).text_xs())
                        }),
                ),
        )
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        h_flex()
            .size_full()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("Nothing here matches")
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) {
        self.selected = ix;
    }

    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        let Some(pick) = self
            .selected
            .and_then(|ix| self.row(ix))
            .map(|row| row.pick.clone())
        else {
            return;
        };
        // Closed first, so focus is back where it was before the pick moves
        // it, and an action goes where its key would have.
        window.close_dialog(cx);
        let _ = self
            .chat
            .update(cx, |chat, cx| chat.run_pick(pick, window, cx));
    }
}

impl Chat {
    /// Open the palette over whatever is showing.
    pub(crate) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        let groups = self.palette_groups(cx);
        let palette = Palette {
            chat: cx.entity().downgrade(),
            shown: sift(&groups, ""),
            groups,
            selected: None,
        };
        let list = cx.new(|cx| ListState::new(palette, window, cx).searchable(true));
        list.update(cx, |list, cx| {
            list.set_selected_index(Some(IndexPath::default()), window, cx)
        });
        // Only the list goes in: the builder runs while the chat draws,
        // where reading the chat would panic.
        let shown = list.clone();
        window.open_dialog(cx, move |dialog, _, cx| {
            dialog
                .w(px(600.))
                .p_0()
                .close_button(false)
                .child(
                    List::new(&shown)
                        .h(px(380.))
                        .p_1()
                        .search_placeholder("Type an action, a page, a session, a model or a path"),
                )
                .child(hint(cx))
        });
        list.update(cx, |list, cx| list.focus(window, cx));
    }

    /// Every row the palette can show, from what the window holds now.
    fn palette_groups(&self, cx: &App) -> Vec<(&'static str, Vec<Row>)> {
        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        let mut actions: Vec<Row> = commands()
            .into_iter()
            .map(|(label, action)| {
                // The last bound takes precedence, as when the key is pressed.
                let key = keymap
                    .bindings_for_action(action.as_ref())
                    .next_back()
                    .and_then(|binding| binding.keystrokes().first())
                    .map(|key| key.as_keystroke().clone());
                Row {
                    key,
                    ..Row::new(AppIcon::Play, label, Pick::Action(action))
                }
            })
            .collect();
        actions.push(Row {
            detail: "Attach a folder to this session".into(),
            ..Row::new(
                AppIcon::Folder,
                "Choose a project folder",
                Pick::ChooseFolder,
            )
        });
        actions.extend(LOOKS.iter().map(|&(value, label)| Row {
            current: self.store.settings.appearance == value,
            ..Row::new(AppIcon::Palette, label, Pick::Look(value))
        }));
        // Only while it would do something: signed in, the account menu
        // signs out, and a sign-in already waiting is the account page's.
        if self.account.is_none() && self.sign_in_url.is_none() {
            actions.push(Row {
                detail: "Letronna account".into(),
                ..Row::new(AppIcon::User, "Sign in", Pick::SignIn)
            });
        }

        let mut places: Vec<Row> = PAGES
            .iter()
            .map(|&(page, icon)| Row {
                key: page_action(page).and_then(|action| crate::ui::keys::bound_key(&*action, cx)),
                ..Row::new(icon, page.title(), Pick::Page(page))
            })
            .collect();
        places.extend(super::settings::PAGES.iter().map(|&(name, icon)| Row {
            detail: "Settings".into(),
            ..Row::new(icon, name, Pick::Settings(name))
        }));

        let now = backend::now();
        let mut by_age: Vec<&Conversation> = self.store.conversations.iter().collect();
        by_age.sort_by_key(|c| std::cmp::Reverse(c.updated));
        let sessions = by_age
            .into_iter()
            .map(|c| Row {
                note: age_label(c.updated, now).into(),
                ..Row::new(AppIcon::MessageSquare, c.title.clone(), Pick::Session(c.id))
            })
            .collect();

        let skills = self
            .skills
            .iter()
            .map(|skill| Row {
                detail: skill.description.clone().into(),
                note: "Use".into(),
                ..Row::new(
                    AppIcon::Sparkles,
                    skill.name.clone(),
                    Pick::Skill(skill.name.clone()),
                )
            })
            .collect();

        let (on, using) = self.store.model(self.store.active);
        let mut models = self.models.clone();
        // The model in use is listed even when its gateway did not offer it,
        // as the model menu lists it.
        if !using.is_empty() && !models.iter().any(|(g, m)| g == on && m == using) {
            models.insert(0, (on.to_owned(), using.to_owned()));
        }
        let models = models
            .into_iter()
            .map(|(gateway, id)| Row {
                detail: gateway.clone().into(),
                current: gateway == on && id == using,
                ..Row::new(
                    AppIcon::Robot,
                    id.clone(),
                    Pick::Model(gateway.clone(), id.clone()),
                )
            })
            .collect();

        let folders = palette::recent_folders(&self.store.conversations)
            .into_iter()
            .map(folder_row)
            .collect();

        vec![
            ("Actions", actions),
            ("Go to", places),
            ("Sessions", sessions),
            ("Skills", skills),
            ("Models", models),
            (FOLDERS, folders),
        ]
    }

    /// Do what a palette row stands for.
    fn run_pick(&mut self, pick: Pick, window: &mut Window, cx: &mut Context<Self>) {
        // Going somewhere in the chat leaves Settings, which would cover it.
        let leaves = matches!(
            pick,
            Pick::Page(_) | Pick::Session(_) | Pick::Skill(_) | Pick::Folder(_)
        );
        if leaves && self.settings_open {
            self.close_settings(window, cx);
        }
        match pick {
            Pick::Action(action) => window.dispatch_action(action.boxed_clone(), cx),
            Pick::Page(page) => self.open_page(page, window, cx),
            Pick::Settings(name) => {
                self.settings_page.update(cx, |page, cx| {
                    *page = name;
                    cx.notify();
                });
                if !self.settings_open {
                    self.open_settings(window, cx);
                } else if name == "Usage" {
                    self.load_usage(cx);
                }
            }
            Pick::Session(id) => self.go_to_session(id, window, cx),
            Pick::Skill(name) => self.use_skill(&name, window, cx),
            Pick::Model(gateway, id) => self.choose_model(gateway, id, window, cx),
            Pick::Folder(folder) => self.open_folder(folder, window, cx),
            Pick::Look(value) => {
                self.store.settings.appearance = value.to_owned();
                self.save();
                // The blurred snapshot behind settings still wears the old
                // colours, as when it is picked there.
                self.drop_backdrop(cx);
                crate::theme::apply(value, window, cx);
            }
            Pick::ChooseFolder => self.attach_workspace(window, cx),
            // On the Account page, where the wait for the browser shows,
            // as the account menu does it.
            Pick::SignIn => {
                self.open_account(window, cx);
                self.sign_in(cx);
            }
        }
        cx.notify();
    }

    /// Show session `id` in the chat, at its newest message, with the keys
    /// in the composer.
    pub(crate) fn go_to_session(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.store.index_of(id) else {
            return;
        };
        self.leave_for(ix);
        self.store.active = ix;
        self.error = None;
        self.page = Page::Chat;
        self.follow();
        self.focus_composer(window, cx);
        self.save();
        cx.notify();
    }

    /// Work in `folder`: in the session on screen while nothing has been
    /// said in it, or else in a new one, so a conversation keeps the
    /// project it was about.
    pub(crate) fn open_folder(
        &mut self,
        folder: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.store.conversations[self.store.active].length() > 0 {
            self.new_chat(window, cx);
        }
        self.store.conversations[self.store.active].workspace = Some(folder);
        self.page = Page::Chat;
        self.focus_composer(window, cx);
        self.save();
    }
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{FOLDERS, PAGES, Pick, Row, folder_row, sift};
    use crate::{app::Page, assets::AppIcon};

    #[test]
    fn every_page_the_sidebar_opens_can_be_gone_to_from_here() {
        let pages = [
            Page::Capabilities,
            Page::Plugins,
            Page::Messaging,
            Page::Artifacts,
            Page::Jobs,
            Page::Activity,
            Page::Workflows,
        ];
        for page in pages {
            let listed = PAGES.iter().any(|&(p, _)| p == page);
            assert!(listed, "{} is not in the palette", page.title());
        }
    }

    fn labels(shown: &[(&'static str, Vec<Row>)]) -> Vec<(&'static str, Vec<String>)> {
        shown
            .iter()
            .map(|(title, rows)| (*title, rows.iter().map(|r| r.label.to_string()).collect()))
            .collect()
    }

    fn groups() -> Vec<(&'static str, Vec<Row>)> {
        let page = |page: Page| Row::new(AppIcon::Clock, page.title(), Pick::Page(page));
        let session =
            |id, title: &str| Row::new(AppIcon::MessageSquare, title.to_owned(), Pick::Session(id));
        vec![
            (
                "Go to",
                vec![page(Page::Messaging), page(Page::Capabilities)],
            ),
            (
                "Sessions",
                vec![
                    session(41, "Capabilities to add"),
                    session(7, "Fix the tray"),
                ],
            ),
            (FOLDERS, Vec::new()),
        ]
    }

    #[test]
    fn nothing_typed_lists_every_group_in_order_and_leaves_out_empty_ones() {
        assert_eq!(
            labels(&sift(&groups(), "")),
            vec![
                ("Go to", vec!["Messaging".into(), "Capabilities".into()]),
                (
                    "Sessions",
                    vec!["Capabilities to add".into(), "Fix the tray".into()]
                ),
            ]
        );
    }

    #[test]
    fn the_group_holding_the_best_answer_comes_first() {
        let shown = sift(&groups(), "tray");
        assert_eq!(
            labels(&shown),
            vec![("Sessions", vec!["Fix the tray".into()])]
        );
        // "Capabilities" is exact on the page, a prefix on the session.
        let shown = sift(&groups(), "capabilities");
        assert_eq!(
            labels(&shown),
            vec![
                ("Go to", vec!["Capabilities".into()]),
                ("Sessions", vec!["Capabilities to add".into()]),
            ]
        );
    }

    #[test]
    fn a_pasted_session_id_goes_straight_to_its_session() {
        let shown = sift(&groups(), "7");
        assert_eq!(
            labels(&shown),
            vec![("Sessions", vec!["Fix the tray".into()])]
        );
    }

    #[test]
    fn a_typed_folder_is_offered_once_among_the_folders() {
        let dir = std::env::temp_dir();
        let offered = |groups: &[(&'static str, Vec<Row>)]| {
            let shown = sift(groups, &dir.display().to_string());
            let folders = shown.iter().find(|(title, _)| *title == FOLDERS);
            folders.is_some_and(|(_, rows)| {
                rows.len() == 1 && matches!(&rows[0].pick, Pick::Folder(f) if *f == dir)
            })
        };
        assert!(offered(&groups()));
        // Already a recent folder, it is not listed a second time.
        let mut recent = groups();
        recent[2].1.push(folder_row(dir.clone()));
        assert!(offered(&recent));
    }
}
