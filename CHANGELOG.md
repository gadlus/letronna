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
- **All** — `letronna doctor` checks the setup, and a local log and crash files help find out what went wrong.
- **App** — The window wears letronna.ai's colours; Settings choose its colours and whether it shows film grain.
- **All** — Letronna knows each model's context window and output limit.
- **All** — Helpers can answer in a checked shape.
- **All** — The model knows today's date.
- **All** — Send more while Letronna works: it reaches the model at the reply's next step, in the window and the terminal.
- **All** — Helper types, built in and from your own files, share one token budget across a whole tree of helpers.
- **All** — Helpers' conversations are kept beside their session, out of the session list.
- **All** — See, steer and stop helpers at work, and answer their approvals, in the window and the terminal.
- **All** — Fork a helper from the conversation, and ask something aside with `/btw`.
- **All** — Kept helpers wait idle for more work and wake on a message.
- **All** — A session's agents share a task list with owners and blockers.
- **All** — Helpers can answer through a checked schema, even at their round limit.
- **All** — Workflow runs keep a journal and replay what already finished.
- **All** — Letronna keeps notes on each project in a memory of its own, which refuses keys and planted instructions.
- **All** — `/init` has the model write the project's AGENTS.md, and AGENTS.md or CLAUDE.md can pull in other files with `@path`.
- **All** — `/usage` adds up tokens and cost per session, day and model; the usage line says how full the model's window was.
- **All** — Each session keeps its own model and effort.
- **All** — Your own hook commands can block a call or keep a turn going.
- **All** — The model sees pictures you attach and those its tools open.
- **All** — MCP over HTTP; servers take variables, tool filters and their own start time, restart after they stop, and reload when mcp.json changes. `letronna mcp` adds them.
- **All** — Many MCP tools are looked up on demand instead of sent with every request.
- **All** — Any tool can be switched off, and tools that cannot work are not offered.
- **Chat apps** — Talk to Letronna from Slack. A Telegram chat pairs by a code, and only its owner answers.
- **Chat apps** — Whole, formatted replies with Allow/Deny buttons; files, photos and voice notes go both ways.
- **Terminal** — Notices show and are taken mid-turn and between prompts; lines typed while it works show as queued and can be taken back.
- **All** — Send now: a queued message goes in without waiting for the step.
- **App** — Point mode in the browser panel: click a spot on the page, comment on it, and the reference goes in the composer.

### Changed
- **All** — A failed model request says what went wrong: the key, credit, a rate limit, a busy provider, a conversation too long, and so on.
- **All** — OpenAI's reasoning models work, and a server that turns down a field is sent the request again without it.
- **All** — The context window counts tool calls, pictures and Chinese, Japanese and Korean text, so long conversations are trimmed where they should be.
- **All** — Following up while Letronna works keeps the turn's original request in view.
- **All** — The checklist stays in view, in the window and the terminal, even in a long task.
- **All** — Approvals you give in one session stay in that session.
- **All** — Tools accept other agents' argument names and quoted values, and `edit_file` finds text through different line endings and spacing.
- **All** — Commands run with no pager or editor to wait on, and colour codes are kept out of what the model reads.
- **App** — The approval card and tool lookups no longer reread files on every frame.
- **App** — The example plugin starts switched off.
- **All** — Batches of calls run at once.
- **All** — Session search finds words in any order, in the sidebar and `/resume`; the terminal lists sessions without reading every file.
- **All** — Earlier turns survive a failed summary, a summary survives a restart, and compacting shows.
- **All** — A project folder inside a repository keeps the repository's rules.
- **All** — Commands and MCP servers start with the login shell's variables.
- **All** — Stopping an MCP call tells its server, and stopping a server ends everything it started.
- **All** — Plan mode refuses tools that change things, however they are asked for.
- **All** — Secret files such as `.env` and private keys stay out of the model's reach; `.env.example` does not.
- **All** — Stop reaches waits and running commands, and ends them.
- **All** — Long replies are no longer cut off at two minutes; only five minutes of silence ends one.
- **All** — Unattended runs never stall waiting on a question.
- **All** — Summaries reuse the chat connection, and a busy background job no longer copies its whole log on every read.

