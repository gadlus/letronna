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

With none chosen, `search_web` searches through the gateway where it sells search, such as Letronna's (paid from your credit) or a gateway whose model list has search models, and falls back to DuckDuckGo in the same search when the gateway cannot answer. The approval card says who pays. A chosen service is the only one used: choose DuckDuckGo alone (Use alone in Settings › Search, or `/search services`) to keep every search off the gateway's paid search.

## Browser

`browse` reads pages, takes screenshots and reads the console. `use_page` clicks, types and runs scripts. Both drive the Chrome, Edge or Brave already installed, over the DevTools protocol, so nothing is downloaded.

- **What the model sees**: the page's accessibility tree, with refs such as `e12` to act on.
- **Profiles**: each session gets its own private browser. Settings › Privacy can switch this to one shared, persistent profile.
- **In the window**: the browser shows live beside the chat, and you can use it too. Sign in, get past a check, or point at an element to comment on it.
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
