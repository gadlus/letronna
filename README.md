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
- Gateways: Letronna (hosted models on your account), OpenAI, Anthropic, OpenRouter, Ollama, or a custom base URL
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

`LETRONNA_DISPOSABLE=1` declares the machine thrown away after the run, as a benchmark's container is. Any run in Bypass that nobody watches, such as `letronna -p --yes`, `letronna mcp serve --yes` or a Bypass schedule, then does commands that are hard to take back, such as `rm -r` or `git reset --hard`, instead of refusing them. It still refuses those Letronna never runs, such as deleting the home folder, and any that sends a `.env` file off the computer or reaches a cloud machine's metadata service. Never set it on a machine whose files matter.

## Gateways

| Gateway     | Base URL                          | Key      |
|-------------|-----------------------------------|----------|
| Letronna    | `https://api.letronna.ai/v1`      | sign in  |
| OpenAI      | `https://api.openai.com/v1`       | required |
| OpenRouter  | `https://openrouter.ai/api/v1`    | required |
| Ollama      | `http://localhost:11434/v1`       | none     |
| Custom      | anything OpenAI-compatible        | optional |

The Letronna gateway's address follows `LETRONNA_API`, for working against a local API.

A base URL over plain HTTP has to be on this computer. `LETRONNA_ALLOW_HTTP=1` lets it be anywhere on the local network too, such as `http://host.docker.internal:4000/v1` or a proxy named on a container's network, with the key sent unencrypted across that network.

`letronna -p` can be pointed at a server with nothing set up, as in a container: `AGENT_BASE_URL` sends that run to it as Custom, with `AGENT_API_KEY` as its key and `AGENT_MODEL` (or `--model`) as the model. None of it is saved, and `--gateway` goes back to the gateways set up.

Model lists come from `GET {base}/models`. Streaming uses SSE chat completions.

A reply that stays silent for five minutes is given up on. `LETRONNA_READ_TIMEOUT` sets that silence in seconds, from 30 to 3600, for a gateway or proxy that holds each reply back until it is whole.

## Web search

The Web toggle is the gateway's own search, and only some gateways have one. A search service of your own works with any of them: set one up in Settings under Search, or with `/search services` in the terminal, and the agent gets a `search_web` tool that asks before each search. Until a service is chosen, searches go to DuckDuckGo without a key, each one asked first. DuckDuckGo has no public search API, so this is a setting, on unless switched off: `/search keyless` in the terminal, or `"keyless_search": false` in `settings.json`. Switched off with no service chosen, `search_web` says how to switch it back on or add a key instead of searching.

| Service  | Official address                                  | Needs               |
|----------|---------------------------------------------------|---------------------|
| Brave    | `https://api.search.brave.com/res/v1/web/search`  | an API key          |
| Tavily   | `https://api.tavily.com/search`                   | an API key          |
| Exa      | `https://api.exa.ai/search`                       | an API key          |
| SearXNG  | your own server, such as `http://mac-mini:8888`   | its json format on  |
| DuckDuckGo | `https://html.duckduckgo.com/html`              | nothing; unofficial, may be rate-limited |

The one set up last is used. Each is reached at its official address unless you give another, such as a proxy; Reset, or `official` in the terminal, goes back to it. A SearXNG answers searches from Letronna only with `json` among the `formats` in its `settings.yml`. Keys are kept with the gateways' (see Local data).

## Images, speech and video

Signed in, the agent can also make things on Letronna's hosted models, whichever gateway the chat itself uses:

| Tool         | Makes                                                     | Paid by       |
|--------------|-----------------------------------------------------------|---------------|
| `make_image` | an image from a description                               | the image     |
| `speak`      | text read aloud                                           | the character |
| `transcribe` | the words in an audio or video file, up to 25 MB          | the minute    |
| `make_video` | a short MP4 from a description, in a few minutes          | the second    |

