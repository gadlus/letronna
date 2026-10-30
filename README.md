<p align="center">
  <img src="crates/gui/assets/letronna-front.png" width="280" alt="Letronna">
</p>

<h1 align="center">Letronna</h1>

<p align="center">
  A native desktop and terminal agent, with a cloud that keeps working when you close your laptop.<br>
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

Letronna is an agent that works on your computer. It reads and edits files, runs commands, drives a real browser and remembers what you tell it. It ships as a desktop app on [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) and a terminal app, both built on one Rust core, and it talks to any OpenAI-compatible model.

## Features

- **Agent tools**: files, shell, web search, a live browser beside the chat, images, speech and video. Every action that changes something asks first, or you can use an auto mode that judges what is safe.
- **Helpers and workflows**: sub-agents that work side by side on a shared task list, plus scripted workflows that run many of them.
- **Any model**: hosted models on your Letronna account, your ChatGPT or Claude plan, OpenAI, Anthropic, Gemini, xAI, OpenRouter, OpenCode Go/Zen, Ollama, or any base URL. A provider takes more than one account, each plan shows how much is left, and a request goes on with your next account when one runs out.
- **Extensible**: MCP servers (stdio and HTTP), plugins, skills, hooks, language servers, and editors over ACP.
- **Any model**: hosted models on your Letronna account, OpenAI, Anthropic, OpenRouter, Ollama, or any base URL.
- **Extensible**: MCP servers (stdio and HTTP), plugins (with code in JavaScript or Python, nothing to build), skills, hooks, language servers, and editors over ACP.
- **Chat apps**: answer from Telegram, Discord or Slack with `letronna serve`.
- **Letronna Cloud**: run tasks on Letronna's servers, keep services running, and host web apps.
- **Small and local**: one executable, no runtime, and sessions stay on your disk.

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
