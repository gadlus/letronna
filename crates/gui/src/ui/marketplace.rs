//! A `letronna://marketplace/<id>` link, as the "Open in Letronna" button on
//! letronna.ai/marketplace hands over: the window comes forward and says
//! what the listing is and exactly what adding it does, with Add and Cancel.
//!
//! The link is a stranger's, since any page, email or message can carry
//! one. So it only names a listing, which core looks up in letronna.ai's
//! own list (`letronna_core::marketplace`), and nothing is added, fetched
//! from anyone else or run until Add is pressed here. Add is a press, never
//! the Enter key, and waits a moment after what it does comes on screen, so
//! a link that arrives mid-keystroke or mid-click adds nothing. While one
//! link's dialog is open another link is dropped: what is on screen cannot
//! change under the pointer.
//!
//! How a link gets here differs by system. macOS hands it to the app that
//! runs. Windows and Linux start the program again with the link as its
//! first argument, and that start, finding a window already open, leaves
//! the link in `open-link` in the data folder for it, as a session is
//! handed over (`handoff`). A start given a link reads no flag after it:
//! whoever wrote the link may have written what follows it too.

use crate::prelude::*;
use crate::ui::capabilities::CapabilityTab;
use gpui_component::dialog::Dialog;
use gpui_component::notification::Notification;
use letronna_core::marketplace::{self, Added, Key, Kind, Listing, Way};
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The file in the data folder a link is left in for the window that runs.
const ASKED: &str = "open-link";
/// How old a link left there may be and still be opened: one that no
/// window took is not acted on the next day.
const FRESH: Duration = Duration::from_secs(30);
/// The most bytes read from the file a link is left in. What is written
/// there is under 70; a bigger file is not one of ours.
const ASKED_BYTES: u64 = 256;
/// Links that wait for the window at once; more than that is a page playing.
const WAITING: usize = 4;
/// How long what Add does is on screen before Add can be pressed.
const ARMED_AFTER: Duration = Duration::from_millis(800);

type Links = (
    async_channel::Sender<String>,
    async_channel::Receiver<String>,
);

/// The links that have come and that the window has not yet taken.
fn links() -> &'static Links {
    static LINKS: OnceLock<Links> = OnceLock::new();
    LINKS.get_or_init(|| async_channel::bounded(WAITING))
}

/// Whether an argument is a link, whatever it goes on to say.
fn is_link(arg: &str) -> bool {
    arg.get(..9)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("letronna:"))
}

/// The link among `args`: the first of them, when that is one.
fn first_link(mut args: impl Iterator<Item = String>) -> Option<String> {
    args.next().filter(|arg| is_link(arg))
}

/// The link this start of the window was given.
pub(crate) fn link_arg() -> Option<String> {
    first_link(std::env::args().skip(1))
}

/// A link has come for the window: from the system, with this start, or
/// left by a later one. Only what core keeps of it waits here: a listing's
/// id, or the reason it is refused.
pub(crate) fn arrived(link: String) {
    if !is_link(&link) {
        return;
    }
    // Full of links nobody has answered yet: this one is dropped.
    let _ = links().0.try_send(marketplace::kept(&link));
}

/// Leave `link` for the window that runs, or the copy that is about to.
pub(crate) fn ask_for(link: &str) {
    ask_in(&backend::data_dir(), link);
}

/// Leave `link` in `folder`. Only what core keeps of it is written, so no
/// pairing secret and nothing else a link carried reaches the disk. Written
/// beside the file and moved onto it, so the window, which looks the moment
/// the file appears, never finds it still empty.
fn ask_in(folder: &Path, link: &str) {
    let _ = std::fs::create_dir_all(folder);
    let beside = folder.join(format!("{ASKED}.{}", std::process::id()));
    if std::fs::write(&beside, marketplace::kept(link)).is_ok()
        && std::fs::rename(&beside, folder.join(ASKED)).is_err()
    {
        let _ = std::fs::remove_file(&beside);
    }
}

/// The link left in the data folder's file, which is taken.
fn take_asked() -> Option<String> {
    take_in(&backend::data_dir())
}

