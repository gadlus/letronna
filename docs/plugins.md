# Plugins that run code

A plugin can run code written in **JavaScript** or **Python**, with nothing to build and no Rust toolchain. It can add tools the model calls, look at tool calls before and after they run (and block one), send the model back when its work is not done, add context to what the user sends, measure what each turn uses, add slash commands, keep settings, and show a status, a panel and notices in the window and the terminal.

- **JavaScript** runs inside Letronna on the QuickJS engine it already ships. Nothing is installed.
- **Python** runs as one process per plugin, on the Python already on the computer (3.9 or later). It needs nothing but Python's own library.

Working examples are in [`examples/plugins`](../examples/plugins):

| Example | Language | Shows |
|---|---|---|
| `guard` | JavaScript | Blocks force pushes and other commands that are hard to take back (`tool.before`), each behind a setting. |
| `session-meter` | JavaScript | Tokens, cost and time of each turn in the status bar (`model.reply`, `turn.end`), and a word to the model when a call keeps failing the same way (`tool.failed`). |
| `git-status` | Python | The branch in the status bar and what changed in a panel; can tell the model where the project stands with each message (`prompt.submit`). |
| `test-gate` | Python | Runs your tests when the model says it is done and sends it back until they pass (`turn.stop`). |

## Quick start

Put a folder in the plugins folder of the [data folder](data-and-privacy.md), or install one from git with `/plugins add`:

```
<data>/plugins/word-count/
  plugin.json
  plugin.js
```

```json
{
  "name": "word-count",
  "version": "1.0.0",
  "description": "Counts words for the model.",
  "main": "plugin.js",
  "contributes": {
    "tools": [
      {
        "name": "count",
        "description": "How many words a text has.",
        "parameters": {
          "type": "object",
          "properties": { "text": { "type": "string" } },
          "required": ["text"]
        },
        "readOnly": true
      }
    ]
  }
}
```

```js
letronna.tool("count", ({ text }) => String(text.split(/\s+/).filter(Boolean).length));
```

Then allow it, with `/plugins allow word-count` in the terminal or **Allow** on the Plugins page. The model now has a tool called `plugin__word-count__count`.

The same in Python, with `"main": "plugin.py"`:

```python
import letronna

@letronna.tool("count")
def count(args, ctx):
    return str(len(args["text"].split()))
```

## The manifest