`edit_image` changes pictures instead of making one: one or more from the project, or already in the conversation, with an optional mask marking where to redraw. It goes to the chat's own gateway, so it also works on OpenAI or any OpenAI-compatible server with an `/images/edits` route, paid there; on Letronna it is paid by the image. A picture is never taken from outside the project or Letronna's media folder, nor from a file of keys, and one the route would refuse for its size is stopped before it is uploaded.

Each one asks first, since it spends your credit, and none is offered in plan mode or in a shared chat. Unless a model is named, the cheapest one working is used; the prices are on the dashboard's models page. What is made is saved in `media/` in the project folder, or in the data folder when no project is attached. The window shows a made image in the chat, and opens any made file.

## Browser

The agent can use a real web browser: the Chrome, Edge or Brave already installed (Edge comes with Windows), driven over the DevTools protocol. Nothing is downloaded and no Node or Python is needed.

| Tool       | Does                                                                            |
|------------|---------------------------------------------------------------------------------|
| `browse`   | opens a page and lists it, scrolls, goes back, switches tabs, reads the console, takes a screenshot |
| `use_page` | clicks, types, presses keys, picks from dropdowns, runs JavaScript              |

A page reaches the model as its accessibility tree, one element a line with a ref such as `e12` to click or type into; screenshots show in the chat, and a model that takes images sees the newest one. Each session browses in a browser of its own, as a separate agent would: a private one, as an incognito window is, with its own sign-ins and tabs, forgotten when Letronna quits. The window's browser panel shows the one of the session on screen. **One browser for every session** (Settings, Privacy) shares a single one instead, on its own profile in the data folder, never your usual one, where a site signed in to once stays signed in. It is one browser either way, the sessions' private ones living inside it, and at most four private ones are kept at once. It has no window of its own, and closes when Letronna exits, or once nothing has used it for a while (15 seconds after the panel closes, 3 minutes after the agent's last step), to start again when next needed with each session's page and sign-ins as they were; `LETRONNA_BROWSER_HEADLESS=0` gives it one, for the terminal or for a site that turns away a browser without one. `LETRONNA_BROWSER` picks the browser by its path.

The browser can be elsewhere instead: a cloud browser, or Chrome on another machine. Give its address under **Browser elsewhere** (Settings, Privacy), or in `LETRONNA_BROWSER_URL`: a cloud service's connect address with its key in it as the service gives it (`wss://…?token=…` or `?apiKey=…`), or `http://its-address:9222` for a Chrome started with `--remote-debugging-port=9222` (through an SSH tunnel is safest; the port lets anyone who reaches it drive the browser). The address is kept with the keys, since it may carry a key. Everything works the same there, the panel included, over the one connection such a browser gives, with some differences: it has no window here; it draws at its own screen's scale (start it with `--force-device-scale-factor` to match yours); it cannot open addresses on this computer, as `localhost` is its own; each step crosses the network, so it is as quick as the link is; and when Letronna quits or the browser goes idle, it is let go of, with the private browsers made in it, rather than closed, as it may be somebody else's. A cloud service's session ends there.

Both tools ask first, as a web address can carry things out as well as bring them in. Looking is allowed in plan mode and acting is not; neither is offered in a shared chat. An address carrying one of your own keys or tokens is never opened, nor a cloud machine's metadata service.

The window shows the browser beside the chat, live, while the agent uses it or once **Browser** in the menu is clicked, which opens on the session's page, or on Google: a pointer goes to what the agent is about to click or type into, outlines it, and lands as the action happens, with a line saying what it is doing. It is yours to use too: type an address, go back and forward, reload, and click, type, scroll, copy and paste on the page. A question the page asks waits there for your answer, and a sign-in or a site's check is got past there. The panel's view menu lays the page out in portrait, as a phone does, or in landscape, as a desktop browser does in a wider panel, and zooms it (also Ctrl and +, -, 0, or the wheel); drag the panel's edge to widen it, and its window button gives the browser a window of its own, with the same page and sign-ins, until clicked again. The page is drawn as sharp as the screen: the browser's own pictures of it, sent only when it changes, 30 a second at most, and made sharp once it stands still, so a page standing still costs nothing, and none are sent once the panel is closed. A narrow window floats the panel in a corner, showing the page smaller. With **Show the page as the agent browses** (Settings, Privacy) off, the agent browses unseen, lighter on memory, and the panel opens only from the menu. The terminal shows none of this and waits for no pointer.

## Local data

`%LOCALAPPDATA%\Letronna\` on Windows, `~/Library/Application Support/Letronna/` on macOS, `~/.local/share/Letronna/` on Linux. `AGENT_DATA_DIR` overrides all three.

- `chats.json` sessions and the selected gateway, endpoint and model
- `keys.json` API keys per gateway, plain text on your disk only; on macOS and Windows, in the usual data folder, they are moved to the Keychain or Credential Manager (under Letronna), and so are the sign-in and chat-app tokens, with `vault.json` listing which. On Windows that keeps them out of backups and sync but not from other programs you run, as Credential Manager lets any of them read them. `"keys_in": "files"` in `settings.json` keeps them in the files instead, moving them all back at the next start; set it and start Letronna once before going back to a version older than this, which only reads the files. `letronna doctor` says where they are
- `account.json` the Letronna sign-in for this computer
- `SOUL.md` who Letronna is, in your words; replaces the built-in first line of the prompt (optional, yours to write)
- `USER.md` what Letronna knows about you, and `MEMORY.md` its own notes; it keeps both with its `memory` tool, and you can edit them. Both go into every conversation except chats other people can read, so they stay small (1,375 and 2,200 characters)
- `mcp.json` MCP servers whose tools Letronna may use (optional, see below)
- `lsp.json` language servers that check the files Letronna edits (optional, see below)
- `search.json` the search service in use, and any address given for one
- `media/` images, speech and video made with no project attached, and browser screenshots
- `browser/` the browser's own profile: delete it to sign out of everything there

With a project folder attached, its `AGENTS.md` (or `CLAUDE.md`) is read as the project's instructions, and so is the one in each folder above it up to the repository's root. An `AGENTS.local.md` (or `CLAUDE.local.md`) beside one, for rules of your own that you keep out of git, is read after it.

Chat text is sent to the gateway you selected and nowhere else.

## MCP servers

Letronna can use the tools of any [MCP](https://modelcontextprotocol.io) server that runs as a local command. List them in `mcp.json` in the data folder, in the same shape Claude Desktop and Cursor use, so an existing config can be copied over:

```json
{
  "mcpServers": {
    "everything": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-everything"],
      "autoApprove": ["echo"]
    }
  }
}