/// The link left in `folder`, which is taken, when it was left lately. Only
/// a plain file is read, and only its start: a pipe or a link to elsewhere
/// put under that name is removed unread.
fn take_in(folder: &Path) -> Option<String> {
    use std::io::Read as _;
    let path = folder.join(ASKED);
    let about = std::fs::symlink_metadata(&path).ok()?;
    let age = about.modified().ok().map(|at| at.elapsed());
    let mut text = String::new();
    let read = about.file_type().is_file()
        && std::fs::File::open(&path)
            .and_then(|file| file.take(ASKED_BYTES).read_to_string(&mut text))
            .is_ok();
    let _ = std::fs::remove_file(&path);
    let fresh = age.is_some_and(|age| age.is_ok_and(|age| age < FRESH));
    (read && fresh).then_some(text)
}

/// A line of what the dialog says Add does.
#[derive(Debug, PartialEq, Eq)]
enum Said {
    Words(String),
    /// An address, a command or a place, shown whole and as it will be used.
    Exact(String),
}

/// What a listing is called for its kind, as the website's tabs have it.
fn kind_word(kind: Kind) -> &'static str {
    match kind {
        Kind::Plugin => "Plugin",
        Kind::Skill => "Skill",
        Kind::Mcp => "MCP server",
        Kind::Chat => "Chat app",
        Kind::Editor => "Editor",
    }
}

/// The page of the window that `page` is, and what it is called.
fn page_word(page: marketplace::Page) -> &'static str {
    match page {
        marketplace::Page::Plugins => "Plugins",
        marketplace::Page::Messaging => "Messaging",
        marketplace::Page::Skills => "Capabilities › Skills",
    }
}

/// What the button that goes ahead says, for a listing there is something
/// to do with.
fn press(way: &Way) -> Option<String> {
    match way {
        Way::Server { .. } | Way::Command { .. } | Way::Skill { .. } => Some("Add".into()),
        Way::Page(page) => Some(format!("Open {}", page_word(*page))),
        Way::Elsewhere => None,
    }
}

/// The command `words` start, with the folder that follows them: the one
/// picked, or a mark where it will go.
fn command_line(words: &[String], folder: Option<&Path>, takes_folder: bool) -> String {
    let mut line = words.join(" ");
    if takes_folder {
        line.push(' ');
        match folder {
            Some(folder) => line.push_str(&folder.display().to_string()),
            None => line.push_str("<the folder you choose>"),
        }
    }
    line
}

/// What pressing the button does with `listing`, in the order it happens,
/// each address, command and place exactly as it will be used.
fn said(listing: &Listing, folder: Option<&Path>) -> Vec<Said> {
    let name = &listing.name;
    let words = |text: String| Said::Words(text);
    match &listing.way {
        Way::Server {
            name: server,
            url,
            key,
        } => {
            let mut lines = vec![
                words(format!(
                    "Adding it puts an MCP server called {server} in mcp.json, reached at:"
                )),
                Said::Exact(url.clone()),
            ];
            lines.push(words(match key {
                Key::SignIn => format!(
                    "Letronna then connects to that address. To use it you sign in to {name} \
                     from its row under Capabilities › MCP, in your browser, and each of its \
                     tools asks before it runs."
                ),
                Key::Token(_) => "The token you paste below is kept in mcp.json on this \
                     computer and sent to that address, and nowhere else, with each request. \
                     Each of its tools asks before it runs."
                    .to_owned(),
            }));
            lines
        }
        Way::Command {
            name: server,
            words: command,
            folder: takes_folder,
        } => vec![
            words(format!(
                "Adding it puts an MCP server called {server} in mcp.json. Letronna then runs \
                 this on your computer, now and each time it starts:"
            )),
            Said::Exact(command_line(command, folder, *takes_folder)),
            words(format!(
                "That is a program with your own reach on this computer, so add it only if you \
                 trust {}. Each of its tools asks before it runs.",
                match listing.maker.as_str() {
                    "" => "who makes it",
                    maker => maker,
                }
            )),
        ],
        Way::Skill { source } => vec![
            words("Adding it downloads the skill from GitHub, at:".to_owned()),
            Said::Exact(source.clone()),
            words(
                "It goes in your skills folder, switched on. A skill is instructions for the \
                 model: nothing in it is run when it is added."
                    .to_owned(),
            ),
        ],
        Way::Page(page) => vec![words(format!(
            "{name} comes with Letronna, so there is nothing to download. It is set up on the \
             {} page, which this opens; nothing is switched on for you.",
            page_word(*page)
        ))],
        Way::Elsewhere => vec![words(format!(
            "{name} is set up in {name} itself, not here: letronna.ai/marketplace/{} has the \
             steps. Nothing was changed.",
            listing.id
        ))],
    }
}

