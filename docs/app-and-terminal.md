# App and terminal

Both front ends sit on the same core and share the account, sessions and settings. A session started in one can be resumed in the other.

## The window

- **Sessions sidebar**: search, pinning and date groups. Each session is its own file, so several windows can run at once.
- **Model menu**: lists the models of every configured gateway. Effort, thinking and web search can be toggled where the model supports them.
- **Side panels**: Browser, Files, Changes and Jobs.
- **Command palette**: `Ctrl+K`. The whole window is reachable from the keyboard.
- **Going back**: right-click a message and pick Rewind to here, or press `Esc` twice in an empty composer and then `W` on the message. Choose whether the code and the conversation go back, the conversation alone, or the code alone.
- **Tray**: on Windows and Linux, closing the window keeps Letronna in the tray.

## Command line

| Command | Does |
|---|---|
| `letronna` | Interactive session |
| `letronna -p "…"` | One-shot run; prints the answer and exits |
| `letronna --continue` | Resume the most recent session |
| `letronna --resume [id]` | Pick a recent session, or open one by id |
| `letronna gui` | Open the window |
| `letronna setup` | Setup questions |
| `letronna serve` | Answer chat apps with no window open |
| `letronna cloud "…"` | Send a task to Letronna Cloud |
| `letronna mcp …` | Add, list or serve MCP servers |
| `letronna doctor` | Check the installation |
| `letronna --uninstall` | Remove the install |

Flags:

- `--plan`: read-only
- `--yes`: run tools without asking
- `--model`, `--gateway`: route the run to a particular model or gateway

For scripted runs, `-p` also takes a deadline and a JSON Schema for the answer.

In the terminal, Esc or Ctrl+C stops a reply and keeps Letronna open; Ctrl+C twice on an empty box leaves, as does Ctrl+D. A line cleared with Esc comes back with ↑.

In the terminal, the box's top edge names the open session and shows where its folder stands in git, and the window's title leads with the session's name, saying "waiting for you" while a card or a question waits for an answer.

## Modes

| Mode | Behaviour |
|---|---|
| **Ask** (default) | Every change asks first |
| **Accept edits** | Edits go through; commands still ask |
| **Auto** | A judge on your own model decides whatever would otherwise ask. On macOS and Linux, commands can run confined by the OS. |
| **Plan** | Read and search only |
| **Bypass** | Nothing asks. Only use it where that is safe. |

In the terminal, a card that asks about a tool while you are typing keeps your typing: letters go on into the box behind it, and an Enter pressed while you are still typing sends your line instead of answering. A line you sent while it worked never answers a question asked after it.

In the terminal, Shift+Tab (or Alt+M) steps through Ask, Accept edits, Plan and Auto, and Bypass once you have chosen it; the footer says what each lets run as you land on it. With no flag, the terminal starts in the mode the window's settings start sessions in.

## Slash commands

Type `/` to list them all. The common ones:

| Command | Does |
|---|---|
| `/new`, `/clear`, `/resume` | Start a session, or reopen one; Ctrl+L clears the screen |
| `/status`, `/export` | Where things stand on one screen; the conversation as Markdown |
| `/model`, `/effort`, `/thinking` | Model and reasoning controls |
| `/connect`, `/search` | Providers, their plan and usage, and web search |
| `/plan` | Switch to read-only |
| `/init` | Have the model write the project's `AGENTS.md` |
| `/skills`, `/mcp` | Skills and MCP servers |
| `/jobs` | Background commands |
| `/usage` | Tokens and cost per session, day and model |
| `/copy` | Copy the last reply as written; `/copy 2` the one before it |
| `/context` | What fills the context, part by part; the box's bottom edge says how full it is |
| `/btw` | Ask something aside without derailing the turn |
| `/rewind`, `/undo` | Go back to any message you sent; see below |
| `/loop`, `/heartbeat` | Keep a session working toward a goal |
| `/team`, `/login`, `/logout`, `/whoami` | Account |

`@path` attaches a file, a line range or a folder. Your own prompts and skills also run as slash commands.

In the terminal, a line starting with `!` runs as a command in the session's folder, and the model sees what it printed with your next prompt.

In the terminal, `?` on an empty line shows the keys, and `/help <command>` says more about one command.

## Going back

`/rewind` (or `/undo`, or `Esc` twice on an empty box) lists the messages you sent, newest first; type to narrow them. Pick one, then:

| Choice | Does |
|---|---|
| Restore code and conversation | The chat goes back to before the message, its text returns to the box, and the files go back to how they were then |
| Restore conversation | The chat goes back; the files stay as they are now |
| Restore code | The files go back; the chat stays, and the model is told which files went back |
| Summarize up to here | What came before the message is summarized, as `/compact` does |
| Branch from here | A copy of the session carries on from before the message; the original keeps everything |

What a command changed in the project goes back as well as what the file tools wrote. A file someone changed since, a link, or anything outside the project is left alone and named.