A model whose list says nothing of its limits and that Letronna does not know, as behind vLLM or a LiteLLM proxy, gets 6,000 tokens of the conversation and replies of 8,192. `"context_window"` and `"max_output"` in `settings.json`, set with Letronna closed, or `LETRONNA_CONTEXT_WINDOW` and `LETRONNA_MAX_OUTPUT` for one run, give it its real ones; a limit the provider's error names still lowers them.

## Web search

The Web toggle is the gateway's own search, and only some gateways have one. A search service of your own works with any of them: set one up in Settings under Search, or with `/search services` in the terminal, and the agent gets a `search_web` tool that asks before each search.

| Service  | Official address                                  | Needs               |
|----------|---------------------------------------------------|---------------------|
| Brave    | `https://api.search.brave.com/res/v1/web/search`  | an API key          |
| Tavily   | `https://api.tavily.com/search`                   | an API key          |
| Exa      | `https://api.exa.ai/search`                       | an API key          |
| SearXNG  | your own server, such as `http://mac-mini:8888`   | its json format on  |

The one set up last is used. Each is reached at its official address unless you give another, such as a proxy; Reset, or `official` in the terminal, goes back to it. A SearXNG answers searches from Letronna only with `json` among the `formats` in its `settings.yml`. Keys are kept with the gateways' (see Local data).

## Images, speech and video

Signed in, the agent can also make things on Letronna's hosted models, whichever gateway the chat itself uses:

| Tool         | Makes                                                     | Paid by       |
|--------------|-----------------------------------------------------------|---------------|
| `make_image` | an image from a description                               | the image     |
| `speak`      | text read aloud                                           | the character |
| `transcribe` | the words in an audio or video file, up to 25 MB          | the minute    |
| `make_video` | a short MP4 from a description, in a few minutes          | the second    |

`edit_image` changes pictures instead of making one: one or more from the project, or already in the conversation, with an optional mask marking where to redraw. It goes to the chat's own gateway, so it also works on OpenAI or any OpenAI-compatible server with an `/images/edits` route, paid there; on Letronna it is paid by the image. A picture is never taken from outside the project or Letronna's media folder, nor from a file of keys, and one the route would refuse for its size is stopped before it is uploaded.

Each one asks first, since it spends your credit, and none is offered in plan mode or in a shared chat. Unless a model is named, the cheapest one working is used; the prices are on the dashboard's models page. What is made is saved in `media/` in the project folder, or in the data folder when no project is attached. The window shows a made image in the chat, and opens any made file.

## Browser

The agent can use a real web browser: the Chrome, Edge or Brave already installed (Edge comes with Windows), driven over the DevTools protocol. Nothing is downloaded and no Node or Python is needed.

| Tool       | Does                                                                            |
|------------|---------------------------------------------------------------------------------|
| `browse`   | opens a page and lists it, scrolls, goes back, switches tabs, reads the console, takes a screenshot |
| `use_page` | clicks, types, presses keys, picks from dropdowns, runs JavaScript              |