/// Where a link's dialog has got to.
enum Stage {
    /// letronna.ai's list is being read.
    Looking,
    /// Nothing can be done with the link, and why.
    Refused(String),
    /// What the list says, for the user to add or not.
    Found(Listing),
}

/// The dialog a link opens, and what the user has put into it.
pub(crate) struct LinkSheet {
    chat: WeakEntity<Chat>,
    stage: Stage,
    /// The token, for a server that takes one.
    token: Entity<InputState>,
    /// The folder picked, for a command that works in one.
    folder: Option<PathBuf>,
    /// A server of this listing's name is in mcp.json already.
    taken: bool,
    /// When what Add does came on screen.
    shown: Instant,
    /// Add was pressed and is under way.
    adding: bool,
    /// Why Add added nothing.
    error: Option<String>,
}

/// The link dialog that is open, if one is.
struct Open(WeakEntity<LinkSheet>);

impl Global for Open {}

/// Whether a link's dialog is on screen.
fn sheet_open(cx: &App) -> bool {
    cx.try_global::<Open>()
        .is_some_and(|open| open.0.upgrade().is_some())
}

impl LinkSheet {
    fn new(
        chat: WeakEntity<Chat>,
        stage: Stage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            chat,
            stage,
            token: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Paste the token")
                    .masked(true)
            }),
            folder: None,
            taken: false,
            shown: Instant::now(),
            adding: false,
            error: None,
        }
    }

    /// Read letronna.ai's list for `id`, off this thread, and show what it
    /// says.
    fn look_up(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move { marketplace::find(&id) })
                .await;
            let _ = this.update(cx, |this, cx| this.show(found, cx));
        })
        .detach();
    }

    /// Show what was found, and let Add be pressed a moment later.
    fn show(&mut self, found: Result<Listing, String>, cx: &mut Context<Self>) {
        self.stage = match found {
            Ok(listing) => {
                self.taken = match &listing.way {
                    Way::Server { name, .. } | Way::Command { name, .. } => {
                        letronna_core::mcp::status()
                            .iter()
                            .any(|server| &server.name == name)
                    }
                    _ => false,
                };
                Stage::Found(listing)
            }
            Err(why) => Stage::Refused(why),
        };
        self.shown = Instant::now();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ARMED_AFTER).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
        cx.notify();
    }

    /// Whether what Add does has been on screen long enough to mean it.
    fn armed(&self) -> bool {
        self.shown.elapsed() >= ARMED_AFTER
    }

    /// What the user gave for this listing, or None while it is still owed:
    /// the token, the folder, or nothing where none is asked for.
    fn given(&self, cx: &App) -> Option<String> {
        let Stage::Found(listing) = &self.stage else {
            return None;
        };
        match &listing.way {
            Way::Server {
                key: Key::Token(_), ..
            } => {
                let token = self.token.read(cx).value().trim().to_owned();
                (!token.is_empty()).then_some(token)
            }
            Way::Command { folder: true, .. } => Some(self.folder.as_ref()?.to_str()?.to_owned()),
            _ => Some(String::new()),
        }
    }

    /// Pick the folder a command works in, in the system's own picker.
    fn choose_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.armed() {
            return;
        }
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            let Some(folder) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.folder = Some(folder);
                this.error = None;
                cx.notify();
            });
        })
        .detach();
    }

    /// The button was pressed: add the listing, or open its page.
    fn go_ahead(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Stage::Found(listing) = &self.stage else {
            return;
        };
        if self.adding || !self.armed() {
            return;
        }
        let Some(chat) = self.chat.upgrade() else {
            return;
        };
        let listing = listing.clone();
        if self.taken {
            window.close_dialog(cx);
            return chat.update(cx, |chat, cx| {
                chat.show_capability_page(CapabilityTab::Mcp, window, cx)
            });
        }
        if let Way::Page(page) = listing.way {
            window.close_dialog(cx);
            return chat.update(cx, |chat, cx| {
                chat.show_listing_page(page, &listing.id, window, cx)
            });
        }
        let Some(given) = self.given(cx) else {
            return;
        };
        self.adding = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let signs_in = matches!(
                listing.way,
                Way::Server {
                    key: Key::SignIn,
                    ..
                }
            );
            let added = cx
                .background_executor()
                .spawn(async move { marketplace::add(&listing, &given) })
                .await;
            let said = this
                .update_in(cx, |this, window, cx| {
                    this.adding = false;
                    this.error = added.as_ref().err().cloned();
                    if added.is_ok() {
                        // Not left in a field that is about to go.
                        this.token
                            .update(cx, |input, cx| input.set_value("", window, cx));
                        window.close_dialog(cx);
                    }
                    cx.notify();
                })
                .is_ok();
            let _ = cx.update(|window, cx| {
                chat.update(cx, |chat, cx| match added {
                    Ok(added) => chat.show_added(added, signs_in, window, cx),
                    // The dialog went, with its window say, before it could
                    // say why: the window says it instead.
                    Err(why) if !said => {
                        chat.error = Some(why);
                        cx.notify();
                    }
                    Err(_) => {}
                })
            });
        })
        .detach();
    }
}

