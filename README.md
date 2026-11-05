<p align="center">
  <img src="crates/gui/assets/letronna-front.png" width="280" alt="Letronna">
</p>

<h1 align="center">Letronna</h1>

<p align="center">
  An agent with its own cloud and wallet. Prompt in, live link out.<br>
  Rust, GPU rendered, no browser inside. <b>Alpha</b>: expect rough edges and breaking changes.
</p>

<p align="center">
  <a href="https://github.com/gadlus/letronna/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/gadlus/letronna?include_prereleases&label=release&color=1f4fd8"></a>
  <a href="LICENSE"><img alt="MIT license" src="https://img.shields.io/badge/license-MIT-1f4fd8"></a>
  <a href="https://letronna.ai"><img alt="letronna.ai" src="https://img.shields.io/badge/web-letronna.ai-00126b"></a>
  <a href="https://github.com/sponsors/gadlus"><img alt="Sponsor" src="https://img.shields.io/badge/sponsor-%E2%9D%A4-db61a2?logo=githubsponsors&logoColor=white"></a>
</p>

<p align="center">
  <a href="https://letronna.ai/dashboard/download">Download</a> ·
  <a href="docs/README.md">Docs</a> ·
  <a href="CHANGELOG.md">Changelog</a> ·
  <a href="https://github.com/sponsors/gadlus">Sponsor</a>
</p>

Letronna is an agent with its own cloud and wallet. It builds on your computer or in Letronna Cloud, hosts the result at a live link, pays for the APIs it needs, and is written in Rust: it starts in 8 ms. It ships as a desktop app on [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui), a terminal app and a phone app, and the desktop and terminal share one Rust core.

## Features

**Its own cloud**
- **Letronna Cloud**: runs tasks on Letronna's servers, builds web apps and hosts them at a live link anyone can open. You can watch the page and its code being written while it works.
- **Keeps working**: close the laptop and a cloud run goes on. Sessions on a GitHub repo push their branch and open a pull request when you ask.
- **Routines and connectors**: runs on a schedule, and connectors signed in with OAuth.

**Its own wallet**
- **Pays for what it needs**: pays for APIs over x402 and buys from your crypto wallet. Dedicated, encrypted and open source, so anyone can check it.

**On your computer**
- **Agent tools**: files, shell, web search, a live browser beside the chat, images, speech and video. Every action that changes something asks first, or an auto mode judges what is safe.
- **Helpers and workflows**: sub-agents that work side by side on a shared task list, scripted workflows that run many of them, and a tester and reviewer that check the work against what you asked.
- **Background sessions**: `letronna --bg` keeps working with the terminal closed. `attach` picks it back up, and `/recap` says where a session stands.
- **Local models**: picks the models your computer runs well, downloads them checked, and answers on your GPU with no internet. Any GGUF can be added from a Hugging Face link.
- **Editors**: a VS Code extension and `/ide` hand it what you select, `/review` reviews a pull request, and language servers and formatters check each edit.
- **Terminal**: vim keys, themes, your own status line, keybindings, history search with ctrl+r, and carrying a session between the terminal and the window.

**On your phone** (iPhone and Android, coming soon)
- **Deploy anything from your pocket**: ask for an app and get a live link, with each cloud step shown as it goes and a Live Activity while it runs.
- **Your computer from your phone**: pair over an encrypted link, or straight over Tailscale, and approve its calls from the phone.
- **Chats that make things**: web search with sources, files, photos, QR codes, and Word or PDF export.

**Any model, honest cost**
- Hosted models on your Letronna account (frontier models at 70% off, see [pricing](https://letronna.ai/pricing)), your ChatGPT or Claude plan, OpenAI, Anthropic, Gemini, xAI, OpenRouter, OpenCode Go/Zen, Ollama, or any base URL.
- Every answer shows its tokens and what it cost. A provider can have several accounts, and a request moves to your next one when a plan runs out.

**Extensible**
- MCP servers (stdio and HTTP), plugins (code in JavaScript or Python, nothing to build), skills, hooks, language servers, and editors over ACP.
- Answers from Telegram, Discord or Slack with `letronna serve`.

**Small and fast**
- One executable with no runtime. Cold start in 7.65 ms and 9.5 MB idle, and sessions stay on your disk.
- 79.8% on Terminal-Bench 2.0, 84.0 on Harness-Bench, and 10/10 on SWE-bench Verified. See [letronna.ai/benchmark](https://letronna.ai/benchmark).

## Install

```powershell
# Windows (PowerShell)
iex (irm https://letronna.ai/install.ps1)
```

```sh
# Linux
curl -fsSL https://letronna.ai/install.sh | sh
```

On macOS, download the `.dmg` from the [dashboard](https://letronna.ai/dashboard/download). After installing, `letronna setup` signs you in and picks a model. See [Getting started](docs/getting-started.md).

## Usage

```sh
letronna                  # chat in the terminal
letronna gui              # open the window
letronna -p "question"    # one-shot, prints the answer
letronna --continue       # resume the last session
```

## Documentation

The full guide is in [`docs/`](docs/README.md):

- [Getting started](docs/getting-started.md)
- [App and terminal](docs/app-and-terminal.md)
- [Gateways and models](docs/gateways.md)
- [Tools](docs/tools.md)
- [Extending](docs/extending.md)
- [Letronna Cloud](docs/cloud.md)
- [Configuration](docs/configuration.md)
- [Data and privacy](docs/data-and-privacy.md)
- [Architecture](docs/architecture.md)
- [Building](docs/building.md)
- [Benchmarks](docs/benchmarks.md)

## Build

```sh
cargo build --release
cargo test --release
```

Platform requirements are listed in [Building](docs/building.md).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Small, focused pull requests are welcome.

## License

MIT, © 2026 [Gadlus LLC](https://gadl.us). GPUI and GPUI Kit are Apache-2.0. Phosphor Icons are MIT.
