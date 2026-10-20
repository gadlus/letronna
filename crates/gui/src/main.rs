#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod app;
mod assets;
mod highlight;
mod platform;
mod plugins;
mod prelude;
mod runs;
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
            KeyBinding::new("secondary-shift-m", ui::panel::WideBrowser, Some("Chat")),
            Some("Give the browser the most room, or put it back"),
        ),
        (
            KeyBinding::new("secondary-shift-b", ui::panel::PreviewBrowser, Some("Chat")),
            Some("Shrink the browser to a preview, or show it beside the chat"),
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
        // Not Shift+I: off macOS that is gpui-component's inspector in a
        // debug build, bound window-wide and so tried before this.
        (
            KeyBinding::new("secondary-shift-t", ui::activity::ShowRunning, Some("Chat")),
            Some("Show what is running"),
        ),
        // Nearer than the chat's own escape, which would stop the reply.
        (
            KeyBinding::new("escape", ui::activity::LeaveStrip, Some("LiveStrip")),
            Some("In the strip over the composer: close its list"),
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
        // Everywhere else in the chat, Ctrl+Tab lists the sessions used
        // last; the capabilities page's own, nearer, still goes first there.
        (
            KeyBinding::new("ctrl-tab", ui::switcher::SwitchSession, Some("Chat")),
            Some("Switch to a recent session (hold Ctrl, let go to open)"),
        ),
        (
            KeyBinding::new(
                "ctrl-shift-tab",
                ui::switcher::SwitchSessionBack,
                Some("Chat"),
            ),
            None,
        ),
        (
            KeyBinding::new("tab", ui::switcher::NextRecent, Some("Switcher")),
            None,
        ),
        (
            KeyBinding::new("down", ui::switcher::NextRecent, Some("Switcher")),
            None,
        ),
        (
            KeyBinding::new("shift-tab", ui::switcher::PreviousRecent, Some("Switcher")),
            None,
        ),
        (
            KeyBinding::new("up", ui::switcher::PreviousRecent, Some("Switcher")),
            None,
        ),
        (
            KeyBinding::new("enter", ui::switcher::OpenRecent, Some("Switcher")),
            None,
        ),
        (
            KeyBinding::new("escape", ui::switcher::CloseRecent, Some("Switcher")),
            None,
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
                gpui_component::input::Enter {
                    secondary: true,
                    shift: true,
                },
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
        // On the browser's preview, as on a folded run; its Escape is nearer
        // than Chat's too.
        (
            KeyBinding::new(
                "enter",
                ui::panel::ExpandBrowserPreview,
                Some("BrowserPreview"),
            ),
            Some("On the browser's preview: show the page beside the chat"),
        ),
        (
            KeyBinding::new(
                "escape",
                ui::panel::CloseBrowserPreview,
                Some("BrowserPreview"),
            ),
            Some("On the browser's preview: close it"),
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
        (
            KeyBinding::new("up", turn::PreviousTurn, Some("Turn")),
            Some("On a turn: step to the one before"),
        ),
        (
            KeyBinding::new("down", turn::NextTurn, Some("Turn")),
            Some("On a turn: step to the one after"),
        ),
        // The sidebar's own, as its filters below are, and like a turn's
        // they act only while the sidebar itself has the focus.
        (
            KeyBinding::new("up", sessions::PreviousSession, Some("Sessions")),
            Some("In the sidebar: the row above"),
        ),
        (
            KeyBinding::new("down", sessions::NextSession, Some("Sessions")),
            Some("In the sidebar: the row below"),
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
        // A Mac's delete key is Backspace, and Cmd+Backspace moves a file to
        // the Trash in Finder: Delete alone is Fn and the same key there.
        (
            KeyBinding::new("backspace", sessions::DeleteSession, Some("Sessions")),
            None,
        ),
        (
            KeyBinding::new(
                "secondary-backspace",
                sessions::DeleteSession,
                Some("Sessions"),
            ),
            None,
        ),
        // Selecting several, as in a file list; P and Delete then act on
        // all of them.
        (
            KeyBinding::new("space", sessions::ToggleSelected, Some("Sessions")),
            Some("In the sidebar: select it, or no longer, to act on several"),
        ),
        (
            KeyBinding::new("shift-up", sessions::SelectPrevious, Some("Sessions")),
            Some("In the sidebar: select the row above as well"),
        ),
        (
            KeyBinding::new("shift-down", sessions::SelectNext, Some("Sessions")),
            Some("In the sidebar: select the row below as well"),
        ),
        (
            KeyBinding::new("secondary-a", sessions::SelectAll, Some("Sessions")),
            Some("In the sidebar: select every session listed"),
        ),
        (
            KeyBinding::new("escape", sessions::ClearSelection, Some("Sessions")),
            Some("In the sidebar: select nothing"),
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
        // Settings' list of pages, once Tab has put the keys on it.
        (
            KeyBinding::new(
                "up",
                ui::settings::PreviousSettingsPage,
                Some("SettingsNav"),
            ),
            Some("In settings: the page above"),
        ),
        (
            KeyBinding::new("down", ui::settings::NextSettingsPage, Some("SettingsNav")),
            Some("In settings: the page below"),
        ),
        // Settings is modal: Tab and Shift-Tab go round its own controls,
        // nearer than gpui-component's, which go on to the window behind.
        (
            KeyBinding::new("tab", ui::settings::SettingsTab, Some("Settings")),
            None,
        ),
        (
            KeyBinding::new("shift-tab", ui::settings::SettingsTabBack, Some("Settings")),
            None,
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
        // A picture in the chat opens over the window, and its own keys
        // act there, nearer than the dialog's and the chat's.
        (
            KeyBinding::new("enter", ui::viewer::OpenPicture, Some("Picture")),
            Some("On a picture: open it in the window"),
        ),
        (
            KeyBinding::new("+", ui::viewer::ZoomIn, Some("PictureViewer")),
            Some("In a picture: zoom in"),
        ),
        // Windows hands the plus over with the Shift that typed it.
        (
            KeyBinding::new("shift-+", ui::viewer::ZoomIn, Some("PictureViewer")),
            None,
        ),
        (
            KeyBinding::new("=", ui::viewer::ZoomIn, Some("PictureViewer")),
            None,
        ),
        (
            KeyBinding::new("-", ui::viewer::ZoomOut, Some("PictureViewer")),
            Some("In a picture: zoom out"),
        ),
        (
            KeyBinding::new("0", ui::viewer::ZoomFit, Some("PictureViewer")),
            Some("In a picture: fit it to the window"),
        ),
        (
            KeyBinding::new("left", ui::viewer::PanLeft, Some("PictureViewer")),
            Some("In a zoomed picture: move about it with the arrows"),
        ),
        (
            KeyBinding::new("right", ui::viewer::PanRight, Some("PictureViewer")),
            None,
        ),
        (
            KeyBinding::new("up", ui::viewer::PanUp, Some("PictureViewer")),
            None,
        ),
        (
            KeyBinding::new("down", ui::viewer::PanDown, Some("PictureViewer")),
            None,
        ),
        (
            KeyBinding::new("o", ui::viewer::OpenOutside, Some("PictureViewer")),
            Some("In a picture: open it with the system's viewer"),
        ),
        (
            KeyBinding::new("alt-u", ToggleCrew, Some("Chat")),
            Some("Switch Crew on or off for the next message"),
        ),
        (
            KeyBinding::new("alt-w", DismissCrew, Some("Chat")),
            Some("Send this message without Crew, though it says @crew"),
        ),
        (
            KeyBinding::new("enter", ui::actions::ToggleDiff, Some("ChangedFile")),
            Some("On a changed file under a turn: show or hide its diff"),
        ),
        (
            KeyBinding::new("enter", ui::actions::ReviewFiles, Some("TurnFiles")),
            Some("On the files a turn changed: review them in the Changes tab"),
        ),
        (
            KeyBinding::new("enter", ui::messages::ToggleRun, Some("ToolRun")),
            Some("On a tool call, a thought, a folded run or a preview: open or close it"),
        ),
        (
            KeyBinding::new("space", ui::messages::ToggleRun, Some("ToolRun")),
            None,
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
    // A Mac app quits on Cmd+Q from its menu, and the window sets none.
    #[cfg(target_os = "macos")]
    keys.push((KeyBinding::new("cmd-q", Quit, None), Some("Quit Letronna")));
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
    // Enter and Escape answer a permission card from the card itself or
    // from an empty composer. Bound after the input's own keys, so they are
    // tried first and hand the key back when there is no card to answer.
    keys.extend([
        (
            KeyBinding::new("enter", approval::AllowCall, Some("Permission")),
            Some("On a call waiting for a yes, or in an empty composer: allow it once"),
        ),
        (
            KeyBinding::new("escape", approval::DenyCall, Some("Permission")),
            Some("On a call waiting for a yes, or in an empty composer: deny it"),
        ),
        (
            KeyBinding::new("enter", approval::AllowCall, Some("Composer > Input")),
            None,
        ),
        (
            KeyBinding::new("escape", approval::DenyCall, Some("Composer > Input")),
            None,
        ),
    ]);
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
    // A Claude Code session the removed Claude Code plugin started still names
    // this binary as its hook; without an answer Claude Code goes on as it
    // would, so it gets none rather than a window.
    if std::env::args().any(|a| a == "--claude-code-hook") {
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
    let app = gpui_platform::application().with_assets(Assets);
    // A click on the Dock icon while the window is in the Dock brings it
    // back, as it does for any Mac app; elsewhere nothing calls it.
    app.on_reopen(platform::tray::show_main_window);
    app.run(|cx| {
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
                Box::new(plugins::chatgpt::ChatGpt::new()),
                Box::new(plugins::claude::Claude::new()),
            ],
        );
        // MCP servers can take seconds to start; started with the window,
        // their tools are there by the first message.
        letronna_core::mcp::start();
        // Off unless Settings turned it on.
        let _ = letronna_core::providers::local::apply(&letronna_core::store::current_settings());
        cx.bind_keys(
            keys(cx.keyboard_mapper().as_ref())
                .into_iter()
                .map(|(binding, _)| binding),
        );
        cx.on_action(|_: &Quit, cx| cx.quit());
        ui::quick::init(cx);
        platform::tray::init(cx);
        cx.on_window_closed(|cx, _| {
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
            // After the jobs, which may have been writing in a helper's
            // worktree; one kept for its work is left for the user to find.
            let _ = letronna_core::agents::stop_all();
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
        // away: hidden on Windows, in the Dock elsewhere. Without one that
        // would strand it, so let it close; the tray's Quit, or Cmd+Q on a
        // Mac, is what ends it.
        if platform::tray::available() {
            let _ = window.update(cx, |_, window, cx| {
                window.on_window_should_close(cx, |window, _| {
                    platform::window::hide(window);
                    false
                });
            });
        }
        // Back from the Dock or the tray, the window showed the frame it
        // went away with until something else redrew it, so a session
        // opened from the menu meanwhile waited for the pointer to move.
        let _ = window.update(cx, |_, window, _| {
            window
                .observe_window_visibility(|visibility, window, _| {
                    if visibility.is_visible() {
                        window.refresh();
                    }
                })
                .detach();
        });
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::keys;

    #[test]
    fn every_key_but_the_window_wide_ones_is_bound_in_a_context() {
        // gpui ranks a binding with no context above every other, so a new
        // one would take its key from every dialog, menu and text box. A new
        // session, the sidebar, settings, and on macOS quitting.
        let window_wide: Vec<String> = keys(&gpui::DummyKeyboardMapper)
            .iter()
            .filter(|(binding, _)| binding.predicate().is_none())
            .map(|(binding, _)| binding.keystrokes()[0].unparse())
            .collect();
        let expected = if cfg!(target_os = "macos") { 4 } else { 3 };
        assert_eq!(window_wide.len(), expected, "{window_wide:?}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn cmd_q_quits_from_anywhere_in_the_window() {
        let quit: Vec<_> = keys(&gpui::DummyKeyboardMapper)
            .into_iter()
            .filter(|(binding, _)| binding.keystrokes()[0].unparse() == "cmd-q")
            .collect();
        assert_eq!(quit.len(), 1);
        let (binding, about) = &quit[0];
        assert!(binding.action().partial_eq(&crate::app::Quit));
        assert!(binding.predicate().is_none());
        assert!(about.is_some());
    }

    #[test]
    fn no_key_is_the_one_the_inspector_takes_in_a_debug_build() {
        // gpui-component binds Ctrl+Shift+I with no context in a debug
        // build, which outranks any of ours; Cmd is Ctrl off macOS.
        let taken: Vec<String> = keys(&gpui::DummyKeyboardMapper)
            .iter()
            .map(|(binding, _)| binding.keystrokes()[0].unparse())
            .filter(|key| matches!(key.as_str(), "ctrl-shift-i" | "cmd-shift-i"))
            .collect();
        assert!(taken.is_empty(), "{taken:?}");
    }
}
