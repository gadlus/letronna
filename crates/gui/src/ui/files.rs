//! The Files tab of the right panel: the project's tree, a preview of the
//! file picked, and the ways to hand a file on -- to the message, to an
//! editor, to the system.
//!
//! It is a reader, not an editor: editing goes to the user's own editor.
//! The folders are read by core (files.rs) off the UI thread, again after a
//! tool comes back and when the window comes to the front, and only while
//! the tab is on screen; hidden, it only notes that it is out of date. The
//! files the session's tools wrote carry a dot, and so do the folders they
//! are in, so the agent's work can be found in a tree that is closed.

use crate::prelude::*;
use crate::ui::panel::PanelTab;
use gpui_component::Selectable;
use gpui_component::list::ListItem;
use letronna_core::files::{self, Preview, Row};
use std::collections::HashSet;
use std::ops::Range;
use std::path::Path;
use std::rc::Rc;

actions!(
    files,
    [
        FilesTab,
        FileUp,
        FileDown,
        FileLeft,
        FileRight,
        OpenFile,
        AddToMessage,
        OpenInEditor,
        OpenWithSystem,
        ShowInFolder,
        CopyPath,
        ToggleIgnored,
        ClosePreview,
    ]
);

/// What a button under the tree does to the picked row.
type Run = fn(&mut Chat, &mut Window, &mut Context<Chat>);

/// How far each level of the tree is set in.
const INDENT: f32 = 14.;
/// How tall the tab is in the floating panel, at most.
const FLOAT_HEIGHT: Pixels = px(440.);
/// The share of the tab the tree keeps while a file is previewed under it.
const TREE_SHARE: f32 = 0.4;

/// The tree, the file previewed and whether either is out of date.
pub(crate) struct Files {
    tree: files::Tree,
    /// The session's project folder the tree was last shown for, as the
    /// session names it, so a switch of session is seen without a read.
    shown_for: Option<PathBuf>,
    /// The keys go to the tree while it has this focus.
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    preview_scroll: ScrollHandle,
    shown: Option<Shown>,
    /// Something may have changed on disk since the last read.
    stale: bool,
    reading: Option<Task<()>>,
    previewing: Option<Task<()>>,
    /// Why the project folder could not be read.
    trouble: Option<String>,
}

/// A file previewed, as it is drawn.
struct Shown {
    path: PathBuf,
    look: Result<Look, String>,
}

/// What a preview draws, worked out off the UI thread.
enum Look {
    Markdown(SharedString),
    /// The file's lines, and the longest, which the list takes its width from.
    Lines(Vec<SharedString>, usize),
    Picture(PathBuf),
    Elsewhere(String),
}

impl From<Preview> for Look {
    fn from(preview: Preview) -> Self {
        match preview {
            Preview::Markdown(text) => Look::Markdown(text.into()),
            Preview::Text(text) => {
                // A tab is as wide as one space in a list of glyphs, which
                // would pull indented code to the left.
                let lines: Vec<String> = text
                    .lines()
                    .map(|line| line.replace('\t', "    "))
                    .collect();
                let widest = (0..lines.len())
                    .max_by_key(|&ix| lines[ix].chars().count())
                    .unwrap_or(0);
                Look::Lines(lines.into_iter().map(SharedString::from).collect(), widest)
            }
            Preview::Picture(path) => Look::Picture(path),
            Preview::Elsewhere(why) => Look::Elsewhere(why),
        }
    }
}

impl Files {
    pub(crate) fn new(cx: &mut App) -> Self {
        Self {
            tree: files::Tree::default(),
            shown_for: None,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::default(),
            preview_scroll: ScrollHandle::default(),
            shown: None,
            stale: true,
            reading: None,
            previewing: None,
            trouble: None,
        }
    }

    /// Note that files may have changed, for the next time the tab is seen.
    pub(crate) fn changed(&mut self) {
        self.stale = true;
    }
}

impl Chat {
    /// Whether the session on screen has a project folder to show.
    pub(crate) fn has_files(&self) -> bool {
        self.store.conversations[self.store.active]
            .workspace
            .is_some()
    }

