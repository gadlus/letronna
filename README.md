<p align="center">
  <img src="assets/letronna-front.png" width="320" alt="Letronna">
</p>

<h1 align="center">Letronna</h1>

<p align="center">
  A small native desktop agent. Rust, GPU rendered, no browser inside.<br>
  <b>Alpha.</b> Expect rough edges and breaking changes.
</p>

## What it is

Letronna is a chat-first desktop app built on [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) and [gpui-component](https://github.com/longbridge/gpui-component). It talks to any OpenAI-compatible endpoint through a gateway you pick, streams replies, and keeps sessions on your machine.

- Sessions sidebar with search, pinning and date groups
- Markdown replies, streaming, multi-line composer
- Gateways: Upstream Free, Upstream, OpenAI, OpenRouter, Ollama, or a custom base URL
- Searchable model picker fed by the gateway's model list
- Single ~24 MB executable, no runtime, no Electron

## Run it

Download or build it and run it.

On **Windows**, first launch copies the executable to `%LOCALAPPDATA%\Programs\Letronna`, adds a Start Menu entry so it shows up in Windows search, and registers in Apps & features. Newer builds re-register on launch. Uninstall from Apps & features, or run `Letronna.exe --uninstall`.

On **Linux**, first launch copies the binary to `~/.local/bin/letronna` and writes `~/.local/share/applications/letronna.desktop`, so it appears in the application menu. Uninstall with `letronna --uninstall`. The tray icon uses StatusNotifierItem over D-Bus, which needs a panel that supports it (KDE does; GNOME needs an extension such as AppIndicator Support). There is no backdrop blur behind dialogs, and closing the window minimises rather than hiding to the tray.

On **macOS** the app runs in place: no install, no tray, no blur. A proper install there means an `.app` bundle, which is not built yet.

| | Windows | Linux | macOS |
|---|---|---|---|
| Chat and agent tools | yes | yes | yes |
| Install / uninstall | yes | yes | no |
| Single instance | yes, focuses the open window | yes, second launch exits | no |
| Tray icon | yes, themed menu | yes, native menu | no |
| Backdrop blur | yes | no | no |

If Discord is running, Letronna shows a rich presence (Idle, or Chatting with the model). Set `LETRONNA_DISCORD_APP_ID` to your own Discord application id, and upload a `letronna` art asset to it, for the icon to appear.

Closing the window keeps Letronna in the notification area. Left-click the tray icon to bring it back; right-click for Check for updates and Quit.

First launch uses the Upstream Free gateway, which needs no key and gives a few guest replies. Open **Settings** at the bottom of the sidebar to pick another gateway, paste an API key, and reload the model list.

## Gateways

| Gateway     | Base URL                          | Key      |
|-------------|-----------------------------------|----------|
| Upstream Free   | `https://pool.example/v1`          | none     |
| Upstream        | `https://pool.example/v1`          | required |

Upstream Free talks to the guest chat API and lists the free catalog (`pool-free/…` ids). Upstream talks to the marketplace API with your key and lists its own catalog. The two catalogs are separate, so a model shown on one gateway is not necessarily served on the other.
| OpenAI      | `https://api.openai.com/v1`       | required |
| OpenRouter  | `https://openrouter.ai/api/v1`    | required |
| Ollama      | `http://localhost:11434/v1`       | none     |
| Custom      | anything OpenAI-compatible        | optional |

Model lists come from `GET {base}/models`. Streaming uses SSE chat completions.

## Local data

`%LOCALAPPDATA%\Letronna\`

- `chats.json` sessions and the selected gateway, endpoint and model
- `keys.json` API keys per gateway, plain text on your disk only
- `upstream-session.txt` the Upstream guest cookie

Chat text is sent to the gateway you selected and nowhere else.

## Build

Requirements on Windows: Rust, Visual Studio C++ build tools, and a Windows SDK with `fxc.exe`. Set `GPUI_FXC_PATH` in `.cargo/config.toml` to your SDK's shader compiler.

On Linux you need gpui's usual build dependencies (Vulkan, X11 or Wayland, and `libxkbcommon`). The tray needs no extra system libraries: the KSNI backend talks D-Bus directly.

```powershell
cargo build --release
target
elease\letronna.exe   # registers itself on first run
```

Checks:

```powershell
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --release
```

Icons are Phosphor Light. `scripts/icons.sh` re-syncs them from npm.

## Layout

```
src/main.rs         entry point: wires plugins, tray, window
src/app.rs          Chat state, sessions, sending, render root
src/prelude.rs      shared imports
src/theme.rs        fonts and palette
src/assets.rs       embedded icons and the AppIcon enum

src/core/           domain layer (no UI, no OS)
  chat.rs           HTTP: validation, streaming, model lists, reasoning, usage
  gateway.rs        gateway presets
  store.rs          on-disk sessions and keys

src/ui/             view surfaces (impl Chat blocks)
  sidebar.rs composer.rs messages.rs settings.rs sessions.rs chrome.rs

src/plugins/        optional integrations behind a Plugin trait + event bus
  mod.rs            Plugin trait, AppEvent, registry, emit()
  discord.rs        Discord rich presence

src/platform/       OS integration; same API everywhere, per-OS behind cfg
  install.rs        first-run registration and --uninstall
  tray.rs           notification-area icon and menu
  window.rs         Win32 window helpers
  backdrop.rs       captures and blurs the window for modal backdrops

assets/             icon, avatar, SVG icons
scripts/            icon sync, window capture for UI checks
```

Adding an integration (a new plugin) is one file in `src/plugins` implementing
`Plugin`, plus one line in `main`. The app only emits `AppEvent`s; it never
calls a plugin directly.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Small, focused pull requests are welcome.

## License

MIT, © 2026 Gadlus Engineering. GPUI and gpui-component are Apache-2.0. Phosphor Icons are MIT.

Developed by the Gadlus Engineering team.