### Fixed
- **All** — Catastrophic commands, such as deleting your home folder, are refused whatever the approval setting.
- **All** — Two copies of Letronna never mix their writes to one session, and a crash part way through a save costs one line, not the session.
- **All** — Keys, settings and memory files are never silently reset or wiped by a read that failed, and a broken skills.json is never replaced by the defaults.
- **All** — A session deleted in one copy of Letronna stays deleted, and a leftover lock file no longer lists a session twice.
- **All** — Commands never hang on a child process they leave running, and output that is not valid UTF-8 survives.
- **All** — `web_fetch` checks every address and redirect it follows.
- **All** — Streamed tool calls are put together per call.
- **All** — A project's AGENTS.md can no longer hang the app by linking to a device.
- **Terminal** — `letronna -p` no longer keeps a processor core busy while a reply streams.
- **All** — Two copies of Letronna starting at once never write into one session.
- **Terminal** — Background jobs stop when the terminal exits.

## [0.1.0-alpha.2] - 2026-09-27

### Added
- **App** — Right-click or hover a turn to copy, edit, retry, rewind or fork it.
- **Terminal** — Take back or retry the last exchange, or branch a session.
- **All** — A real browser: Letronna reads pages, clicks, types and takes screenshots. In the app it sits beside the chat, where you can watch it and use it yourself.
- **App** — Each session has a browser of its own, or one shared by all, and it can be a cloud browser or Chrome on another machine.
- **All** — Letronna updates itself: the app finds, checks and installs new versions, and `letronna update` does it in the terminal.
- **All** — Search the web with your own search service.
- **All** — MCP servers reached by URL, with headers such as a key; one-shot questions wait for MCP servers to start.
- **All** — Rewind a session, and it stays rewound.
- **Terminal** — The guide is a click away, from `/help`.
- **All** — Letronna makes images, speech and video, and turns speech into text; pick which model makes each.
- **All** — Letronna remembers: `SOUL.md` says who it is, `USER.md` and `MEMORY.md` hold what it knows about you and its own notes, and a project's `AGENTS.md` is read as its instructions.
- **All** — Tools from MCP servers, set up in `mcp.json`.
- **All** — Read a web page.
- **All** — Search past sessions.
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
- **App** — Type while it answers, press Esc to stop, and deleting asks first.
- **App** — The chat reads as a column, with your messages in cards and no flash when switching sessions.
- **All** — Signing in no longer waits forever for the browser, ends on a page that looks like the website, and while waiting offers to open the page again, copy it or cancel.
- **Windows**, **macOS** — Releases can be code signed, so Windows and macOS stop warning about an unknown publisher.
- **All** — Old turns are summarized instead of forgotten when a conversation outgrows the model's window.
- **All** — Model calls reuse one connection, and search skips files without a match before reading them.
- **All** — Letronna is the default gateway, and the only hosted one.
- **All** — Each session is its own file, so two copies of Letronna can run at once.
- **All** — Settings saved on a gateway that is no longer offered move to your Letronna account.
- **Terminal** — Questions are asked inside the input box, above where you type.
- **All** — A turn that fails says why; tools work on gateways that rename them.

### Fixed
- **All** — Names with accents are no longer garbled by signing in.
- **Linux**, **macOS** — Keys and sign-in files are readable by you only.
- **All** — Your keys and sign-in stay out of the model's reach.
- **Terminal** — Sessions keep their pins and todos when the terminal saves them.
- **Windows** — Pastes arrive as one paste, commands run without flashing a console window, and the wheel scrolls.
- **All** — Context no longer lost on large tool results; dangling tool calls are repaired.
- **App** — Asking the Discord bot to read the chat no longer hangs the app.
- **macOS** — The window's buttons stay visible when another app has focus.

## [0.1.0-alpha.1] - 2026-09-18

### Added
- **App** — The first alpha of the Letronna desktop app: chat with an agent that can read, write and run things in your project, with approvals, sessions, and a choice of gateway (OpenAI, OpenRouter, Ollama or any OpenAI-compatible server).
- **Windows** — Installs itself on first run, with a Start Menu entry and a tray icon.
