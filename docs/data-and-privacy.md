# Data and privacy

Everything Letronna keeps lives in one folder:

| OS | Data folder |
|---|---|
| Windows | `%LOCALAPPDATA%\Letronna\` |
| macOS | `~/Library/Application Support/Letronna/` |
| Linux | `~/.local/share/Letronna/` |

`AGENT_DATA_DIR` overrides it.

| File | Holds |
|---|---|
| `chats.json`, session files | Conversations, as append-only JSONL with one file per session, plus the selected gateway and model |
| `keys.json` | API keys for each gateway and search service |
| `account.json` | The Letronna sign-in for this machine |
| `settings.json` | Settings |
| `SOUL.md` | Optional. Who Letronna is, in your words; replaces the first line of the prompt. |
| `USER.md`, `MEMORY.md` | What it knows about you, and its own notes. You can edit both. |
| `mcp.json`, `lsp.json`, `hooks.json` | See [Extending](extending.md) |
| `skills/`, `plugins/` | Installed skills and plugins |
| `media/` | Media made with no project attached, browser screenshots, and pictures attached with `/image` |
| `pasted/` | A picture taken off the clipboard with Ctrl+V in the terminal, until `/image` attaches it |
| `browser/` | The shared browser profile. Delete it to sign out of everything. |
| `prompt-history.jsonl` | What you sent at the terminal's prompt, with its project folder, for ↑ and Ctrl+R in later sessions: the newest 1,000 for each folder, readable by you alone. Answers to questions, keys among them, are never kept, nor is anything typed in a private chat, whether `--private` or `/remember`'s list made it one, a `/branch` of one and one opened again with `/resume` included. Delete it to forget them. |
| `drafts/` | What is in the terminal's box while Ctrl+G has it open in your editor, readable by you alone; removed when the editor closes |

## Keys

On macOS and Windows, keys and tokens are moved to the Keychain or Credential Manager, and `vault.json` lists which ones. Setting `"keys_in": "files"` in `settings.json` keeps them in plain files instead. On Linux they stay in plain files.

Keys never reach the model. File tools cannot read the data folder, and the browser refuses addresses that carry a key. `letronna doctor` reports where the keys are stored.

## What leaves your machine

- **Chat text** goes only to the gateway of the selected model. With the Letronna gateway, that is `api.letronna.ai`, which records each request's model and cost for your usage page.
- **Session names**: once a session's first reply is in, a model gives it a short name. It reads the first message and the start of the reply, never a tool's output. By default (automatic) that is Letronna's quick model when the chat is on the Letronna gateway, otherwise your side model or the chat's own, on the chat's own gateway: a chat on your own key, a company's gateway or a model on this computer is never sent elsewhere to be named. A name from a quick model is checked by Jev where tool search may ask it (Settings › Search), which reads the same words and the names; Letronna's own Jev checks only a chat on the Letronna gateway. Settings › Model or `/model names` picks another model, or turns naming off, which keeps the first line of what you asked.
- **Cloud tasks** carry only what the task itself contains (see [Cloud](cloud.md)).
- **Search, browsing and media tools** contact their services only when the agent uses them, and they ask first.
- **Local data**: diagnostic logs and crash files stay local.
