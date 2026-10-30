---
name: letronna
description: "Letronna itself: its commands, settings, chat apps, MCP servers, profiles and doctor. Load before answering how to use or set up Letronna."
---

# Letronna

You run inside Letronna, an agent with a window (`letronna gui`) and a terminal (`letronna`) that share one data folder. When the user asks how to use, set up or fix it, answer from this page and its three files, and give the command to type or the place in the window to go.

- `commands.md`: every `letronna` verb and flag: `-p` and its options, resuming, modes, models, worktrees, `cloud`, `wallet`, `mcp`, `acp`, `update`, `--uninstall`.
- `chat-apps.md`: Telegram, Discord and Slack, pairing, and `letronna serve`.
- `settings-and-data.md`: the data folder and its files, `settings.json`, environment variables, profiles, a project's own files.

## Command line

- `letronna` opens a conversation in the terminal. `--memory when-asked|ask|auto` sets when that session saves to memory, `--memory-scope auto|project|chat` where it keeps memory and what of it loads, and `--private` starts a private chat whose notes stay its own.
- `letronna cloud "task"` runs it on Letronna's servers (Pro, from credit); `--repo <url>`, `--tier light|normal|heavy`, `--thread <id>`; `letronna cloud list|show|stop|relay|apps|logs|down|up`.
- `letronna wallet` (/wallet): the crypto wallet, see `--help`; sends and keys need Windows Hello or Touch ID.
- `letronna local` (/local): on-device models, adding from Hugging Face.
- `letronna -p "question"` (`--print`, or just `letronna "question"`) asks once, prints the answer and exits. With -p: `--output text|json|stream-json`, `--max-rounds <n>` (tool rounds a message gets, 100 unless set), `--tools a,b` (only those tools), `--budget <tokens>` (stops once that many are spent), `--input-format stream-json` (messages, interrupts and answers as JSON lines on stdin), `--permission-prompt-tool stdio` (asks stdin about each call that needs a yes), `--json-schema <file>` (answers as JSON that the schema checks; a new session only). `--dump-config` prints the settings and limits a -p run would get, as JSON. Its environment variables are in `settings-and-data.md`.
- `letronna --continue` (`-c`) picks up this folder's latest session, `letronna --resume` (`-r`) offers the recent ones, `--resume <id>` the one `/where` names. `--fork-session` carries on in a copy, `--name <name>` names it. `--worktree` starts a new session in a git worktree of its own, on a branch made from HEAD, leaving the checkout alone; not with -c or -r.
- `letronna -p "question"` (`--print`, or just `letronna "question"`) asks once, prints the answer and exits. With -p: `--output text|json|stream-json`, `--max-rounds <n>` (tool rounds per message, 100), `--tools a,b` (only those tools), `--budget <tokens>` (a token cap), `--input-format stream-json` (JSON lines on stdin), `--permission-prompt-tool stdio` (asks stdin about each call that needs a yes), `--json-schema <file>` (answers as JSON the schema checks; a new session only). `--dump-config` prints the settings and limits a -p run would get, as JSON. Its environment variables are in `settings-and-data.md`.
- `letronna --continue` (`-c`) picks up this folder's latest session, `letronna --resume` (`-r`) offers the recent ones, `letronna --resume <id>` opens the one whose id `/where` shows. `--worktree` starts a new session in a git worktree of its own, branched from HEAD; not with -c or -r.
- `--mode <mode>`: default (asks before writing or running), accept-edits (project edits run, commands ask), plan (reads and searches only), auto (edits run, the user's model judges the rest), dont-ask (runs only what rules allow), bypass (runs without asking, bar risky calls and the user's rules). `--yes` is bypass, `--plan` is plan.
- `--system-prompt <text>`/`--system-prompt-file <file>` replace Letronna's own instructions for the run; `--append-system-prompt`/`--append-system-prompt-file` add to them.
- `--model <id>` with `--gateway <name>` picks this session's model, and `--effort <level>` (or auto) how hard it thinks; the defaults stay as they are.
- `letronna serve` answers the chat apps and runs the schedules with no window.
- `letronna setup` asks for a name, sign-in, provider, model and what to install; run it again to change one.
- `letronna update` installs the newest version.
- `letronna doctor` checks the setup, gateway, sign-in, keys, logs and last crash. It exits 1 on any problem and never prints a key; `--offline` leaves the network alone. Suggest it first when something is wrong.
- `letronna doctor` checks the setup, keys, logs and last crash, exits 1 on any problem and never prints a key; `--offline` leaves the network alone. Suggest it first when something is wrong.
- `letronna mcp` manages MCP servers; `letronna mcp serve` offers Letronna's browser, media and search tools to another agent over stdio (`--yes` runs them without asking, `--allow-turn` adds a whole agent turn).
- `letronna acp` serves an editor over the Agent Client Protocol.
- `letronna --uninstall` takes it off again. `--help` (`-h`), `--version` (`-V`).
`letronna` opens a session in the terminal and `letronna gui` the window; `letronna -p "question"` asks once and prints the answer; `letronna setup` changes the first-run answers; `letronna doctor` checks everything, so suggest it first when something is wrong. Every other verb and flag is in `commands.md`.

