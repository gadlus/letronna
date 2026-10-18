# Settings and data

## The data folder

`%LOCALAPPDATA%\Letronna\` on Windows, `~/Library/Application Support/Letronna/` on macOS, `~/.local/share/Letronna/` on Linux. `AGENT_DATA_DIR` overrides all three, and `letronna doctor` prints the one in use. The window and the terminal share it.

Letronna's file tools do not reach this folder, so the user edits these files themselves, or changes them through the window and the slash commands. Edit `settings.json` with Letronna closed: an open window or terminal saves over it.

- `settings.json`: the gateway and model, and everything else shared between sessions (below).
- `sessions/<id>.jsonl`: a file per session; `sessions/index.json` caches the list. `chats.json` is the older single-file store, converted once.
- `keys.json`: API keys per gateway. On macOS and Windows they are moved to the Keychain or Credential Manager (under Letronna), with the sign-in and the chat apps' tokens, and `vault.json` lists which.
- `account.json`: the Letronna sign-in on this computer.
- `SOUL.md`: who Letronna is, in the user's words; it replaces the first line of the prompt.
- `USER.md` and `MEMORY.md`: what Letronna knows about the user and its own notes, kept with its memory tool and the user's to edit (`/memory edit user` or `memory`).
- `mcp.json`: MCP servers under `"mcpServers"`, in the shape Claude Desktop and Cursor use: `command`, `args`, `env` and `cwd` for one run here, `url` and `headers` for one elsewhere, plus `autoApprove` (tool names, or `"readOnly"`), `"disabled": true`, `includeTools`, `excludeTools`, `timeout_ms`, `startup_timeout_ms` and `${VAR}` or `${VAR:-default}` from the environment. `letronna mcp add` writes it.
- `lsp.json`: language servers that check the files Letronna edits, each with `command`, `extensions` and the `projects` it serves.
- `hooks.json`: the user's commands run before and after tool calls, when a turn ends and when a card waits, in Claude Code's hook format.
- `permissions.json`: rules that allow, ask about or deny a tool by what it is called with, everywhere or for one project folder.
- `search.json`: the search service set up with `/search services` or Settings, Search.
- `messaging.json`: the chat apps (see `chat-apps.md`). `schedules.json`: the scheduled jobs.
- `skills/` the user's skills, with `skills-off.json` naming those switched off; `commands/` slash commands as `name.md`; `styles/` reply styles as Markdown; `agents/` helper types as Markdown; `plugins/` with `plugins.json`.
- `media/`: images, speech and video made with no project attached, and browser screenshots. `browser/`: the shared browser's profile; deleting it signs out of everything there.
- `logs/` and `crashes/`: what `letronna doctor` points at.

## settings.json

Most of it is set in the window's Settings or with a slash command (`mode`, `effort`, `style`, `thinking`, `fallbacks` with /fallback, `keep_days` with /retention, `memory_review` with /memory-review, `side_model` with /model side, `verify_edits` with /verify, `voice` with /voice, `notifications` with /notify, `tool_search`, `jev_endpoint` and `jev_model` (how find_tools ranks: auto, jev or names, and where Jev is asked) with /tool-search, `manual_updates` with Update automatically in Settings, About). These are edited by hand:

- `context_cap`: the most tokens a request carries besides the reply; 0 keeps the default of 64,000.
- `computer_use`: `true` offers the computer tool, which sees and uses other apps' windows, asking once for each app.
- `keys_in`: `"files"` keeps keys in `keys.json` and the other files instead of the Keychain or Credential Manager.
- `disabled_tools`: tools never offered nor run, by name, an MCP server's as `mcp__server__tool`; the Tools and MCP tabs of Capabilities switch them too.
- `helper_model`, `helpers_at_once` (8 unless set), `helper_depth` (2) and `helper_budget` (500k tokens) for helpers.

## Environment variables

`AGENT_DATA_DIR` (the data folder), `LETRONNA_API` (the Letronna gateway's address), `LETRONNA_READ_TIMEOUT` (seconds a reply may stay silent, 30 to 3600, 300 unless set), `LETRONNA_BROWSER` (the browser's path), `LETRONNA_BROWSER_URL` (a browser elsewhere: a cloud service's `wss://` address, or `http://host:9222`), `LETRONNA_BROWSER_HEADLESS=0` (gives the browser a window).

For a -p run, saving none of it: `LETRONNA_DEADLINE` (the seconds a question may take, or with `--input-format stream-json` the whole run: at 80% it is told to write its outputs, and at the end it stops), `AGENT_BASE_URL` (an OpenAI-compatible server to ask instead of the gateway set up, unless `--gateway` names one), `AGENT_API_KEY` (the key to send it) and `AGENT_MODEL` (the model to ask, unless `--model` names one).

## Profiles

A profile is a data folder of its own, for a separate identity: its own sessions, keys, settings, skills and memory. Default is the data folder itself, and every other one is `profiles/<name>` inside it. In the window, Profile in the account menu lists them to switch between, and New profile makes one, empty or cloned from another (the browser's profile is not cloned). Switching is refused while a reply, a run, a helper or a background job is going. Letronna opens on Default each time it starts; the terminal uses Default unless `AGENT_DATA_DIR` points at a profile's folder.

## A project's own files

- `AGENTS.md` (or `CLAUDE.md`) in the project folder and each folder above it up to the git root: the project's instructions. `/init` writes or fills one in. An `AGENTS.local.md` (or `CLAUDE.local.md`) beside one holds the user's own rules kept out of git, read after it.
- `.agents/skills` or `.claude/skills`: the project's skills; the user's own of the same name win.
- `.letronna/commands` or `.claude/commands`: slash commands. `.letronna/agents` or `.claude/agents`: helper types.
- `.letronna/permissions.json`: rules in the shape of `permissions.json`, but only deny and ask rules count.

A project cannot add hooks, MCP servers or language servers, since whoever wrote a repository may not be whoever runs it.