A page reaches the model as its accessibility tree, one element a line with a ref such as `e12` to click or type into; screenshots show in the chat, and a model that takes images sees the newest one. Each session browses in a browser of its own, as a separate agent would: a private one, as an incognito window is, with its own sign-ins and tabs, forgotten when Letronna quits. The window's browser panel shows the one of the session on screen. **One browser for every session** (Settings, Privacy) shares a single one instead, on its own profile in the data folder, never your usual one, where a site signed in to once stays signed in. It is one browser either way, the sessions' private ones living inside it, and at most four private ones are kept at once. It has no window of its own, and closes when Letronna exits, or once nothing has used it for a while (15 seconds after the panel closes, 3 minutes after the agent's last step), to start again when next needed with each session's page and sign-ins as they were; `LETRONNA_BROWSER_HEADLESS=0` gives it one, for the terminal or for a site that turns away a browser without one. `LETRONNA_BROWSER` picks the browser by its path.

The browser can be elsewhere instead: a cloud browser, or Chrome on another machine. Give its address under **Browser elsewhere** (Settings, Privacy), or in `LETRONNA_BROWSER_URL`: a cloud service's connect address with its key in it as the service gives it (`wss://…?token=…` or `?apiKey=…`), or `http://its-address:9222` for a Chrome started with `--remote-debugging-port=9222` (through an SSH tunnel is safest; the port lets anyone who reaches it drive the browser). The address is kept with the keys, since it may carry a key. Everything works the same there, the panel included, over the one connection such a browser gives, with some differences: it has no window here; it draws at its own screen's scale (start it with `--force-device-scale-factor` to match yours); it cannot open addresses on this computer, as `localhost` is its own; each step crosses the network, so it is as quick as the link is; and when Letronna quits or the browser goes idle, it is let go of, with the private browsers made in it, rather than closed, as it may be somebody else's. A cloud service's session ends there.

Both tools ask first, as a web address can carry things out as well as bring them in. Looking is allowed in plan mode and acting is not; neither is offered in a shared chat. An address carrying one of your own keys or tokens is never opened, nor a cloud machine's metadata service.

The window shows the browser beside the chat, live, while the agent uses it or once **Browser** in the menu is clicked, which opens on the session's page, or on Google: a pointer goes to what the agent is about to click or type into, outlines it, and lands as the action happens, with a line saying what it is doing. It is yours to use too: type an address, go back and forward, reload, and click, type, scroll, copy and paste on the page. A question the page asks waits there for your answer, and a sign-in or a site's check is got past there. The panel's view menu lays the page out in portrait, as a phone does, or in landscape, as a desktop browser does in a wider panel, and zooms it (also Ctrl and +, -, 0, or the wheel); drag the panel's edge to widen it, and its window button gives the browser a window of its own, with the same page and sign-ins, until clicked again. The page is drawn as sharp as the screen: the browser's own pictures of it, sent only when it changes, 30 a second at most, and made sharp once it stands still, so a page standing still costs nothing, and none are sent once the panel is closed. A narrow window floats the panel in a corner, showing the page smaller. With **Show the page as the agent browses** (Settings, Privacy) off, the agent browses unseen, lighter on memory, and the panel opens only from the menu. The terminal shows none of this and waits for no pointer.

## Local data

`%LOCALAPPDATA%\Letronna\` on Windows, `~/Library/Application Support/Letronna/` on macOS, `~/.local/share/Letronna/` on Linux. `AGENT_DATA_DIR` overrides all three.

- `chats.json` sessions and the selected gateway, endpoint and model
- `keys.json` API keys per gateway, plain text on your disk only; on macOS and Windows, in the usual data folder, they are moved to the Keychain or Credential Manager (under Letronna), and so are the sign-in and chat-app tokens, with `vault.json` listing which. On Windows that keeps them out of backups and sync but not from other programs you run, as Credential Manager lets any of them read them. `"keys_in": "files"` in `settings.json` keeps them in the files instead, moving them all back at the next start; set it and start Letronna once before going back to a version older than this, which only reads the files. `letronna doctor` says where they are
- `account.json` the Letronna sign-in for this computer
- `SOUL.md` who Letronna is, in your words; replaces the built-in first line of the prompt (optional, yours to write)
- `USER.md` what Letronna knows about you, and `MEMORY.md` its own notes; it keeps both with its `memory` tool, and you can edit them. Both go into every conversation except chats other people can read, so they stay small (1,375 and 2,200 characters)
- `mcp.json` MCP servers whose tools Letronna may use (optional, see below)
- `lsp.json` language servers that check the files Letronna edits (optional, see below)
- `search.json` the search service in use, and any address given for one
- `media/` images, speech and video made with no project attached, and browser screenshots
- `browser/` the browser's own profile: delete it to sign out of everything there

With a project folder attached, its `AGENTS.md` (or `CLAUDE.md`) is read as the project's instructions, and so is the one in each folder above it up to the repository's root. An `AGENTS.local.md` (or `CLAUDE.local.md`) beside one, for rules of your own that you keep out of git, is read after it.

Chat text is sent to the gateway you selected and nowhere else.

## MCP servers

Letronna can use the tools of any [MCP](https://modelcontextprotocol.io) server that runs as a local command. List them in `mcp.json` in the data folder, in the same shape Claude Desktop and Cursor use, so an existing config can be copied over:

```json
{
  "mcpServers": {
    "everything": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-everything"],
      "autoApprove": ["echo"]
    }
  }
}
```

Servers start when Letronna does, and their tools appear as `mcp__<server>__<tool>` once they answer; `letronna -p` waits up to 15 seconds for them before it asks. Every call asks first unless the tool is listed in `autoApprove`; `"disabled": true` leaves a server off. A server that stops or cannot start is started again: one run here once, one reached by URL up to five times, 1 s later and doubling each time; then `/mcp reconnect <name>` starts it again. A server that says its tools changed is asked for them again. After editing `mcp.json`, `/mcp reload` or Reload in Capabilities › MCP in the window starts the servers added, stops those removed and starts again those changed, leaving the rest running. Stopping a server run here, whether by Reconnect, switching it off, a reload or quitting, ends everything it started too, such as the node process npx runs.

A server that runs elsewhere -- a hosted one, or one on another machine -- is given by its address instead of a command, with any headers it needs, such as a key:

```json
{
  "mcpServers": {
    "deepwiki": { "url": "https://mcp.deepwiki.com/mcp" },
    "hosted": {
      "url": "https://example.com/mcp",
      "headers": { "Authorization": "Bearer your-key" }
    }
  }
}
```

It is spoken to over HTTP as the protocol now has it (streamable HTTP: an address that usually ends in `/mcp`). The older way, an address ending in `/sse`, is not supported.

A server's `command`, `args`, `cwd`, `url`, `env` and `headers` may name `${VAR}` or `${VAR:-default}`, filled in from Letronna's environment when the server starts, so a key need not be written into the file; a variable that is not set and has no default keeps that server from starting. A server also takes:

- `timeout_ms`: how long one call may take (2 minutes unless set), and `startup_timeout_ms`: how long it may take to start and list its tools (1 minute unless set); a day at most.
- `includeTools` and `excludeTools`: tool names, where `*` matches any run of characters, to offer the model only some of a server's tools; an excluded tool is never offered.
- `"autoApprove": "readOnly"`: every tool the server marks read-only, not destructive and closed to the world runs without asking. A server run here is taken at its word on that already; this takes a server elsewhere at its word too.
- `cwd`: the folder a server run here starts in, from the data folder when relative.
- `"env_scrub": true`: start it with only `PATH`, `HOME`, the language, the temp folder and, on Windows, the system folders, plus its own `env`, rather than all of Letronna's environment.

```json
{
  "mcpServers": {
    "hosted": {
      "url": "https://example.com/mcp",
      "headers": { "Authorization": "Bearer ${HOSTED_KEY}" },
      "excludeTools": ["delete_*"],
      "autoApprove": "readOnly"
    }
  }
}
```

### A lighter browser

On Linux and macOS, [Lightpanda](https://github.com/lightpanda-io/browser) is a browser made for agents that draws nothing, which makes it far lighter than Chrome or Edge with no window: on a Mac mini, about 20 MB idle and under 60 MB with Wikipedia open, where a hidden Edge takes over 160 MB with a small page. It comes with an MCP server, so with `lightpanda` on the PATH this is all it needs (it sends usage telemetry unless told not to, as here):

```json
{
  "mcpServers": {
    "lightpanda": {
      "command": "lightpanda",
      "args": ["mcp"],
      "env": { "LIGHTPANDA_DISABLE_TELEMETRY": "true" }
    }
  }
}
```

Its tools open pages and read them as text or markdown, find, click and fill what is on them, run scripts, and keep private sessions apart. Drawing nothing, it has nothing to show, and its screenshots are the page's text laid out; a site that needs what it does not implement yet can fail.

There is no Windows build, and inside WSL or Docker its virtual machine costs more than it saves. It can run on another machine instead, a Mac or Linux box on the same Tailscale network say, over SSH with a key that logs in without asking; browsing then costs this one only the connection:

```json
"lightpanda": {
  "command": "ssh",
  "args": ["you@mac-mini", "LIGHTPANDA_DISABLE_TELEMETRY=true exec ~/.local/bin/lightpanda mcp"]
}
```

## Language servers

After Letronna writes or edits a file, a language server can check it, and the errors the edit brought in reach the model with its next request, without a build. Nothing runs unless you list servers in `lsp.json` in the data folder; each takes the `command`, `args`, `env` and `cwd` an MCP server does, the file `extensions` it serves, and the `projects` it is for:

```json
{
  "servers": {
    "rust": {
      "command": "rust-analyzer",
      "extensions": ["rs"],
      "projects": ["/home/me/code/letronna"]
    }
  }
}
```

A server serves only the project folders listed for it. Most run the project's own code as they check it -- rust-analyzer its build scripts and proc macros, a TypeScript server the project's own compiler -- so list only projects you would build yourself: there, an edit Letronna makes, in Accept edits mode too, can run that code on the next check.

A server starts in the project folder the first time a file it serves is written there, serves every session in that project, and is shut down after ten minutes without a write and when Letronna quits. Only errors are told, not warnings, and only those that were not there before the edit. A server that cannot start or stops is mentioned once and not started again until the next session. Projects cannot add servers of their own, since a server is a command and whoever wrote a repository may not be whoever runs it.

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