/// The dialog around `sheet`, as it is built each time it is drawn.
fn dialog_of(sheet: Entity<LinkSheet>) -> impl Fn(Dialog, &mut Window, &mut App) -> Dialog {
    move |dialog, _, cx| {
        let adding = sheet.read(cx).adding;
        let closing = sheet.clone();
        dialog
            .title(div().font_family(HEADING_FONT).child("Add to Letronna"))
            .w(px(520.))
            // Neither a click beside it nor Enter answers for the user:
            // Enter does nothing here, and Add is pressed.
            .overlay_closable(false)
            .on_ok(|_, _, _| false)
            // And Escape or the cross does not leave an add under way.
            .close_button(!adding)
            .on_cancel(move |_, _, cx| !closing.read(cx).adding)
            .child(sheet.clone())
    }
}

/// An address, a command or a place, set apart and whole.
fn exact(text: String, cx: &App) -> Div {
    div()
        .px_2()
        .py_1p5()
        .rounded(cx.theme().radius)
        .bg(cx.theme().secondary)
        .font_family(MONO_FONT)
        .text_xs()
        .child(text)
}

impl Render for LinkSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let close = |label: &'static str, cx: &mut Context<Self>| {
            Button::new("link-close")
                .plain(cx)
                .small()
                .label(label)
                .on_click(|_, window, cx| window.close_dialog(cx))
        };
        let listing = match &self.stage {
            Stage::Looking => {
                return v_flex()
                    .gap_4()
                    .child(
                        h_flex()
                            .gap_2()
                            .text_sm()
                            .text_color(muted)
                            .child(crate::ui::busy::Spinner::new().xsmall())
                            .child("Looking it up on letronna.ai…"),
                    )
                    .child(h_flex().justify_end().child(close("Cancel", cx)));
            }
            Stage::Refused(why) => {
                return v_flex()
                    .gap_4()
                    .child(div().text_sm().child(why.clone()))
                    .child(h_flex().justify_end().child(close("Close", cx)));
            }
            Stage::Found(listing) => listing,
        };
        let by = match listing.maker.as_str() {
            "" => kind_word(listing.kind).to_owned(),
            maker => format!("{} · by {maker}", kind_word(listing.kind)),
        };
        let mut body = v_flex().gap_3().child(
            v_flex()
                .gap_0p5()
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .child(listing.name.clone()),
                )
                .child(div().text_xs().text_color(muted).child(by)),
        );
        if !listing.line.is_empty() {
            body = body.child(div().text_sm().child(listing.line.clone()));
        }
        let taken = self.taken;
        if taken {
            let name = match &listing.way {
                Way::Server { name, .. } | Way::Command { name, .. } => name.as_str(),
                _ => listing.name.as_str(),
            };
            body = body.child(div().text_sm().child(format!(
                "A server called {name} is in mcp.json already, so there is nothing to add. It \
                 is under Capabilities › MCP."
            )));
        } else {
            for line in said(listing, self.folder.as_deref()) {
                body = body.child(match line {
                    Said::Words(text) => div().text_sm().child(text),
                    Said::Exact(text) => exact(text, cx),
                });
            }
        }
        match &listing.way {
            Way::Server {
                key: Key::Token(made_at),
                ..
            } if !taken => {
                let made_at = made_at.clone().map(|at| {
                    Button::new("link-token-page")
                        .plain(cx)
                        .small()
                        .label("Make a token")
                        .tooltip(at.clone())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.armed() {
                                cx.open_url(&at);
                            }
                        }))
                });
                body = body.child(
                    h_flex()
                        .gap_2()
                        .child(Input::new(&self.token).small().flex_1())
                        .children(made_at),
                );
            }
            Way::Command { folder: true, .. } if !taken => {
                body = body.child(
                    h_flex().child(
                        Button::new("link-folder")
                            .framed(cx)
                            .small()
                            .label(if self.folder.is_some() {
                                "Choose another folder"
                            } else {
                                "Choose the folder"
                            })
                            .disabled(self.adding)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.choose_folder(window, cx)),
                            ),
                    ),
                );
            }
            _ => {}
        }
        if let Some(why) = &self.error {
            body = body.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(why.clone()),
            );
        }
        let label = match taken {
            true => Some("Show it".to_owned()),
            false => press(&listing.way),
        };
        let ready = self.armed() && !self.adding && (taken || self.given(cx).is_some());
        let adding = self.adding;
        let go = label.map(|label| {
            Button::new("link-add")
                .primary()
                .small()
                .label(if adding {
                    "Adding…".to_owned()
                } else {
                    label
                })
                .loading(adding)
                .disabled(!ready)
                .on_click(cx.listener(|this, _, window, cx| this.go_ahead(window, cx)))
        });
        let leave = if go.is_some() { "Cancel" } else { "Close" };
        body.child(
            h_flex()
                .pt_1()
                .gap_2()
                .justify_end()
                // Once Add is under way it cannot be taken back, so there is
                // no leaving until it has said how it went.
                .child(close(leave, cx).disabled(adding))
                .children(go),
        )
    }
}

