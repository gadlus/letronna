# Architecture

One Cargo workspace with three crates. Anything that is not about drawing belongs in `core`, so the window and the terminal behave the same way.

```
crates/core   the agent: chat and streaming, gateways, tools, sessions,
              settings, memory, skills, MCP, plugins, hooks, the cloud client
crates/cli    `letronna`: the terminal UI, setup, install, serve, ACP, MCP server
crates/gui    `letronna-gui`: the GPUI window, its views (src/ui), tray,
              install and per-OS integration (src/platform)
vendor/       the GPUI snapshot the window builds against
scripts/      installers, release, icon sync, macOS bundling, UI capture
bench/        speed baseline and Harness-Bench results
```

## A turn

`agent.rs` runs one turn as a state machine:

1. **Ask**: `chat.rs` builds the request (system prompt, project instructions, memory, tool list) and streams the reply over SSE.
2. **Run a tool**: every call passes the gate (rules, mode and approvals in `gate.rs` and `approvals.rs`), then hooks, then the tool itself in `tools.rs`.
3. **Ask the user**: approvals and questions become cards in the window, or prompts in the terminal.
4. **Done**: the turn ends when the model stops calling tools. A run nobody watches is also checked against its checklist and the outputs it was asked for.

Sessions are append-only JSONL, one file per session (`store.rs`), with an index so that listing thousands of sessions stays fast. When the context fills up, `compact.rs` summarises older turns.

## Adding things

- **A tool**: a struct implementing `Tool` in `tools.rs`, plus a line in `all()`.
- **A slash command**: an entry in `bar::COMMANDS`, plus an arm in `commands::command`.

See [CONTRIBUTING.md](../CONTRIBUTING.md) for the file-by-file map and the conventions.
