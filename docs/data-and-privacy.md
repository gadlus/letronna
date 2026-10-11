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
| `media/` | Media made with no project attached, and browser screenshots |
| `browser/` | The shared browser profile. Delete it to sign out of everything. |

## Keys

On macOS and Windows, keys and tokens are moved to the Keychain or Credential Manager, and `vault.json` lists which ones. Setting `"keys_in": "files"` in `settings.json` keeps them in plain files instead. On Linux they stay in plain files.

Keys never reach the model. File tools cannot read the data folder, and the browser refuses addresses that carry a key. `letronna doctor` reports where the keys are stored.

## What leaves your machine

- **Chat text** goes only to the gateway of the selected model. With the Letronna gateway, that is `api.letronna.ai`, which records each request's model and cost for your usage page.
- **Cloud tasks** carry only what the task itself contains (see [Cloud](cloud.md)).
- **Search, browsing and media tools** contact their services only when the agent uses them, and they ask first.
- **Local data**: diagnostic logs and crash files stay local.
