# Changelog

Every change to Letronna — the app, the terminal, on Windows, macOS and Linux —
by version, newest first. One version number covers all of them.

Versions follow [semantic versioning](https://semver.org): `0.x` until the first
stable release, `-alpha.N` / `-beta.N` while it is early. Changes gather under
**Unreleased** and become a version when it is tagged (`vX.Y.Z`), which builds
the release for every platform. Each line starts with where it applies:
**App**, **Terminal**, **Windows**, **macOS**, **Linux**, or **All**.

This file is also the App tab of https://letronna.ai/changelog.

## [Unreleased]

### Added
- **Terminal** — `letronna`, the agent in a terminal: the conversation, tools, diffs, `/resume` by name, `/model`, `/effort`, `/thinking`, `/search`, `/plan`, `/jobs` and `/skills`.
- **All** — One Letronna account for the app and the terminal: `/login`, `/whoami`, `/logout`, and an Account page in the app. Signing in happens on letronna.ai.
- **All** — Letronna's hosted models on your account, paid from your credit, with no key to paste.
- **All** — Use every gateway you set up at once: the model list gathers models from Letronna and each gateway with a key, and a message goes to the gateway of the model you pick.
- **All** — Gateways as a status list (signed in, connected, not set up, not running): Settings > Gateways in the app, `/gateways` in the terminal.
- **All** — Teams: bill hosted models to a team's shared pool or to yourself, with `/team` in the terminal or Settings > Account in the app.
- **All** — Effort, thinking and web search, where the gateway supports them.
- **All** — Scheduled jobs, background jobs, a checklist, plan mode, and plugins that add tools.
- **App** — Reach Letronna from Telegram and Discord, and scheduled jobs report to every channel.
- **App** — The Artifacts and Skills pages.
- **macOS** — macOS builds, with native fonts, paths and a proper .app.
- **Linux** — Install and tray on Linux.
- **All** — Install in one line, then a few questions.

### Changed
- **All** — Letronna is the default gateway, and the only hosted one.
- **All** — Each session is its own file, so two copies of Letronna can run at once.
- **All** — Settings saved on a gateway that is no longer offered move to your Letronna account.
- **Terminal** — Questions are asked inside the input box, above where you type.
- **All** — A turn that fails says why; tools work on gateways that rename them.

### Fixed
- **Windows** — Pastes arrive as one paste, commands run without flashing a console window, and the wheel scrolls.
- **All** — Context no longer lost on large tool results; dangling tool calls are repaired.
- **App** — Asking the Discord bot to read the chat no longer hangs the app.
- **macOS** — The window's buttons stay visible when another app has focus.

## [0.1.0-alpha.1] - 2026-09-18

### Added
- **App** — The first alpha of the Letronna desktop app: chat with an agent that can read, write and run things in your project, with approvals, sessions, and a choice of gateway (OpenAI, OpenRouter, Ollama or any OpenAI-compatible server).
- **Windows** — Installs itself on first run, with a Start Menu entry and a tray icon.