impl Chat {
    /// Take the links as they come, each bringing the window forward: this
    /// start's own, one a later start leaves in the file, and on macOS those
    /// the system hands to the app.
    pub(crate) fn watch_links(cx: &mut Context<Self>) {
        use ::notify::Watcher as _;
        if let Some(link) = link_arg().or_else(take_asked) {
            arrived(link);
        }
        let folder = backend::data_dir();
        let _ = std::fs::create_dir_all(&folder);
        let mut watcher =
            ::notify::recommended_watcher(move |event: ::notify::Result<::notify::Event>| {
                let ours = event.is_ok_and(|event| {
                    (event.paths.iter()).any(|path| path.file_name().is_some_and(|n| n == ASKED))
                });
                if ours && let Some(link) = take_asked() {
                    arrived(link);
                }
            })
            .ok();
        // Without a watch the links of this start and of the system still come.
        if let Some(watching) = watcher.as_mut()
            && watching
                .watch(&folder, ::notify::RecursiveMode::NonRecursive)
                .is_err()
        {
            watcher = None;
        }
        let coming = links().1.clone();
        cx.spawn(async move |_, cx| {
            // Kept for as long as links are taken: dropped, it stops watching.
            let _watcher = watcher;
            while let Ok(link) = coming.recv().await {
                cx.update(|cx| crate::platform::tray::open_link(link, cx));
            }
        })
        .detach();
    }