These keys sit beside the ones every plugin has (`name`, `version`, `description`, `skills`, `commands`, `styles`, `mcpServers`, `providers`; see [Extending](extending.md#plugins)).

| Key | Meaning |
|---|---|
| `main` | The code to run: a `.js` file, or a `.py` file, inside the plugin. |
| `start` | `"on-use"` (the default) starts it the first time something it brings is used. `"launch"` starts it with Letronna, for a status that shows from the start. |
| `permissions` | `"network"`: JavaScript may use `letronna.fetch`. `"conversation"`: the events it hears carry what was said (prompts, answers, tool arguments and output), not only that something happened. |
| `contributes.tools` | Tools the model can call: `name`, `description`, `parameters` (a JSON schema), `readOnly`, `openWorld`, `destructive`, `timeout` (seconds, default 120). |
| `contributes.actions` | Slash commands that run code: `name`, `description`, `hint` (what to type after it). |
| `contributes.events` | The events it hears: `turn.start`, `turn.stop`, `turn.end`, `prompt.submit`, `tool.before`, `tool.after`, `tool.failed`, `model.reply`, `approval.wait`. |
| `contributes.status` | `true` when it shows a status. |
| `contributes.panel` | `{"title": "..."}` when it draws a panel in the window. |
| `contributes.settings` | Its settings: `key`, `label`, `type` (`switch`, `text` or `choice`), `choices`, `default`. Letronna keeps them and shows them, in the window on the Plugins page and in the terminal under `/plugins settings <name>`. |

Everything a plugin brings is declared here rather than found by running it, so lists, menus and the tools array are there before anything starts, and a plugin that is never used never runs.

## The API

The JavaScript API is the global `letronna`; the Python one is the `letronna` module. The names are the same, in each language's spelling.

| JavaScript | Python | Does |
|---|---|---|
| `letronna.tool(name, (args, ctx) => …)` | `@letronna.tool(name)` | Handles a declared tool. Returns text, or anything JSON, which the model gets as JSON. A throw is the tool's error. |
| `letronna.action(name, (text, ctx) => …)` | `@letronna.action(name)` | Handles `/name text`. Text it returns is shown to the user; `{compose: "…"}` is put in the box for the user to read and send. |
| `letronna.on(event, (data) => …)` | `@letronna.on(event)` | Handles an event. What a handler returns matters for some events (below): a string, `{note: "…"}`, or for `tool.before` `{block: "why"}`. |
| `letronna.ui.panel(tree)` | `letronna.ui.panel(tree)` | Draws the plugin's panel. |
| `letronna.ui.onAction(id, (value) => …)` | `@letronna.ui.on_action(id)` | Handles a button, switch or input of the panel. |
| `letronna.status(text)` | `letronna.status(text)` | Sets the status; empty clears it. |
| `letronna.toast(text, kind)` | `letronna.toast(text, kind)` | Puts up a notice: `info`, `warning` or `error`. |
| `letronna.log(...)`, `console.log(...)` | `letronna.log(...)`, `print(...)` | Writes to the plugin's log. |
| `letronna.settings.get(key)` | `letronna.settings.get(key)` | Reads a setting. `settings.change` is heard when the user changes one. |
| `letronna.storage.get/set/delete/all` | `letronna.storage.get/set/delete/all` | A small store of its own, kept across restarts (1 MB). |
| `letronna.fetch(url, options)` | Python's own `urllib` | HTTPS requests, with the `network` permission. |
| `setTimeout`, `setInterval`, `require("./file")` | Python's own | Timers, and the plugin's other files. |

`ctx` holds `conversation` (the session's id) and `workspace` (its project folder). In Python it also has `cancelled`, a `threading.Event` set when Letronna stops waiting. A Python handler may be `async def`.

### Events

| Event | When | Carries | What a handler may return |
|---|---|---|---|
| `turn.start` | A turn begins | `conversation`, `first` (the first turn in that session since Letronna started) | |
| `prompt.submit` | The user sent a message, before the model reads it | `conversation`, `workspace`; with `conversation`: `text` | Text for the model to read with the message. It goes in just before the message, fenced, as a notice the user sees too. |
| `tool.before` | A call the gate allowed is about to run | `conversation`, `workspace`, `tool`; with `conversation`: `args` | `{block: "why"}` stops the call; text is a note for the model. |
| `tool.after` | A call ran | as `tool.before`, and `ms`; with `conversation`: `output` | Text: a note for the model. |
| `tool.failed` | A call ran and failed | as `tool.before`, and `ms`; with `conversation`: `error` | Text: a note for the model, such as that it failed the same way before. |
| `model.reply` | The model answered one request | `conversation`, `model`, `input_tokens`, `output_tokens`, `cached_tokens`, `reasoning_tokens`, `per_second`, `cost` (when the gateway says) | |
| `turn.stop` | The model has finished and the turn is about to end | `conversation`, `workspace`, `again` (it was sent back already this turn); with `conversation`: `answer` | Text, or `{block}`: what is left to do. The model is sent back to do it, at most 3 times a turn, counting the user's own Stop hooks. |
| `turn.end` | A turn is done | `conversation`, `workspace`, `stopped`, and what it used: `ms`, `requests`, `input_tokens`, `output_tokens`, `cached_tokens`, `cost`, `tool_calls`, `tool_failures`; with `conversation`: `answer` | |
| `approval.wait` | A card waits for the user | `conversation`, `tool`, `kind` (`permission` or `question`) | |
| `panel.open` | The plugin's panel was opened | | |
| `settings.change` | The user changed a setting | `key`, `value` | |

`turn.stop` gets up to five minutes, so a plugin can run a test suite; the user's stop ends the wait. The other events whose answer matters get 2 s, as below.

### The panel

A panel is a tree of plain parts that the window draws with its own widgets, in the theme's colours:

```json
{"type": "column", "children": [
  {"type": "text", "text": "25:00", "size": "large"},
  {"type": "row", "children": [
    {"type": "button", "id": "start", "label": "Start", "primary": true},
    {"type": "switch", "id": "sound", "label": "Sound", "value": true}
  ]},
  {"type": "input", "id": "task", "placeholder": "What are you working on?"},
  {"type": "progress", "value": 0.4},
  {"type": "divider"},
  {"type": "link", "label": "Docs", "url": "https://example.com"}
]}
```

`text` also takes `muted`, `mono`, `size` (`small`, `normal`, `large`) and `tone` (`default`, `success`, `warning`, `error`). Every button, switch and input needs an `id`; pressing, flipping or pressing Enter in one calls its `onAction` handler with the new value. A panel holds at most 500 parts, 16 deep.

The panel is a tab beside the chat. It shows once the plugin has drawn it, and can be opened from the plugin's status chip, its row on the Plugins page, or the command palette. The terminal shows statuses and notices, and runs actions and tools, but draws no panels.

## Consent and safety

A plugin's code is someone else's program, so nothing runs until you allow the plugin. What you are asked says what it runs and what it may reach, and the consent covers that:

- A plugin installed from git is asked about again whenever an update changes its code, its permissions or what it brings.
- A plugin you put in the plugins folder yourself is asked about once. Your own edits to it are yours, and `/plugins reload` starts it afresh.

What a plugin can do:

- **JavaScript** runs in its own sandbox: no files, no commands, no addresses on this computer or the local network, and the internet only with `network`, over HTTPS.
- **Python** runs with your account's access to the computer. Nothing can hold it to less, which is what you are told before allowing one.
- **No allowing.** A hook runs after the [approval gate](tools.md), so a plugin can block a call but never allow one the gate stopped.
- **Marked as someone else's.** What a plugin says to the model (a block's reason, a note, a tool's result) is fenced as text from outside, as a web page's is.
- **Approval for tools.** A plugin's tool asks before it runs unless it is `readOnly`, not `destructive` and does not reach past this computer, as a local MCP server's tool. A JavaScript plugin with `network` counts as reaching past it. A Python tool counts as reaching past it unless it says `"openWorld": false`.

## Staying light

None of this costs anything until it is used:

- **Off or unused costs nothing.** A plugin that is off, not allowed or not used yet has no thread and no process. A started JavaScript plugin is one sleeping thread with a heap held to 32 MB. A Python plugin is one sleeping process.
- **Events are opt-in.** An event goes only to the plugins that list it. With none listening, an event costs one atomic read and builds nothing.
- **Nothing waits on a slow plugin.**
  - Plugins hearing `tool.before`, `tool.after`, `tool.failed` or `prompt.submit` get 2 s between them (10 s when one has to start first). A late one is reported and the call or the message goes on.
  - The window never waits on them: it asks off its own thread and starts the turn when they have answered.
  - JavaScript's own work is stopped after 250 ms in an event's handler and 5 s in a tool's, and the plugin carries on.
  - Each plugin's queue holds 64 messages; past that, what it is sent is dropped, and you are told once.
- **Redraws are capped.** The window redraws for plugins at most once a frame; the terminal looks four times a second, and only redraws when something changed.
- **Failures back off.** A plugin that fails waits longer before each start (1 s, doubling to 30 s). After 5 failures in 10 minutes it is left off until `/plugins reload`.

## Writing and debugging

- **Iterating in place:** work in `<data>/plugins/<name>` and use `/plugins reload <name>` after an edit.
- **Logs:** what a plugin logs or prints goes to `<data>/plugin-data/<name>/log.txt`, with its settings and storage beside it.
- **Failures:** a failing handler is reported once in the window or the terminal, and in full in the log.
- **One copy per front end:** the window and the terminal each run their own copy of a plugin, so a plugin that starts with Letronna runs twice when both are open. Settings and storage are shared files.
- **Finding Python:** set `LETRONNA_PYTHON` to the interpreter to use when the one on `PATH` is not the right one.
- **Native modules:** compiled programs inside a plugin are refused unless they sit in a folder the manifest declares, so a Python plugin that needs a compiled module should ask for it to be installed with `pip` instead.
