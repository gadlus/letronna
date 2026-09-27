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
    Application::new().with_assets(Assets).run(|cx| {
        gpui_component::init(cx);
        theme::apply(cx);
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
            KeyBinding::new(
                "shift-enter",
                gpui_component::input::Enter { secondary: true },
                Some("Input"),
            ),
            // On the browser's page, Tab moves through the page, not the
            // window.
            KeyBinding::new("tab", ui::browser::PageTab, Some("Browser")),
            KeyBinding::new("shift-tab", ui::browser::PageTabBack, Some("Browser")),
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
            letronna_core::browser::close_hidden();
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
        let _ = window.update(cx, |_, window, _| platform::window::use_dark_chrome(window));
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
