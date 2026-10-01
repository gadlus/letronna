#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod app;
mod assets;
mod highlight;
mod platform;
mod plugins;
mod prelude;
mod theme;
mod ui;

use prelude::*;

/// Every key the window answers, each with what it does as the key list
/// (F1) shows it; a binding with no words is left off the list.
pub(crate) fn keys(mapper: &dyn PlatformKeyboardMapper) -> Vec<(KeyBinding, Option<&'static str>)> {
    use ui::{
        actions as turn, approval, capabilities, changes, complete, composer, files, import,
        sessions,
    };
    let mut keys = vec![
        // "secondary" is cmd on macOS and ctrl everywhere else.
        (
            KeyBinding::new("secondary-n", NewSession, None),
            Some("Start a new session"),
        ),
        (
            KeyBinding::new("secondary-b", ToggleSidebar, None),
            Some("Show or hide the sidebar"),
        ),
        (
            KeyBinding::new("secondary-,", OpenSettings, None),
            Some("Open settings"),
        ),
        // Scoped to the chat rather than global: gpui ranks a binding
        // with no context above every other, which took escape away
        // from dialogs and menus. Inside the chat, theirs go first.
        (
            KeyBinding::new("escape", Dismiss, Some("Chat")),
            Some("Stop the reply, or close what is open"),
        ),
        (
            KeyBinding::new("secondary-j", ui::panel::TogglePanel, Some("Chat")),
            Some("Show or hide the panel"),
        ),
        (
            KeyBinding::new("secondary-shift-l", ui::panel::BrowserTab, Some("Chat")),
            Some("Open the browser"),
        ),
        (
            KeyBinding::new("secondary-shift-e", files::FilesTab, Some("Chat")),
            Some("Open the files"),
        ),
        (
            KeyBinding::new("secondary-shift-u", ui::usage::OpenUsage, Some("Chat")),
            Some("Open usage"),
        ),
        (
            KeyBinding::new("secondary-shift-a", OpenActivity, Some("Chat")),
            Some("Open activity"),
        ),
        (
            KeyBinding::new("secondary-k", ui::palette::OpenPalette, Some("Chat")),
            Some("Open the command palette"),
        ),
        (
            KeyBinding::new(
                "secondary-shift-k",
                capabilities::OpenCapabilities,
                Some("Chat"),
            ),
            Some("Open capabilities"),
        ),
        (
            KeyBinding::new(
                "ctrl-tab",
                capabilities::NextCapabilityTab,
                Some("Capabilities"),
            ),
            Some("In capabilities: the next tab"),
        ),
        (
            KeyBinding::new(
                "ctrl-shift-tab",
                capabilities::PrevCapabilityTab,
                Some("Capabilities"),
            ),
            Some("In capabilities: the tab before"),
        ),
        (
            KeyBinding::new("secondary-shift-d", changes::ChangesTab, Some("Chat")),
            Some("Open the changes"),
        ),
        (
            KeyBinding::new("f6", NextRegion, Some("Chat")),
            Some("Move to the next part of the window"),
        ),
        (
            KeyBinding::new("shift-f6", PreviousRegion, Some("Chat")),
            Some("Move to the previous part of the window"),
        ),
        (
            KeyBinding::new("secondary-l", FocusComposer, Some("Chat")),
            Some("Write a message"),
        ),
        (
            KeyBinding::new("f1", ShowKeys, Some("Chat")),
            Some("List these keys"),
        ),
        // Only while the box is empty; the handler hands the key back
        // otherwise, so a question mark still types.
        (
            KeyBinding::new("?", composer::KeysOrType, Some("Composer > Input")),
            None,
        ),
        (
            KeyBinding::new("shift-?", composer::KeysOrType, Some("Composer > Input")),
            None,
        ),
        (
            KeyBinding::new(
                "shift-enter",
                gpui_component::input::Enter { secondary: true },
                Some("Input"),
            ),
            Some("A new line in the message"),
        ),
        // Bound after the input's own Ctrl+Enter, a new line, so it is
        // tried first and can hand the key back when there is nothing
        // to send now.
        (
            KeyBinding::new("secondary-enter", SendNow, Some("Composer > Input")),
            Some("Send now, into the reply that is running"),
        ),
        (
            KeyBinding::new(
                "secondary-shift-v",
                ui::voice::ToggleVoice,
                Some("Composer > Input"),
            ),
            Some("Start or stop speaking a message"),
        ),
        // On the browser's page, Tab moves through the page, not the
        // window.
        (
            KeyBinding::new("tab", ui::browser::PageTab, Some("Browser")),
            None,
        ),
        (
            KeyBinding::new("shift-tab", ui::browser::PageTabBack, Some("Browser")),
            None,
        ),
        // Escape cancels Point mode here, nearer than Chat's own escape,
        // which stops the reply.
        (
            KeyBinding::new("escape", ui::browser::CancelPoint, Some("Browser")),
            None,
        ),
        (
            KeyBinding::new("tab", composer::LeaveComposer, Some("Composer > Input")),
            None,
        ),
        (
            KeyBinding::new("enter", ui::search::OpenHit, Some("SearchHit")),
            None,
        ),
        (
            KeyBinding::new("alt-up", turn::PreviousTurn, Some("Chat")),
            Some("Step to the turn before"),
        ),
        (
            KeyBinding::new("alt-down", turn::NextTurn, Some("Chat")),
            Some("Step to the turn after"),
        ),
        // A turn's keys act only while the turn itself has the focus, and
        // hand the key back from the box a message is changed in.
        (
            KeyBinding::new("c", turn::CopyTurn, Some("Turn")),
            Some("On a turn: copy it"),
        ),
        (
            KeyBinding::new("e", turn::EditTurn, Some("Turn")),
            Some("On your message: change it"),
        ),
        (
            KeyBinding::new("r", turn::RetryTurn, Some("Turn")),
            Some("On a turn: ask it again"),
        ),
        (
            KeyBinding::new("w", turn::RewindTurn, Some("Turn")),
            Some("On a turn: rewind to it"),
        ),
        (
            KeyBinding::new("f", turn::ForkTurn, Some("Turn")),
            Some("On a turn: fork from it"),
        ),
        (
            KeyBinding::new("enter", turn::TurnMenu, Some("Turn")),
            Some("On a turn: its menu"),
        ),
        // The sidebar's own, as its filters below are, and like a turn's
        // they act only while the sidebar itself has the focus.
        (
            KeyBinding::new("up", sessions::PreviousSession, Some("Sessions")),
            Some("In the sidebar: the session above"),
        ),
        (
            KeyBinding::new("down", sessions::NextSession, Some("Sessions")),
            Some("In the sidebar: the session below"),
        ),
        (
            KeyBinding::new("enter", sessions::OpenSession, Some("Sessions")),
            Some("In the sidebar: open the session"),
        ),
        (
            KeyBinding::new("p", sessions::PinSession, Some("Sessions")),
            Some("In the sidebar: pin or unpin it"),
        ),
        (
            KeyBinding::new("f2", sessions::RenameSession, Some("Sessions")),
            Some("In the sidebar: rename it"),
        ),
        (
            KeyBinding::new("delete", sessions::DeleteSession, Some("Sessions")),
            Some("In the sidebar: delete it, once you say so"),
        ),
        // The sidebar's own filters, so a shortcut typed elsewhere is
        // never swallowed by one narrowing a list it cannot see.
        (
            KeyBinding::new(
                "secondary-shift-g",
                sessions::ToggleGrouping,
                Some("Sessions"),
            ),
            Some("In the sidebar: group the sessions by project, or not"),
        ),
        // The Files tab: the arrows and Enter walk the tree while it
        // has the keys, and the rest act on its picked row from
        // anywhere in the tab.
        (
            KeyBinding::new("up", files::FileUp, Some("FileTree")),
            Some("In the files: the row above"),
        ),
        (
            KeyBinding::new("down", files::FileDown, Some("FileTree")),
            Some("In the files: the row below"),
        ),
        (
            KeyBinding::new("left", files::FileLeft, Some("FileTree")),
            Some("In the files: close the folder, or go up"),
        ),
        (
            KeyBinding::new("right", files::FileRight, Some("FileTree")),
            Some("In the files: open the folder"),
        ),
        (
            KeyBinding::new("enter", files::OpenFile, Some("FileTree")),
            Some("In the files: open or close a folder, or show a file"),
        ),
        (
            KeyBinding::new("secondary-enter", files::AddToMessage, Some("Files")),
            Some("In the files: add it to the message"),
        ),
        (
            KeyBinding::new("secondary-o", files::OpenInEditor, Some("Files")),
            Some("In the files: open it in the editor"),
        ),
        (
            KeyBinding::new("secondary-shift-o", files::OpenWithSystem, Some("Files")),
            Some("In the files: open it with the system's program"),
        ),
        (
            KeyBinding::new("secondary-shift-r", files::ShowInFolder, Some("Files")),
            Some("In the files: show it in its folder"),
        ),
        (
            KeyBinding::new("secondary-shift-c", files::CopyPath, Some("Files")),
            Some("In the files: copy its path"),
        ),
        (
            KeyBinding::new("secondary-i", files::ToggleIgnored, Some("Files")),
            Some("In the files: show or hide ignored files"),
        ),
        (
            KeyBinding::new("escape", files::ClosePreview, Some("Files")),
            Some("In the files: close the preview"),
        ),
        // Bound after the input's own, and so tried first: the picker's
        // box keeps the keys while the list moves under it.
        (
            KeyBinding::new("up", import::PickPrevious, Some("ImportPicker > Input")),
            None,
        ),
        (
            KeyBinding::new("down", import::PickNext, Some("ImportPicker > Input")),
            None,
        ),
        (
            KeyBinding::new("enter", import::PickThis, Some("ImportPicker > Input")),
            None,
        ),
        // Last, so they come before the input's own keys and the Tab
        // above while the composer's list is up, and only then.
        (
            KeyBinding::new("up", complete::ChoiceUp, Some("Completing > Input")),
            None,
        ),
        (
            KeyBinding::new("down", complete::ChoiceDown, Some("Completing > Input")),
            None,
        ),
        (
            KeyBinding::new("enter", complete::TakeChoice, Some("Completing > Input")),
            None,
        ),
        (
            KeyBinding::new("tab", complete::OpenChoice, Some("Completing > Input")),
            None,
        ),
        (
            KeyBinding::new("escape", complete::CloseChoices, Some("Completing > Input")),
            None,
        ),
        // The Changes tab's list, once it has the keys.
        (
            KeyBinding::new("down", changes::NextChange, Some("Changes")),
            Some("In the changes: the file below"),
        ),
        (
            KeyBinding::new("up", changes::PreviousChange, Some("Changes")),
            Some("In the changes: the file above"),
        ),
        (
            KeyBinding::new("enter", changes::OpenChange, Some("Changes")),
            Some("In the changes: show or hide its diff"),
        ),
        (
            KeyBinding::new("s", changes::SwitchScope, Some("Changes")),
            Some("In the changes: this session's, or all since the commit"),
        ),
        (
            KeyBinding::new("a", changes::AddChange, Some("Changes")),
            Some("In the changes: name it in the message"),
        ),
        (
            KeyBinding::new("r", changes::RevertChange, Some("Changes")),
            Some("In the changes: undo this session's edits to it"),
        ),
        (
            KeyBinding::new("secondary-shift-j", ui::jobs::JobsTab, Some("Chat")),
            Some("Show the Jobs tab"),
        ),
        (
            KeyBinding::new("down", ui::jobs::NextJob, Some("JobList")),
            Some("In the jobs: the next job"),
        ),
        (
            KeyBinding::new("up", ui::jobs::PreviousJob, Some("JobList")),
            Some("In the jobs: the job before"),
        ),
        (
            KeyBinding::new("down", ui::jobs::LineDown, Some("JobLog")),
            None,
        ),
        (
            KeyBinding::new("up", ui::jobs::LineUp, Some("JobLog")),
            None,
        ),
        (
            KeyBinding::new("shift-down", ui::jobs::PickDown, Some("JobLog")),
            Some("In a job's output: pick lines down"),
        ),
        (
            KeyBinding::new("shift-up", ui::jobs::PickUp, Some("JobLog")),
            Some("In a job's output: pick lines up"),
        ),
        (
            KeyBinding::new("secondary-a", ui::jobs::PickAll, Some("JobLog")),
            Some("In a job's output: pick it all"),
        ),
        (
            KeyBinding::new("escape", ui::jobs::Unpick, Some("JobLog")),
            None,
        ),
        (
            KeyBinding::new("enter", ui::jobs::AddLines, Some("JobLog")),
            Some("In a job's output: add the picked lines to the message"),
        ),
        // Saves the Memory page's box with focus; nothing else there has a
        // save key.
        (
            KeyBinding::new("secondary-s", ui::memory::Save, Some("MemoryPage")),
            Some("On the Memory page: save"),
        ),
        // On the Workflows page, Escape leaves an open run for the list, and
        // the refresh key reads the runs again.
        (
            KeyBinding::new("escape", ui::workflows::Back, Some("Workflows")),
            Some("On the Workflows page: back to the list"),
        ),
        (
            KeyBinding::new("secondary-r", ui::workflows::Refresh, Some("Workflows")),
            Some("On the Workflows page: read the runs again"),
        ),
        (
            KeyBinding::new("enter", ui::actions::ToggleDiff, Some("ChangedFile")),
            Some("On a changed file under a turn: show or hide its diff"),
        ),
        (
            KeyBinding::new("enter", ui::messages::ToggleRun, Some("ToolRun")),
            Some("On a folded run of tool calls: open or close it"),
        ),
        (
            KeyBinding::new("escape", changes::LeaveChanges, Some("Changes")),
            Some("In the changes: back to the composer"),
        ),
        (
            KeyBinding::new("space", changes::ToggleStage, Some("Changes")),
            Some("In the changes: stage or unstage the file"),
        ),
        (
            KeyBinding::new("]", changes::NextHunk, Some("Changes")),
            Some("In the changes: the next hunk"),
        ),
        (
            KeyBinding::new("[", changes::PreviousHunk, Some("Changes")),
            Some("In the changes: the hunk before"),
        ),
        (
            KeyBinding::new("h", changes::ToggleHunk, Some("Changes")),
            Some("In the changes: stage or unstage the hunk"),
        ),
        (
            KeyBinding::new("m", changes::WriteMessage, Some("Changes")),
            Some("In the changes: write the commit message"),
        ),
        (
            KeyBinding::new("g", changes::SuggestMessage, Some("Changes")),
            Some("In the changes: have the model write the message"),
        ),
        // In the commit message; bound after the input's own keys, so
        // these are tried first.
        (
            KeyBinding::new(
                "secondary-enter",
                changes::CommitStaged,
                Some("CommitMessage > Input"),
            ),
            Some("In the commit message: commit what is staged"),
        ),
        (
            KeyBinding::new(
                "secondary-shift-enter",
                changes::CommitAndPush,
                Some("CommitMessage > Input"),
            ),
            Some("In the commit message: commit, then push"),
        ),
        (
            KeyBinding::new(
                "escape",
                changes::LeaveMessage,
                Some("CommitMessage > Input"),
            ),
            None,
        ),
    ];
    // Windows hands Shift and a digit over as the character they type,
    // Ctrl+Shift+4 as "ctrl-$", which a binding written "shift-4" never
    // meets. The platform's keyboard mapper binds the character the
    // layout in use puts on that key; elsewhere it leaves them as written.
    let in_sidebar = KeyBindingContextPredicate::parse("Sessions")
        .ok()
        .map(std::rc::Rc::new);
    let typed = |keys: &str, action: Box<dyn Action>, about: &'static str| {
        KeyBinding::load(keys, action, in_sidebar.clone(), false, None, mapper)
            .ok()
            .map(|binding| (binding, Some(about)))
    };
    keys.extend(
        [
            typed(
                "secondary-shift-1",
                Box::new(sessions::FilterNeedsYou),
                "In the sidebar: Needs you only, or all",
            ),
            typed(
                "secondary-shift-2",
                Box::new(sessions::FilterWorking),
                "In the sidebar: Working only, or all",
            ),
            typed(
                "secondary-shift-3",
                Box::new(sessions::FilterUnseen),
                "In the sidebar: Unseen only, or all",
            ),
            typed(
                "secondary-shift-4",
                Box::new(sessions::FilterPinned),
                "In the sidebar: Pinned only, or all",
            ),
        ]
        .into_iter()
        .flatten(),
    );
    // One binding per letter, so the list says it once.
    keys.extend(('a'..='z').enumerate().map(|(n, letter)| {
        (
            KeyBinding::new(
                &letter.to_string(),
                approval::PickOption(n),
                Some("Question"),
            ),
            (n == 0).then_some("On a question: pick the answer with that letter"),
        )
    }));
    keys
}

