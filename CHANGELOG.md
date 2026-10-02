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

### Highlights
- **All** — Helpers that work side by side, fork, wait for more work and share a task list, plus workflows that run many agents from a script you approve.
- **All** — Auto mode: a judge on your own model decides what would otherwise ask, and on macOS and Linux commands can run confined by the OS instead.
- **All** — MCP servers over HTTP and OAuth, plugins from a git repository, and Zed or JetBrains running Letronna over ACP.
- **App** — The browser opens beside the chat at half the room; long output rests as short previews; live work sits in one line over the box; Files, Changes and Jobs tabs, a Ctrl+K palette, and the whole window reachable from the keyboard.
- **Chat apps** — Slack, pairing by code, files and voice notes both ways, and `letronna serve` to answer with no window open.
- **All** — Schedules in a mode of their own, memory you can review and edit, and a window that uses far less memory and processor while it works or sits idle.

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
- **All** — Workflows: scripts run in a sandboxed JavaScript engine, Letronna can run one it wrote once you approve it, and Crew (`@crew` or a switch) opts a message into orchestration.
- **All** — Helpers can work in a git worktree of their own.
- **All** — `/loop` and `/heartbeat` keep one session going; a session can keep working toward a goal until a check says stop.
- **All** — Run your own prompts and skills as slash commands, and let the model save, patch and undo skills it learned.
- **All** — Attach files, line ranges and folders by typing `@path`; the composer offers commands after `/` and references after `@`.
- **All** — Choose a reply style, or write your own.
- **All** — Replies can be read aloud as they stream; voice input records to text at the cursor.
- **All** — Pictures you attach can be edited through the chat's image gateway, and long video renders run as background jobs.
- **All** — Computer use asks once for each app, on macOS and on Linux under X11.
- **All** — The browser uploads project files, keeps downloads, saves a page as a PDF, shoots the whole page, hovers, drags, waits for words, acts in frames from other sites and runs steps in a batch.
- **All** — Fetched pages keep their links, and a PDF is saved as a file; web searches can keep to some sites, skip others or stay recent.
- **All** — MCP: read resources, run prompts as commands, sign in to servers that use OAuth, and tools get unique names and clean schemas.
- **All** — Plugins bring skills and MCP servers from a git repository.
- **All** — Other agents can use Letronna's browser and media tools over MCP, and Zed and JetBrains editors can run Letronna over ACP.
- **All** — OpenAI models change several files in one patch and one yes.
- **All** — New errors from a language server reach the model after edits, and a turn that changed files is asked once to build or test them.
- **All** — A task carries on with a fallback model when its own fails.
- **All** — A subfolder's AGENTS.md reaches the model when it works there.
- **All** — `/insights` shows the last weeks by day, hour, tool and model; `/diff` shows what changed in the project and in this session.
- **All** — Notifications call you back when a run ends or needs you.
- **All** — Continue a Claude Code or Codex session, in the terminal or the window.
- **All** — Memory makes several changes in one call, all or none.
- **All** — Keys and tokens move to the Keychain or Credential Manager.
- **Terminal** — Other programs can drive `letronna -p` over stdin as JSON lines, and `--mode accept-edits` edits inside the project.
- **App** — Ctrl+K opens a palette of actions, pages, sessions and models, and a global shortcut opens a quick entry bar from anywhere.
- **App** — Files and Changes tabs beside the chat: browse and preview the project, review and revert this session's edits, and stage, commit and push.
- **App** — Capabilities gains Tools and MCP tabs with per-tool switches, and a Plugins page.
- **App** — A first-run setup page, data-folder profiles, and sidebar filters for session state and project grouping.
- **App** — HTML and SVG artifacts can be previewed, and a failed turn says why in the chat and offers the fix.
- **App** — Set up Slack from the Messaging page and pair a member by code.
- **All** — Auto mode: a judge on your own model decides what would otherwise ask, with a tag on each call and a list of its decisions (`/decisions` in the terminal, a Decisions list in the window).
- **All** — On macOS and Linux, a command can run confined by the OS (no network, writes only in the project) instead of asking, in Accept edits and Auto.
- **All** — Saved workflows become slash commands, and `/review` ships built in; watch, pause, stop, resume, skip and retry workflow runs and their agents, in the terminal and the window, and read one agent's transcript or a run's log.
- **All** — Schedules run from core in whichever Letronna is open, never twice, each in a mode of its own; the model can add, list and remove them, and schedules, chat apps and `/bg` run beside the chat.
- **All** — `letronna serve` answers the chat apps with no window open, and the window takes over from it.
- **All** — Letronna can look back every 10 messages for what to remember; see and edit memory with `/memory` or the Memory page in Settings.
- **All** — Summarize earlier turns on demand, now or up to a chosen turn.
- **All** — Old sessions can be deleted after a number of days, and a rewind leaves nothing of what it took back in the file.
- **All** — A command still running at its time limit keeps running as a background job.
- **App** — Every part of the window can be reached from the keyboard; a run of tool calls folds into one line; a finished turn lists the files it changed; a Jobs tab shows background jobs live; a status-bar popover shows what fills the context.