## MCP servers

`letronna mcp add [options] <name> <command-or-url> [args...]` takes Claude Code's flags, so a README's `claude mcp add` line works with `letronna` in its place:

- `letronna mcp add files -- npx -y @modelcontextprotocol/server-filesystem ~/notes`
- `letronna mcp add --transport http sentry https://mcp.sentry.dev/mcp`
- `-e KEY=value` sets the command's environment, `-H "K: v"` a URL's header.

Then `letronna mcp list`, `get <name>`, `remove <name>`, and `login <name>` or `logout <name>` for one reached by URL. They are kept in `mcp.json` in the data folder; in a session `/mcp` shows them, and `/mcp reload` reads the file again.

## Slash commands in the terminal

- /help lists them all, /terminal-setup (shift+enter), /new or /clear starts a fresh session (ctrl+l clears the screen), /exit leaves.
- /help lists them all, /new or /clear starts afresh (ctrl+l clears the screen), /theme (colours), /exit leaves.
- /help lists them all, /new or /clear starts a fresh session (ctrl+l clears the screen), /vim (vim keys), /exit leaves.
- /help lists them all, /new or /clear starts a fresh session (ctrl+l clears the screen), /keybindings (move keys), /exit leaves.
- Model: /model (also image, speech or video models, and `/model side` for a cheaper one for summaries), /fallback (models to carry on with when one is overloaded or out of credit), /connect, /effort, /thinking, /style (reply styles).
- Asking first: /mode, /plan, /decisions (what auto mode's judge allowed or blocked), /verify (build or test after edits), /coauthor (Letronna as co-author of commits).
- Model: /model (also image, speech or video models, and `/model side` for a cheaper one for summaries), /fallback (models to carry on with when one is overloaded or out of credit), /connect, /limits (what is left of each plan), /effort, /thinking, /style (reply styles).
- Asking first: /mode, /plan, /decisions (what auto mode's judge allowed or blocked), /verify (build or test after edits).
- Asking first: /mode, /plan, /decisions (auto mode's judge), /permissions (kept allow rules), /verify (test after edits).
- /help lists them all, /new or /clear starts a fresh session (ctrl+l clears the screen), /exit leaves.
- Model: /model (also image, speech or video models, and `/model side` for a cheaper one for summaries), /fallback (models to carry on with when one fails), /connect, /effort, /thinking, /style (reply styles).
- Asking first: /mode, /plan, /decisions (what auto mode's judge did), /verify (build or test after edits), /format (its formatter after edits).
- Account: /login, /logout, /whoami, /team (bill hosted models to a team).
- Web and voice: /search (lets the model search the web; /search services sets up one of the user's own), /voice (read replies aloud), /notify (say when a reply ends or needs the user).
- Sessions: /resume, /branch (a copy), /rename, /rewind, /undo (any message), /redo, /retry, /compact (sum up old turns), /context, /diff, /where, /status, /export, /import (Claude Code, Codex), /btw (an aside, not kept), /copy, /image <path>, /preview <path>, /usage, /insights.
- Web and voice: /search (web search; /search services sets up the user's own), /voice (read replies aloud), /notify (say when a reply ends or needs the user).
- Sessions: /resume, /branch (a copy), /rename, /rewind, /undo (any message), /retry, /compact (summarize old turns), /context, /diff, /where, /status, /export, /import (Claude Code, Codex), /btw (an aside, not kept), /copy, /image <path>, /preview <path>, /usage, /insights.
- Sessions: /resume, /branch (a copy), /rename, /rewind, /undo (any message), /retry, /compact (summarize old turns), /context, /diff, /where, /window, /status, /export, /import (Claude Code, Codex), /btw (an aside), /copy, /image <path>, /preview <path>, /usage, /insights.
- Sessions: /resume, /branch (a copy), /rename, /rewind, /undo (any message), /retry, /compact (summarize old turns), /context, /diff, /where, /status, /export, /import (Claude Code, Codex), /btw (an aside), /recap, /copy, /image <path>, /preview <path>, /usage, /insights.
- Long work: /goal (keep working until a goal is met, `--check <command>`), /loop (run a prompt every so often; /loop stop), /heartbeat (look in on this session every so many minutes), /jobs (background commands), /agents (helpers), /workflows, /crew (workflows of helpers for big tasks).
- Cloud: ask in chat to run or host something (a bot 24/7), check it or take it down.
- Asking first: /mode, /plan, /decisions (what auto mode's judge decided), /verify (build or test after edits).
- Account: /login, /logout, /whoami, /team (bill hosted models to a team).
- Web and voice: /search (lets the model search the web; /search services sets up one of the user's own), /voice (read replies aloud), /notify (say when a reply ends or needs the user).
- Sessions: /resume, /branch (a copy), /rename, /rewind, /undo (any message), /retry, /compact (summarize old turns), /context, /diff, /where, /status, /statusline, /export, /import (Claude Code, Codex), /btw (an aside, not kept), /copy, /image <path>, /preview <path>, /usage, /insights.
- Long work: /goal (keep working until a goal is met, `--check <command>`), /loop (run a prompt every so often; /loop stop), /heartbeat (look in on this session every few minutes), /jobs (background commands), /agents (helpers), /workflows, /crew (workflows of helpers for big tasks).
- Cloud: just ask in chat to run or host something (a bot 24/7), check it or take it down.
- Keeping and adding: /remember (keep something now, or change when this chat saves), /memory (`/memory edit, log or restore user|memory|project|soul`), /memory-review, /retention (delete old sessions), /init (write AGENTS.md), /skills, /mcp, /plugins, /tool-search.
- Cloud: just ask in chat to run or host something (a bot 24/7), check it or take it down.
- Keeping and adding: /remember (keep something now, or change when this chat saves), /memory (`/memory edit, log or restore user|memory|project|soul`), /memory-review, /retention (delete old sessions), /init (write AGENTS.md), /skills, /mcp, /plugins, /tools, /tool-search.

## The window

The sidebar has New session, Capabilities (tabs Skills, Tools and MCP), Activity, Plugins, Messaging, Artifacts, Scheduled jobs, Wallet and Workflows. The account menu has Account, Usage, Settings, Profile, Set up Letronna, Guide, and signing in or out. Settings has Account, Usage, Gateways, Search, Model, Appearance, Thinking, Memory, Privacy, Wallet, Shortcuts, Plugins and About.

## Skills, profiles and schedules

- Skills: `/skills` switches them on or off, `/skills <name>` puts one in the message box, and `/skills add <owner/repo, link or path>` installs from GitHub, a link or a folder (`anthropics/skills` has Word, Excel, PowerPoint and PDF ones). This one comes with Letronna and can be switched off; a skill of the user's called `letronna` takes its place, and Copy to edit on the window's Skills page makes one from this.
- Profiles: separate data folders for separate identities, switched or made (empty, or cloned) under Profile in the window's account menu. See `settings-and-data.md`.
- Schedules: a prompt that runs on a timer, made with New job on the Scheduled jobs page, or by asking in a chat, which adds one once the user says yes. They run while the window, the terminal or `letronna serve` is open.

If this skill is silent, say so and web_fetch https://letronna.ai/docs; never invent a menu, setting or command.