    /// Open the dialog for a link. What it names is looked up; nothing else
    /// happens until Add is pressed there.
    pub(crate) fn open_link(&mut self, link: &str, window: &mut Window, cx: &mut Context<Self>) {
        // One at a time, and never over another dialog: what the user is
        // reading, or about to press, stays what it is.
        if window.has_active_dialog(cx) {
            if !sheet_open(cx) {
                window.push_notification(
                    Notification::info(
                        "A link for Letronna came while this was open. Close it, then open the \
                         link again.",
                    ),
                    cx,
                );
            }
            return;
        }
        let (stage, id) = match marketplace::id_of(link) {
            Ok(id) => (Stage::Looking, Some(id)),
            Err(why) => (Stage::Refused(why), None),
        };
        let chat = cx.entity().downgrade();
        let sheet = cx.new(|cx| LinkSheet::new(chat, stage, window, cx));
        if let Some(id) = id {
            sheet.update(cx, |sheet, cx| sheet.look_up(id, window, cx));
        }
        cx.set_global(Open(sheet.downgrade()));
        window.open_dialog(cx, dialog_of(sheet));
    }

    /// Go to a tab of Capabilities, from wherever the window was.
    fn show_capability_page(
        &mut self,
        tab: CapabilityTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_open {
            self.close_settings(window, cx);
        }
        self.page = Page::Capabilities;
        self.show_capability(tab, window, cx);
    }

    /// Go to where what was just added is listed, and say what was done.
    fn show_added(
        &mut self,
        added: Added,
        signs_in: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match added {
            Added::Server(name) => {
                self.show_capability_page(CapabilityTab::Mcp, window, cx);
                let next = match signs_in {
                    true => " Sign in on its row once it asks.",
                    false => "",
                };
                self.capabilities.server_note =
                    Some(Ok(format!("Added {name}; it is starting below.{next}")));
            }
            Added::Skills(installed) => {
                self.show_capability_page(CapabilityTab::Skills, window, cx);
                self.skills_note = Some(Ok(crate::ui::skills::install_note(&installed)));
                self.skills_refused = installed.refused;
            }
        }
        cx.notify();
    }