- **App** — The browser opens beside the chat at half the room, and can take it all; while the agent browses, the page shows small in the corner of the chat.
- **App** — Long output rests as short previews: a command's last lines (a failed one in red, kept open), a file's first lines, an edit as +a −b with its diff, thinking as "Thought for 12s"; pictures open in the window's own viewer.
- **App** — Helpers, workflow runs and jobs still working, and the plan's steps, sit in one line over the box; while the model writes a long call the reply says "Writing app.rs…".
- **App** — Approval cards: Enter allows and Escape denies, and "2 of 3" shows when a reply asks about several calls.
- **App** — The sidebar gives the sessions the room, with the other pages under More; Ctrl+Tab switches between recent sessions; the empty chat lists recent sessions.
- **App** — Picking an image, speech or video model makes what you send with it.
- **All** — When a site's check stops the agent, it waits while you get past it and carries on by itself.
- **All** — The browser works on a site's own dropdowns, scrolls a panel by its ref, types into date, time, colour and range fields, names what a click hit, and waits for what a click set off before reading the page.
- **Terminal** — Skills and MCP prompts no longer crowd the `/` menu: they come up as you type, and under `/skills` and `/mcp`.
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
- **All** — Runs without a window (`-p`, helpers, workflows) keep to deny rules and the safety floor, and a yes for the run no longer covers a worse call after it.
- **All** — Unattended runs that repeat one call with the same result are stopped.
- **All** — Helpers and workflow agents go through the whole gate, in their session's mode, with its rules, grants and judge.
- **All** — Jobs are listed, waits match only new output, and a session sees only its own jobs.
- **All** — A command printing past 4 MiB is saved whole for `read_file`.
- **All** — Chat-app, web and media calls reuse one warm connection each; cache misses are named with the part that changed.
- **All** — Inline think blocks stream as reasoning; `read_file` shows notebooks as cells.
- **App** — The window reads a session in only when it is opened, follows what the terminal writes to sessions, and highlights eight languages instead of twenty-seven.
- **App** — Session search skips sessions that cannot hold the words, and a reply that moves the window no longer waits for its summary.
- **App** — The window uses far less memory: pictures are read no larger than shown and let go of after, sessions read in by runs, chat apps or the terminal are let go of too, and switching profile reads only the session it opens.
- **Terminal** — The installed terminal is `letronnacli.exe`, with the logo; `letronna` still starts it.

- **App** — The window uses far less processor: a running reply redraws at 25 frames a second, not 120; a hidden or minimized window and the tray stay quiet; the browser panel decodes only pictures it shows; the approval card and the model list are worked out once.
- **App** — Selecting text no longer leaves 40–80 MB with the window, and video stills and a helper's transcript are let go of once not shown.
- **App** — Night is calmer: a midnight frame around a dark ink page.
- **All** — A command that exits non-zero counts as a failed step, and a web search row shows what was searched for.
- **All** — A workflow's tokens count up while its agents' replies stream.
### Fixed
- **App** — Deleting the last session no longer crashes the window, a new session no longer inherits a deleted one's pictures, and a finished empty reply no longer shows Thinking forever.
- **App** — New session goes to the top, even when it reuses an empty one.
- **App** — Closing settings or the browser panel gives their pictures back to the GPU.
- **App** — New session opens the empty one instead of piling up more, the last session in the sidebar scrolls fully into view, and right-clicking a session no longer draws a black halo.
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
- **All** — Follow-ups and notices sent mid-turn keep the turn's request in view.
- **All** — `/btw` shares the session's cache and leaves no stale locks.
- **App** — The window opens again on a session that has messages.
- **Terminal** — A bad update is put back, and a good one is kept.
- **Terminal** — A Ctrl-C of `letronna -p` ends the command it was running.

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