    /// Show the Files tab, or say why there is none.
    pub(crate) fn open_files_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_files() {
            self.note = Some("Pick a project folder for this session to see its files.".into());
            cx.notify();
            return;
        }
        self.show_tab(PanelTab::Files, window, cx);
    }

    /// Give the tree the keys, as the tab is shown.
    pub(crate) fn focus_files(&self, window: &mut Window) {
        window.focus(&self.files.focus);
    }

    /// Read the tree again, and the file previewed, when the tab is on
    /// screen and either may be out of date: on first sight, after a tool
    /// came back, on coming to the front, or with another session's folder.
    pub(crate) fn keep_files_current(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.tab_on_screen(window, cx) != Some(PanelTab::Files) {
            return;
        }
        let workspace = self.store.conversations[self.store.active]
            .workspace
            .clone();
        if workspace != self.files.shown_for {
            self.files.tree.show(workspace.as_deref());
            self.files.shown_for = workspace;
            self.files.shown = None;
            // A file still being read would otherwise land in the new project.
            self.files.previewing = None;
            self.files.trouble = None;
            self.files.stale = true;
        }
        if self.files.reading.is_some() {
            return;
        }
        if std::mem::take(&mut self.files.stale) {
            if let Some(path) = self.files.shown.as_ref().map(|shown| shown.path.clone()) {
                self.preview_file(path, cx);
            }
            self.read_files(cx);
        } else if self.files.tree.unread() {
            self.read_files(cx);
        }
    }

    /// Coming to the front, the files may have been changed elsewhere.
    pub(crate) fn files_on_front(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !window.is_window_active() {
            return;
        }
        self.files.changed();
        if self.tab_on_screen(window, cx) == Some(PanelTab::Files) {
            cx.notify();
        }
    }

    /// Read the project folder and every open folder off the UI thread.
    fn read_files(&mut self, cx: &mut Context<Self>) {
        let Some((root, dirs, ignored)) = self.files.tree.wanted() else {
            return;
        };
        self.files.reading = Some(cx.spawn(async move |this, cx| {
            let read = cx
                .background_executor()
                .spawn(async move { files::read(&root, &dirs, ignored) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.files.reading = None;
                match read {
                    Ok(listing) => {
                        this.files.trouble = None;
                        this.files.tree.take(listing);
                    }
                    Err(why) => this.files.trouble = Some(why),
                }
                cx.notify();
            });
        }));
    }

    /// Preview a file, read off the UI thread.
    fn preview_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(root) = self.files.tree.root().map(Path::to_path_buf) else {
            return;
        };
        let reading = path.clone();
        self.files.previewing = Some(cx.spawn(async move |this, cx| {
            let look = cx
                .background_executor()
                .spawn(async move { files::preview(&root, &reading).map(Look::from) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.files.previewing = None;
                // A preview read again keeps its place; another starts at the top.
                if this
                    .files
                    .shown
                    .as_ref()
                    .is_none_or(|shown| shown.path != path)
                {
                    this.files.preview_scroll.set_offset(point(px(0.), px(0.)));
                }
                this.files.shown = Some(Shown { path, look });
                cx.notify();
            });
        }));
    }

    /// The row picked, as a path.
    fn picked_file(&self) -> Option<PathBuf> {
        self.files.tree.selected().map(Path::to_path_buf)
    }

    /// Keep the picked row in sight after the keys moved it.
    fn follow_pick(&mut self, cx: &mut Context<Self>) {
        let rows = self.files.tree.rows();
        if let Some(ix) = self.files.tree.place(&rows) {
            self.files.scroll.scroll_to_item(ix, ScrollStrategy::Top);
        }
        cx.notify();
    }

    fn step_file(&mut self, by: isize, cx: &mut Context<Self>) {
        self.files.tree.step(by);
        self.follow_pick(cx);
    }

    fn file_right(&mut self, cx: &mut Context<Self>) {
        if self.files.tree.right() {
            self.read_files(cx);
        }
        self.follow_pick(cx);
    }

    fn file_left(&mut self, cx: &mut Context<Self>) {
        self.files.tree.left();
        self.follow_pick(cx);
    }

    /// Enter or a click: a folder opens or closes, a file is previewed.
    fn open_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if path.is_dir() {
            if self.files.tree.toggle(&path) {
                self.read_files(cx);
            }
        } else {
            self.files.tree.select(&path);
            self.preview_file(path, cx);
        }
        cx.notify();
    }

    fn open_picked(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.picked_file() {
            self.open_file(path, cx);
        }
    }

    /// Put an `@path` to the picked row in the message, and go there.
    fn add_to_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(path), Some(root)) = (self.picked_file(), self.files.tree.root()) else {
            return;
        };
        let reference = files::reference(root, &path);
        let typed = self.composer.read(cx).value();
        let gap = if typed.is_empty() || typed.ends_with(char::is_whitespace) {
            ""
        } else {
            " "
        };
        self.insert(&format!("{gap}{reference} "), window, cx);
        self.focus_composer(window, cx);
        // Setting a many-line box's text puts its cursor at the start, and
        // what is typed next goes on after the reference.
        self.composer.update(cx, |input, cx| {
            use gpui_component::input::RopeExt;
            let end = input.text().offset_to_position(input.value().len());
            input.set_cursor_position(end, window, cx);
        });
    }

    /// Open the picked row in the user's editor, or with the system when
    /// no editor is found. Off the UI thread: finding the editor can wait
    /// on the login shell for its PATH.
    fn open_in_editor(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.picked_file() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let target = path.clone();
            let opened = cx
                .background_executor()
                .spawn(async move { files::open_in_editor(&target) })
                .await;
            let _ = this.update(cx, |this, cx| match opened {
                Ok(true) => {}
                Ok(false) => cx.open_with_system(&path),
                Err(why) => {
                    this.error = Some(why);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn reveal_picked(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.picked_file() {
            cx.reveal_path(&path);
        }
    }

    fn copy_picked(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.picked_file() {
            cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
        }
    }

    fn close_preview(&mut self, cx: &mut Context<Self>) {
        if self.files.shown.take().is_none() {
            // Nothing to close: escape goes on to the chat.
            return cx.propagate();
        }
        self.files.previewing = None;
        cx.notify();
    }

    fn toggle_ignored(&mut self, cx: &mut Context<Self>) {
        let ignored = !self.files.tree.ignored();
        self.files.tree.show_ignored(ignored);
        self.read_files(cx);
        cx.notify();
    }

    /// The Files tab: the tree, what the picked row can do, and the file
    /// previewed under them.
    pub(crate) fn files_tab(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, border) = (theme.muted_foreground, theme.border);
        let root = self.files.tree.root();
        let folder = root
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let head = h_flex()
            .flex_shrink_0()
            .px_3()
            .py_1()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(HEADING_FONT)
                    .child(folder),
            )
            .child(
                Button::new("files-ignored")
                    .plain(cx)
                    .xsmall()
                    .icon(Icon::new(AppIcon::View))
                    .selected(self.files.tree.ignored())
                    .tooltip_with_action(
                        if self.files.tree.ignored() {
                            "Hide ignored files"
                        } else {
                            "Show ignored files"
                        },
                        &ToggleIgnored,
                        Some("Files"),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_ignored(cx))),
            );
        let previewing = self.files.shown.is_some();
        let docked = crate::ui::panel::docks(window.viewport_size().width);
        v_flex()
            .w_full()
            .min_h_0()
            // Floating, the panel is as tall as what it holds, and the tree
            // would have no height to fill; it shrinks in a short window.
            .map(|tab| match docked {
                true => tab.flex_1(),
                false => tab.h(FLOAT_HEIGHT).flex_shrink(),
            })
            .key_context("Files")
            .on_action(
                cx.listener(|this, _: &AddToMessage, window, cx| this.add_to_message(window, cx)),
            )
            .on_action(cx.listener(|this, _: &OpenInEditor, _, cx| this.open_in_editor(cx)))
            .on_action(cx.listener(|this, _: &OpenWithSystem, _, cx| {
                if let Some(path) = this.picked_file() {
                    cx.open_with_system(&path)
                }
            }))
            .on_action(cx.listener(|this, _: &ShowInFolder, _, cx| this.reveal_picked(cx)))
            .on_action(cx.listener(|this, _: &CopyPath, _, cx| this.copy_picked(cx)))
            .on_action(cx.listener(|this, _: &ToggleIgnored, _, cx| this.toggle_ignored(cx)))
            .on_action(cx.listener(|this, _: &ClosePreview, _, cx| this.close_preview(cx)))
            .child(head)
            .children(self.files.trouble.clone().map(|why| {
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(why)
            }))
            .child(
                div()
                    .id("file-tree")
                    .key_context("FileTree")
                    .track_focus(&self.files.focus)
                    .relative()
                    .min_h_0()
                    .map(|tree| match previewing {
                        true => tree.h(relative(TREE_SHARE)).flex_shrink_0(),
                        false => tree.flex_1(),
                    })
                    .on_action(cx.listener(|this, _: &FileUp, _, cx| this.step_file(-1, cx)))
                    .on_action(cx.listener(|this, _: &FileDown, _, cx| this.step_file(1, cx)))
                    .on_action(cx.listener(|this, _: &FileLeft, _, cx| this.file_left(cx)))
                    .on_action(cx.listener(|this, _: &FileRight, _, cx| this.file_right(cx)))
                    .on_action(cx.listener(|this, _: &OpenFile, _, cx| this.open_picked(cx)))
                    .child(self.file_rows(cx))
                    .vertical_scrollbar(&self.files.scroll),
            )
            .children(self.file_actions(cx))
            .children(previewing.then(|| {
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .border_t_1()
                    .border_color(border)
                    .child(self.preview_head(muted, cx))
                    .child(self.preview_body(window, cx))
            }))
            .into_any_element()
    }

    /// The tree's rows, only those in sight laid out.
    fn file_rows(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = Rc::new(self.files.tree.rows());
        let picked = self.files.tree.place(&rows);
        let conversation = &self.store.conversations[self.store.active];
        let touched = Rc::new(files::touched(&conversation.messages));
        uniform_list(
            "file-rows",
            rows.len(),
            cx.processor(move |_, range: Range<usize>, _, cx| {
                range
                    .map(|ix| file_row(ix, &rows[ix], picked == Some(ix), &touched, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .size_full()
        .track_scroll(self.files.scroll.clone())
    }

    /// What the picked row can do, beside its name.
    fn file_actions(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let picked = self.files.tree.selected()?;
        let name = picked
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let button =
            |id: &'static str, icon: Icon, tip: &'static str, action: &dyn Action, run: Run| {
                Button::new(id)
                    .plain(cx)
                    .xsmall()
                    .icon(icon)
                    .tooltip_with_action(tip, action, Some("Files"))
                    .on_click(cx.listener(move |this, _, window, cx| run(this, window, cx)))
            };
        Some(
            h_flex()
                .flex_shrink_0()
                .px_3()
                .py_1()
                .gap_1()
                .border_t_1()
                .border_color(cx.theme().border)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(name),
                )
                .child(button(
                    "file-mention",
                    Icon::new(IconName::Plus),
                    "Add to message",
                    &AddToMessage,
                    Chat::add_to_message,
                ))
                .child(button(
                    "file-editor",
                    Icon::new(AppIcon::Pencil),
                    "Open in editor",
                    &OpenInEditor,
                    |this, _, cx| this.open_in_editor(cx),
                ))
                .child(button(
                    "file-reveal",
                    Icon::new(AppIcon::Folder),
                    "Show in folder",
                    &ShowInFolder,
                    |this, _, cx| this.reveal_picked(cx),
                ))
                .child(button(
                    "file-copy",
                    Icon::new(IconName::Copy),
                    "Copy path",
                    &CopyPath,
                    |this, _, cx| this.copy_picked(cx),
                )),
        )
    }

    /// The previewed file's name, and the way to close it.
    fn preview_head(&self, muted: Hsla, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self
            .files
            .shown
            .as_ref()
            .and_then(|shown| shown.path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        h_flex()
            .flex_shrink_0()
            .px_3()
            .py_1()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(HEADING_FONT)
                    .text_color(muted)
                    .child(name),
            )
            .child(
                Button::new("close-preview")
                    .plain(cx)
                    .xsmall()
                    .icon(Icon::new(IconName::Close))
                    .tooltip_with_action("Close the preview", &ClosePreview, Some("Files"))
                    .on_click(cx.listener(|this, _, _, cx| this.close_preview(cx))),
            )
    }

    /// The previewed file: rendered, as numbered lines, as a picture, or
    /// handed to the system.
    fn preview_body(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(shown) = self.files.shown.as_ref() else {
            return Empty.into_any_element();
        };
        let look = match &shown.look {
            Ok(look) => look,
            Err(why) => {
                return div()
                    .px_3()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(why.clone())
                    .into_any_element();
            }
        };
        match look {
            Look::Markdown(text) => {
                let id = SharedString::from(format!("file-md-{}", shown.path.display()));
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("file-md-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.files.preview_scroll)
                            .px_3()
                            .pb_3()
                            .child(
                                TextView::markdown(id, text.clone(), window, cx).selectable(true),
                            ),
                    )
                    .vertical_scrollbar(&self.files.preview_scroll)
                    .into_any_element()
            }
            Look::Lines(lines, widest) => self.numbered_lines(lines.len(), *widest, cx),
            Look::Picture(path) => div()
                .flex_1()
                .min_h_0()
                .p_3()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    img(path.clone())
                        .max_w_full()
                        .max_h_full()
                        .object_fit(ObjectFit::Contain),
                )
                .into_any_element(),
            Look::Elsewhere(why) => {
                let path = shown.path.clone();
                v_flex()
                    .px_3()
                    .gap_2()
                    .items_start()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(why.clone()),
                    )
                    .child(
                        Button::new("open-with-system")
                            .framed(cx)
                            .small()
                            .icon(Icon::new(AppIcon::PopOut))
                            .label("Open with system")
                            .tooltip_with_action(
                                "Open with the program the system uses for it",
                                &OpenWithSystem,
                                Some("Files"),
                            )
                            .on_click(move |_, _, cx| cx.open_with_system(&path)),
                    )
                    .into_any_element()
            }
        }
    }

    /// A text file's lines, numbered, in the code font; only those in
    /// sight are laid out, and a long line scrolls sideways.
    fn numbered_lines(&self, count: usize, widest: usize, cx: &mut Context<Self>) -> AnyElement {
        let digits = count.max(1).to_string().len() as f32;
        uniform_list(
            "file-lines",
            count,
            cx.processor(move |this, range: Range<usize>, _, cx| {
                let Some(Shown {
                    look: Ok(Look::Lines(lines, _)),
                    ..
                }) = this.files.shown.as_ref()
                else {
                    return Vec::new();
                };
                let muted = cx.theme().muted_foreground;
                range
                    .filter_map(|ix| {
                        let line = lines.get(ix)?.clone();
                        Some(
                            h_flex()
                                .id(ix)
                                .gap_3()
                                .px_3()
                                .whitespace_nowrap()
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        // Digits are about 0.6em wide in a
                                        // code font.
                                        .w(rems(0.45 * digits))
                                        .text_right()
                                        .text_color(muted)
                                        .child((ix + 1).to_string()),
                                )
                                .child(line),
                        )
                    })
                    .collect()
            }),
        )
        .flex_1()
        .min_h_0()
        .font_family(MONO_FONT)
        .text_xs()
        .with_width_from_item(Some(widest))
        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
        .into_any_element()
    }
}

/// One row of the tree: a folder's chevron or a file's mark, its name,
/// muted when `.gitignore` leaves it out, and a dot when the session's
/// tools wrote it or something in it.
fn file_row(
    ix: usize,
    row: &Row,
    picked: bool,
    touched: &HashSet<PathBuf>,
    cx: &mut Context<Chat>,
) -> ListItem {
    let entry = &row.entry;
    let theme = cx.theme();
    let written = touched.iter().any(|path| path.starts_with(&entry.path));
    let mark = match (entry.dir, row.open) {
        (true, true) => Icon::new(IconName::ChevronDown),
        (true, false) => Icon::new(IconName::ChevronRight),
        (false, _) => Icon::new(AppIcon::File),
    };
    let path = entry.path.clone();
    ListItem::new(ix)
        .selected(picked)
        .py_0p5()
        .pl(px(8. + INDENT * row.depth as f32))
        .pr_3()
        .on_click(cx.listener(move |this, _, _, cx| this.open_file(path.clone(), cx)))
        .child(
            h_flex()
                .w_full()
                .gap_1p5()
                .text_sm()
                .when(entry.ignored, |row| row.text_color(theme.muted_foreground))
                .child(mark.size_3p5().text_color(theme.muted_foreground))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(entry.name.clone()),
                )
                .when(written, |row| {
                    row.child(div().size(px(6.)).rounded_full().bg(theme.link))
                }),
        )
}
