#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod app;
mod assets;
mod platform;
mod plugins;
mod prelude;
mod theme;
mod ui;

use prelude::*;

fn main() {
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
    letronna_core::diag::init("gui");
    Application::new().with_assets(Assets).run(|cx| {
        gpui_component::init(cx);
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
        cx.bind_keys([
            // "secondary" is cmd on macOS and ctrl everywhere else.
            KeyBinding::new("secondary-n", NewSession, None),
            KeyBinding::new("secondary-b", ToggleSidebar, None),
            KeyBinding::new("secondary-,", OpenSettings, None),
            // Scoped to the chat rather than global: gpui ranks a binding
            // with no context above every other, which took escape away
            // from dialogs and menus. Inside the chat, theirs go first.
            KeyBinding::new("escape", Dismiss, Some("Chat")),
            KeyBinding::new("secondary-j", ui::panel::TogglePanel, Some("Chat")),
            KeyBinding::new("secondary-shift-l", ui::panel::BrowserTab, Some("Chat")),
            KeyBinding::new("secondary-shift-u", ui::usage::OpenUsage, Some("Chat")),
            KeyBinding::new("secondary-shift-a", OpenActivity, Some("Chat")),
            KeyBinding::new("secondary-k", ui::palette::OpenPalette, Some("Chat")),
            KeyBinding::new(
                "shift-enter",
                gpui_component::input::Enter { secondary: true },
                Some("Input"),
            ),
            // Bound after the input's own Ctrl+Enter, a new line, so it is
            // tried first and can hand the key back when there is nothing
            // to send now.
            KeyBinding::new("secondary-enter", SendNow, Some("Composer > Input")),
            // On the browser's page, Tab moves through the page, not the
            // window.
            KeyBinding::new("tab", ui::browser::PageTab, Some("Browser")),
            KeyBinding::new("shift-tab", ui::browser::PageTabBack, Some("Browser")),
            KeyBinding::new("tab", ui::composer::LeaveComposer, Some("Composer > Input")),
            KeyBinding::new("enter", ui::search::OpenHit, Some("SearchHit")),
        ]);
        platform::tray::init(cx);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        // Background jobs are children of this process; do not outlive it.
        // Nor does a browser with no window, which nobody could close.
        cx.on_app_quit(|_| {
            letronna_core::jobs::stop_all();
            letronna_core::mcp::stop_all();
            letronna_core::browser::close_hidden();
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
                    cx.new(|cx| Root::new(chat, window, cx))
                },
            )
            .expect("Could not open native application window");
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
