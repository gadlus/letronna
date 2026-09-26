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
- Gateways: Letronna (hosted models on your account), OpenAI, OpenRouter, Ollama, or a custom base URL
- Searchable model picker fed by the gateway's model list
- Single ~24 MB executable, no runtime, no Electron

## Install

One line, then a few questions.

**Windows** (PowerShell):

```powershell
iex (irm https://letronna.ai/install.ps1)
```

**Linux**:

```sh
curl -fsSL https://letronna.ai/install.sh | sh
```

The script only delivers the program. It downloads the latest release and hands over to `letronna setup`, which asks what to call you, signs you in, picks a provider and a model (keys are typed as dots and never printed), and installs the window, the terminal or both. Nothing needs administrator rights and nothing goes outside your own account. Run `letronna setup` again any time to change the answers; it also runs by itself the first time Letronna starts with nothing set up.

Until letronna.ai serves the scripts, the same ones are attached to each release:

```powershell
iex (irm https://github.com/gadlus/letronna/releases/latest/download/install.ps1)
```

Set `LETRONNA_DOWNLOAD` to install from a different archive (a URL or a local file), or `LETRONNA_NO_SETUP=1` to unpack without the questions.

## Run it

Or download or build it and run it.

On **Windows**, first launch copies the executable to `%LOCALAPPDATA%\Programs\Letronna`, adds a Start Menu entry so it shows up in Windows search, and registers in Apps & features. Newer builds re-register on launch. Uninstall from Apps & features, or run `Letronna.exe --uninstall`.

On **Linux**, first launch copies the binary to `~/.local/bin/letronna` and writes `~/.local/share/applications/letronna.desktop`, so it appears in the application menu. Uninstall with `letronna --uninstall`. The tray icon uses StatusNotifierItem over D-Bus, which needs a panel that supports it (KDE does; GNOME needs an extension such as AppIndicator Support). There is no backdrop blur behind dialogs, and closing the window minimises rather than hiding to the tray.

On **macOS** the app runs in place: no install, no tray, no blur. `scripts/bundle-macos.sh` wraps the release binary in `target/Letronna.app`, which macOS needs before it will show a Dock icon and the right name; drag it wherever you like. It does not register itself, so `--uninstall` has nothing to do and a second launch opens a second window.

| | Windows | Linux | macOS |
|---|---|---|---|
| Chat and agent tools | yes | yes | yes |
| Install / uninstall | yes | yes | no |
| Single instance | yes, focuses the open window | yes, second launch exits | no |
| Tray icon | yes, themed menu | yes, native menu | no |
| Backdrop blur | yes | no | no |

If Discord is running, Letronna shows a rich presence (Idle, or Chatting with the model). Set `LETRONNA_DISCORD_APP_ID` to your own Discord application id, and upload a `letronna` art asset to it, for the icon to appear.

Closing the window keeps Letronna in the notification area. Left-click the tray icon to bring it back; right-click for Check for updates and Quit.

First launch uses the Letronna gateway: sign in (`/login` in the terminal, or **Account** in Settings) and the hosted models are charged to your account's credit, with no key to paste. Open **Settings** to pick another gateway and use your own API key instead.

## Gateways

| Gateway     | Base URL                          | Key      |
|-------------|-----------------------------------|----------|
| Letronna    | `https://api.letronna.ai/v1`      | sign in  |
| OpenAI      | `https://api.openai.com/v1`       | required |
| OpenRouter  | `https://openrouter.ai/api/v1`    | required |
| Ollama      | `http://localhost:11434/v1`       | none     |
| Custom      | anything OpenAI-compatible        | optional |

The Letronna gateway's address follows `LETRONNA_API`, for working against a local API.

Model lists come from `GET {base}/models`. Streaming uses SSE chat completions.

## Images, speech and video

Signed in, the agent can also make things on Letronna's hosted models, whichever gateway the chat itself uses:

| Tool         | Makes                                                     | Paid by       |
|--------------|-----------------------------------------------------------|---------------|
| `make_image` | an image from a description                               | the image     |
| `speak`      | text read aloud                                           | the character |
| `transcribe` | the words in an audio or video file, up to 25 MB          | the minute    |
| `make_video` | a short MP4 from a description, in a few minutes          | the second    |

Each one asks first, since it spends your credit, and none is offered in plan mode or in a shared chat. Unless a model is named, the cheapest one working is used; the prices are on the dashboard's models page. What is made is saved in `media/` in the project folder, or in the data folder when no project is attached. The window shows a made image in the chat, and opens any made file.

## Local data

`%LOCALAPPDATA%\Letronna\` on Windows, `~/Library/Application Support/Letronna/` on macOS, `~/.local/share/Letronna/` on Linux. `AGENT_DATA_DIR` overrides all three.

- `chats.json` sessions and the selected gateway, endpoint and model
- `keys.json` API keys per gateway, plain text on your disk only
- `account.json` the Letronna sign-in for this computer
- `media/` images, speech and video made with no project attached

Chat text is sent to the gateway you selected and nowhere else.

## Build

Requirements on Windows: Rust, Visual Studio C++ build tools, and a Windows SDK with `fxc.exe`. Set `GPUI_FXC_PATH` in `.cargo/config.toml` to your SDK's shader compiler.

On Linux you need gpui's usual build dependencies (Vulkan, X11 or Wayland, and `libxkbcommon`). The tray needs no extra system libraries: the KSNI backend talks D-Bus directly.

On macOS the Command Line Tools are enough. gpui normally compiles its Metal shaders with `xcrun metal`, which only ships in Xcode's separately downloaded Metal toolchain. The macOS build enables gpui's `runtime_shaders` feature instead, so the Metal driver compiles them at startup. Run `scripts/bundle-macos.sh` after the release build for an `.app`.

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
scripts/            icon sync, macOS .app bundling, window capture for UI checks
```

Adding an integration (a new plugin) is one file in `src/plugins` implementing
`Plugin`, plus one line in `main`. The app only emits `AppEvent`s; it never
calls a plugin directly.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Small, focused pull requests are welcome.

## License

MIT, © 2026 Gadlus Engineering. GPUI and gpui-component are Apache-2.0. Phosphor Icons are MIT.

Developed by the Gadlus Engineering team.