fn main() {
    // The first mark, from which the others are timed.
    letronna_core::diag::mark("start");
    // Asked by the updater and the release build before a new binary is
    // trusted. First, because a release build that went on would install
    // itself over the Letronna already there.
    if std::env::args().any(|a| a == "--version") {
        println!("{BRAND} {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if std::env::args().any(|a| a == "--uninstall") {
        platform::install::uninstall();
    }
    if platform::install::ensure_installed() || !platform::install::single_instance() {
        return;
    }
    // An update downloaded last time goes in now, before anything is running,
    // and so does the old version when a new one keeps failing to start.
    if let Some(program) = letronna_core::update::on_start() {
        let _ = std::process::Command::new(program)
            .arg("--relaunch")
            .spawn();
        return;
    }
    // Before anything reads a key, so each comes from the vault and any
    // still in a file is moved there.
    letronna_core::vault::open();
    letronna_core::diag::init("gui");
    Application::new().with_assets(Assets).run(|cx| {
        letronna_core::diag::mark("app");
        gpui_component::init(cx);
        highlight::register();
        plugins::init(
            cx,
            vec![
                Box::new(plugins::discord::DiscordPresence::new(
                    plugins::PresenceMode::Detailed,
                )),
                Box::new(plugins::hello::Hello::new()),
            ],
        );
        // MCP servers can take seconds to start; started with the window,
        // their tools are there by the first message.
        letronna_core::mcp::start();
        cx.bind_keys(
            keys(cx.keyboard_mapper().as_ref())
                .into_iter()
                .map(|(binding, _)| binding),
        );
        ui::quick::init(cx);
        platform::tray::init(cx);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        // Background jobs are children of this process; do not outlive it.
        // Nor does a browser with no window, which nobody could close. The
        // schedules lock goes too, so a terminal can run them at once.
        cx.on_app_quit(|_| {
            letronna_core::jobs::stop_all();
            letronna_core::mcp::stop_all();
            letronna_core::lsp::stop_all();
            letronna_core::browser::close_hidden();
            letronna_core::schedule::stop();
            letronna_core::diag::stop();
            async {}
        })
        .detach();
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                        None,
                        size(px(1100.), px(760.)),
                        cx,
                    ))),
                    window_min_size: Some(size(px(720.), px(520.))),
                    titlebar: Some(TitlebarOptions {
                        title: Some(BRAND.into()),
                        // Centre the 12px traffic lights in the 34px bar, at
                        // the inset a stock macOS window gives them.
                        traffic_light_position: Some(point(px(12.), px(11.))),
                        ..TitleBar::title_bar_options()
                    }),
                    ..Default::default()
                },
                |window, cx| {
                    let chat = cx.new(|cx| Chat::new(window, cx));
                    // Shared with the quick entry window, so a prompt sent
                    // from the bar lands in this same store.
                    cx.set_global(ui::quick::MainChat(chat.clone()));
                    cx.new(|cx| Root::new(chat, window, cx))
                },
            )
            .expect("Could not open native application window");
        // Opened with the chat built; its first frame comes on the next
        // refresh of the display.
        letronna_core::diag::mark("window");
        let hotkey_spec = cx
            .global::<ui::quick::MainChat>()
            .0
            .read(cx)
            .store
            .settings
            .quick_entry_hotkey
            .clone();
        platform::hotkey::init(&hotkey_spec, cx);
        // With a tray to bring it back, closing the window tucks the app
        // away; without one that would strand it, so let it close.
        if platform::tray::available() {
            let _ = window.update(cx, |_, window, cx| {
                window.on_window_should_close(cx, |window, _| {
                    platform::window::hide(window);
                    false
                });
            });
        }
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::keys;

    #[test]
    fn every_key_but_the_window_wide_three_is_bound_in_a_context() {
        // gpui ranks a binding with no context above every other, so a new
        // one would take its key from every dialog, menu and text box.
        let window_wide: Vec<String> = keys(&gpui::DummyKeyboardMapper)
            .iter()
            .filter(|(binding, _)| binding.predicate().is_none())
            .map(|(binding, _)| binding.keystrokes()[0].unparse())
            .collect();
        assert_eq!(window_wide.len(), 3, "{window_wide:?}");
    }
}
