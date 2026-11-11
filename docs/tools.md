# Tools

A tool that changes something asks before it runs, unless the current [mode](app-and-terminal.md#modes) allows it. Plan mode offers only read-only tools. Any tool can be switched off, and tools that cannot work are not offered.

## Files and shell

Letronna reads, searches, writes and edits files, and shows a diff before each change. It also runs shell commands, and long-running ones move to the background (`/jobs`). If a [language server](extending.md#language-servers) is configured, errors an edit introduces reach the model on its next step.

## Web search

The **Web** toggle uses the gateway's own search, where there is one. A search service works with any gateway; set one up under Settings › Search or with `/search services`:

| Service | Needs |
|---|---|
| Brave, Tavily, Exa | An API key |
| SearXNG | Your own server, with `json` in its `formats` |
| DuckDuckGo | Nothing. Unofficial and may be rate-limited. |

With none chosen, `search_web` searches through the gateway where it sells search, such as Letronna's or a gateway whose model list has search models, and falls back to DuckDuckGo in the same search when the gateway cannot answer. The approval card says who pays.

Letronna's search comes in three settings, under Settings › Search or with `/search letronna auto|free|paid`:
- **Auto** (the default) tries its free search first, and uses the paid one, from your credit, only when the free one finds nothing.
- **Free** uses its free search alone, with no credit used.
- **Paid** uses the paid search alone, which gives the steadiest results. A chosen service is the only one used: choose DuckDuckGo alone (Use alone in Settings › Search, or `/search services`) to keep every search off the gateway's paid search.

## Browser

`browse` reads pages, takes screenshots and reads the console. `use_page` clicks, types and runs scripts. Both drive the Chrome, Edge or Brave already installed, over the DevTools protocol, so nothing is downloaded.

- **What the model sees**: the page's accessibility tree, with refs such as `e12` to act on.
- **Profiles**: each session gets its own private browser. Settings › Privacy can switch this to one shared, persistent profile.
- **In the window**: the browser shows live beside the chat, and you can use it too. Sign in, get past a check, or point at an element to comment on it.
- **A page handed to you**: for what only you can do on a page, a password, a code sent to you or a check that you are human, the model does not ask for your password and types none. It hands the page over (`browse`'s `hand_over`) and waits, with a card that says what to do. You do it on the page, in the window beside the chat, the browser's own window, or from the Letronna app when the run is in Letronna Cloud, and answer the card when you are done. It is asked in every mode, and no "yes for all" covers it. With nobody there, or no way for you to reach the page, the model is told so and says what was left undone.
- **Remote browsers**: a remote Chrome or a cloud browser also works; see `LETRONNA_BROWSER_URL` in [Configuration](configuration.md).
- **Safety**: an address carrying one of your keys is never opened, and neither is a cloud metadata service.

For a light headless browser on macOS and Linux, add [Lightpanda](https://github.com/lightpanda-io/browser) as an MCP server:

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

## Images, speech and video

These tools run on Letronna's hosted models whatever gateway the chat uses. They are paid from your credit, so they ask first.

| Tool | Makes | Billed per |
|---|---|---|
| `make_image` | An image from a description | Image |
| `edit_image` | Edits to existing images, with an optional mask; uses the chat's own gateway | Image |
| `speak` | Speech from text | Character |
| `transcribe` | Text from audio or video, up to 25 MB | Minute |
| `make_video` | A short MP4 | Second |

Output is saved to `media/` in the project. `/model image|speech|video` picks a model for each; otherwise the cheapest working one is used.

## Computer use

On macOS, and on Linux under X11, the agent can operate desktop apps. It asks once for each app.

## Helpers and workflows

- **Helpers** are sub-agents. They:
  - work side by side and share the session's task list
  - can be forked from the conversation, and can wait idle for more work
  - can each work in their own git worktree
  - share one token budget across the whole tree
  - can be watched, steered and stopped
- **Workflows** are sandboxed JavaScript scripts that run many agents. Letronna can write one and run it once you approve it. `@crew` opts a single message into orchestration.

## Memory

`USER.md` holds what Letronna knows about you, and `MEMORY.md` holds its own notes. Both are kept in the data folder by the `memory` tool, and you can edit them. Each project also gets a memory of its own, which refuses keys and planted instructions.