    /// Go to the page something that comes with Letronna is set up on, with
    /// it picked where the page has a list to pick from.
    fn show_listing_page(
        &mut self,
        page: marketplace::Page,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_open {
            self.close_settings(window, cx);
        }
        match page {
            marketplace::Page::Plugins => self.open_page(Page::Plugins, window, cx),
            marketplace::Page::Messaging => {
                if let Some(channel) = crate::ui::messaging::channel_named(id) {
                    self.channel = channel;
                }
                self.open_page(Page::Messaging, window, cx);
            }
            marketplace::Page::Skills => {
                self.show_capability_page(CapabilityTab::Skills, window, cx)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{
        ARMED_AFTER, ASKED, LinkSheet, Said, Stage, ask_in, command_line, dialog_of, first_link,
        is_link, press, said, take_in,
    };
    use gpui::{
        AppContext as _, Context, Entity, IntoElement, Render, Styled as _, TestAppContext,
        VisualTestContext, WeakEntity, Window, div,
    };
    use gpui_component::{Root, WindowExt as _};
    use letronna_core::marketplace::{self, Listing, Way};
    use std::path::Path;
    use std::time::Instant;

    /// What the dialog is over.
    struct Behind;

    impl Render for Behind {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full()
        }
    }

    /// A window with a link's dialog open on a server that signs in, found
    /// and past the wait before Add can be pressed.
    fn opened(cx: &mut TestAppContext) -> (Entity<LinkSheet>, &mut VisualTestContext) {
        cx.update(gpui_component::init);
        let behind = cx.new(|_| Behind);
        let (_, cx) = cx.add_window_view(|window, cx| Root::new(behind, window, cx));
        let found = listing(serde_json::json!({
            "id": "thing", "name": "Thing", "kind": "mcp", "way": "login",
            "server": "thing", "url": "https://mcp.example.com/mcp",
        }));
        let sheet = cx.update(|window, cx| {
            let sheet = cx.new(|cx| {
                let mut sheet =
                    LinkSheet::new(WeakEntity::new_invalid(), Stage::Found(found), window, cx);
                sheet.shown = Instant::now() - ARMED_AFTER;
                sheet
            });
            window.open_dialog(cx, dialog_of(sheet.clone()));
            sheet
        });
        cx.run_until_parked();
        (sheet, cx)
    }

    fn open(cx: &mut VisualTestContext) -> bool {
        cx.run_until_parked();
        cx.update(|window, cx| window.has_active_dialog(cx))
    }

    #[gpui::test]
    fn enter_neither_adds_nor_closes_and_escape_does_not_leave_an_add_under_way(
        cx: &mut TestAppContext,
    ) {
        let (sheet, cx) = opened(cx);
        assert!(open(cx));
        assert!(cx.read(|cx| sheet.read(cx).armed()));
        // Enter is the dialog's own key for yes: here it is nobody's.
        cx.simulate_keystrokes("enter");
        assert!(open(cx));
        assert!(!cx.read(|cx| sheet.read(cx).adding));
        // An add under way cannot be taken back, so there is no leaving it.
        cx.update(|_, cx| sheet.update(cx, |sheet, _| sheet.adding = true));
        cx.simulate_keystrokes("escape");
        assert!(open(cx));
        // Once it has ended, Escape leaves as it does any dialog: so the
        // keys above did reach the dialog.
        cx.update(|_, cx| sheet.update(cx, |sheet, _| sheet.adding = false));
        cx.simulate_keystrokes("escape");
        assert!(!open(cx));
    }

    /// A listing of a list holding only it.
    fn listing(fields: serde_json::Value) -> Listing {
        let list = serde_json::json!({"version": 1, "listings": [fields]}).to_string();
        marketplace::listed(list.as_bytes(), "thing").unwrap()
    }

    fn args(words: &[&str]) -> impl Iterator<Item = String> {
        words
            .iter()
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>()
            .into_iter()
    }

    /// The lines shown whole and as they will be used.
    fn exact(lines: &[Said]) -> Vec<&str> {
        lines
            .iter()
            .filter_map(|line| match line {
                Said::Exact(text) => Some(text.as_str()),
                Said::Words(_) => None,
            })
            .collect()
    }

    #[test]
    fn a_start_given_a_link_takes_it_and_reads_nothing_after_it() {
        let link = "letronna://marketplace/linear";
        assert_eq!(first_link(args(&[link])).as_deref(), Some(link));
        // Whatever follows a link is the link's writer's, not a flag.
        assert_eq!(
            first_link(args(&[link, "--uninstall"])).as_deref(),
            Some(link)
        );
        // A link is only ever the first argument; a flag of ours never is one.
        assert_eq!(first_link(args(&["--session", "12", link])), None);
        assert_eq!(first_link(args(&["--relaunch"])), None);
        assert_eq!(first_link(args(&[])), None);
        // Any spelling of the scheme is a stranger's, to be refused in the
        // dialog rather than read as something else.
        for other in ["LETRONNA://x", "letronna:x", "Letronna://pair?s=1"] {
            assert!(is_link(other), "{other}");
        }
        for not in [
            "",
            "letronna",
            "--letronna://x",
            "https://letronna.ai",
            "létronna:",
        ] {
            assert!(!is_link(not), "{not}");
        }
    }

    /// A folder of this test's own, empty.
    fn scratch(name: &str) -> std::path::PathBuf {
        let folder =
            std::env::temp_dir().join(format!("letronna-link-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    #[test]
    fn a_link_left_for_the_window_is_taken_once_and_holds_no_more_than_an_id() {
        let folder = scratch("handed");
        ask_in(&folder, "letronna://marketplace/linear");
        // Nothing is left beside the file once it is in place.
        let left: Vec<_> = std::fs::read_dir(&folder).unwrap().flatten().collect();
        assert_eq!(left.len(), 1);
        assert_eq!(
            take_in(&folder).as_deref(),
            Some("letronna://marketplace/linear")
        );
        assert_eq!(take_in(&folder), None);
        // A pairing link's secret, and a payload, never reach the disk.
        ask_in(&folder, "letronna://pair?v=1&k=a-key&s=a-secret");
        assert_eq!(
            std::fs::read_to_string(folder.join(ASKED)).unwrap(),
            "letronna://pair"
        );
        ask_in(&folder, "letronna://marketplace/x?command=sh");
        assert_eq!(take_in(&folder).as_deref(), Some("letronna://"));
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn a_link_left_long_ago_or_not_in_a_plain_file_is_removed_unread() {
        let folder = scratch("stale");
        ask_in(&folder, "letronna://marketplace/linear");
        let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(120);
        std::fs::File::options()
            .write(true)
            .open(folder.join(ASKED))
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        assert_eq!(take_in(&folder), None);
        assert!(!folder.join(ASKED).exists());
        // More than a link's worth is cut short, and so reads as no listing.
        std::fs::write(folder.join(ASKED), "x".repeat(100_000)).unwrap();
        assert!(take_in(&folder).is_some_and(|text| text.len() <= 256));
        #[cfg(unix)]
        {
            let elsewhere = folder.join("elsewhere");
            std::fs::write(&elsewhere, "letronna://marketplace/linear").unwrap();
            std::os::unix::fs::symlink(&elsewhere, folder.join(ASKED)).unwrap();
            assert_eq!(take_in(&folder), None);
            assert!(!folder.join(ASKED).exists() && elsewhere.exists());
        }
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn the_dialog_shows_the_address_the_entry_will_hold() {
        let server = listing(serde_json::json!({
            "id": "thing", "name": "Thing", "kind": "mcp", "maker": "Makers", "way": "login",
            "server": "thing", "url": "https://mcp.example.com/mcp",
        }));
        let lines = said(&server, None);
        let (_, entry) = marketplace::entry(&server.way, "").unwrap();
        assert_eq!(exact(&lines), [entry["url"].as_str().unwrap()]);
        assert!(matches!(&lines[0], Said::Words(text) if text.contains("called thing")));
        assert_eq!(press(&server.way).as_deref(), Some("Add"));
    }

    #[test]
    fn the_dialog_shows_the_command_that_will_run_folder_and_all() {
        let command = listing(serde_json::json!({
            "id": "thing", "name": "Thing", "kind": "mcp", "maker": "Makers", "way": "command",
            "server": "files", "command": ["npx", "-y", "@scope/server"], "asks": "folder",
        }));
        assert_eq!(
            exact(&said(&command, None)),
            ["npx -y @scope/server <the folder you choose>"]
        );
        let folder = std::env::temp_dir();
        let lines = said(&command, Some(&folder));
        let (_, entry) = marketplace::entry(&command.way, folder.to_str().unwrap()).unwrap();
        let run: Vec<&str> = std::iter::once(entry["command"].as_str().unwrap())
            .chain(
                entry["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|a| a.as_str().unwrap()),
            )
            .collect();
        assert_eq!(exact(&lines), [run.join(" ")]);
        assert_eq!(
            command_line(
                &["uvx".to_owned(), "thing".to_owned()],
                Some(Path::new("/x")),
                false
            ),
            "uvx thing"
        );
    }

    #[test]
    fn what_comes_with_letronna_opens_its_page_and_an_editor_has_no_button() {
        let skill = listing(serde_json::json!({
            "id": "thing", "name": "Thing", "kind": "skill", "way": "skill",
            "skill": "https://github.com/owner/repository/tree/main/skills/thing",
        }));
        assert_eq!(
            exact(&said(&skill, None)),
            ["https://github.com/owner/repository/tree/main/skills/thing"]
        );
        assert_eq!(press(&skill.way).as_deref(), Some("Add"));
        let pages = [
            ("plugin", "signin", "Open Plugins"),
            ("plugin", "switch", "Open Plugins"),
            ("chat", "messaging", "Open Messaging"),
            ("skill", "ships", "Open Capabilities › Skills"),
        ];
        for (kind, way, label) in pages {
            let built_in = listing(serde_json::json!({
                "id": "thing", "name": "Thing", "kind": kind, "way": way,
            }));
            assert!(matches!(built_in.way, Way::Page(_)));
            assert_eq!(press(&built_in.way).as_deref(), Some(label));
            assert!(exact(&said(&built_in, None)).is_empty());
        }
        let editor = listing(serde_json::json!({
            "id": "thing", "name": "Thing", "kind": "editor", "way": "editor",
        }));
        assert_eq!(press(&editor.way), None);
        assert!(
            matches!(&said(&editor, None)[..], [Said::Words(text)] if text.contains("Nothing was changed"))
        );
    }
}
