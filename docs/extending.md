# Extending

All of these files live in the [data folder](data-and-privacy.md). A project can add its own instructions and skills, but never servers, plugins or hooks: those are programs, and whoever wrote a repository may not be the person running it.

## MCP servers

`mcp.json` follows the same shape that Claude Desktop and Cursor use, so you can copy an existing config across:

```json
{
  "mcpServers": {
    "everything": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-everything"],
      "autoApprove": ["echo"]
    },
    "hosted": {
      "url": "https://example.com/mcp",
      "headers": { "Authorization": "Bearer ${HOSTED_KEY}" },
      "excludeTools": ["delete_*"]
    }
  }
}
```

- Tools appear as `mcp__<server>__<tool>`. Each call asks first unless the tool is listed in `autoApprove`. `"autoApprove": "readOnly"` trusts the server's own read-only hints.
- Remote servers use streamable HTTP. The legacy SSE transport is not supported.
- Any string field can use `${VAR}` or `${VAR:-default}`.
- Other per-server options:
  - `timeout_ms` and `startup_timeout_ms`
  - `includeTools` and `excludeTools`
  - `cwd`
  - `env_scrub`
  - `disabled`
- Servers restart when they crash. After editing the file, `/mcp reload` applies the changes, and `/mcp reconnect <name>` restarts a single server.
- `letronna mcp serve` exposes Letronna itself as an MCP server.

## Language servers

`lsp.json` lists servers that check the files Letronna edits. The errors an edit introduces are fed back to the model, so it can fix them without running a build.

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

Each server runs only for the projects listed. Most language servers execute project code (build scripts, compilers), so list only projects you would build yourself. A server shuts down after ten minutes idle.

## Skills

A skill is a folder holding a `SKILL.md` (frontmatter, then instructions) and whatever files those instructions use. It is the same layout as [anthropics/skills](https://github.com/anthropics/skills).

- **Where they come from**:
  - your own skills, in `<data>/skills`
  - project skills, in `.agents/skills` or `.claude/skills`
  - a few built-in skills
- **Name clashes**: yours win.
- **Installing**: `/skills` installs from GitHub, a link or a folder, and switches skills on and off.
- **Skills the model writes**: they start switched off until you enable them.

## Plugins

A plugin is a git repository with a `plugin.json`. It can bring skills, slash commands, output styles, MCP servers, model providers and code in JavaScript or Python. `/plugins` adds, updates, removes, allows, shows settings and browses marketplaces.

`plugin.json` keys: `name`, `version`, `description`, `skills`, `commands`, `styles` (each a folder or a list of them), `mcpServers` (as in `mcp.json`), `providers` (OpenAI-compatible APIs, data only; see [providers.md](providers.md#from-a-plugin)), and `main`, `start`, `permissions` and `contributes` for code.

Every plugin is scanned before it is installed. Its MCP servers, providers and code are used only after you allow them, and you are asked again whenever an update changes them.

### Plugins that run code

A plugin whose `main` is a `.js` or `.py` file runs it: JavaScript on the engine Letronna already ships, Python on the Python already on the computer. Its code can add tools, block or annotate tool calls, follow turns, add slash commands that run code, keep settings, and show a status, a panel and notices. Nothing runs until something it brings is used. See [Plugins that run code](plugins.md), and the examples in [`examples/plugins`](../examples/plugins).

## Hooks

`hooks.json` uses the same shape as Claude Code's hooks, so existing scripts carry over:

- Events: before and after a tool call, at the end of a turn, and when an approval waits.
- Hooks read JSON on stdin.
- Exit code `2` blocks the action, with stderr as the reason.

A hook can block an action or keep a turn going. It can never allow something the approval gate stopped.

## Project instructions

`AGENTS.md` (or `CLAUDE.md`) is read from the project folder and from every folder above it up to the repository root. `AGENTS.local.md` is read after it, for personal rules kept out of git. `@path` lines pull in other files. `/init` writes a first draft.

## Editors (ACP)

`letronna acp` speaks the Agent Client Protocol on stdio, so Zed and JetBrains IDEs can run Letronna as their agent.

## Chat apps

Telegram, Discord and Slack can each reach your agent. Telegram pairs by a code, and only the owner gets answers. Replies come with Allow/Deny buttons, and files and voice notes work in both directions. Scheduled jobs report there too. `letronna serve` answers with no window open.

## The phone app

The Letronna app on your phone can use this computer. Turn it on in Messaging › Letronna app, choose Pair a phone, and scan the code with the app. Both screens then show six digits; choose Pair only if they match. The phone and the computer each keep a connection to Letronna's servers, which pass messages between them encrypted end to end, with keys made when you pair: the servers cannot read them, and nothing on the computer listens on the network. A phone's chats are ordinary sessions, in the sidebar here. It works in Default or Plan mode only, and every call that needs a yes asks on the phone, one call at a time. Remove a phone in the same place to cut it off.

If this computer and your phone are both on your Tailscale network, turn on Let phones connect straight here, under Over Tailscale on the same page. This computer then also listens on its tailnet address, port 47821 (`phone_port` in settings.json for another), and only there, so nothing off your tailnet reaches it. The pairing code then carries the address too, so a phone on the tailnet pairs over it, and one paired before learns it the next time it connects. Either way the phone tries the tailnet first from then on, falling back to the servers when it is not on it. What crosses is encrypted end to end the same way. The page says when Tailscale is not running, and Letronna picks it up when it starts.
