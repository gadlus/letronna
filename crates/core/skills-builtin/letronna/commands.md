# The command line

Every `letronna` verb and flag. `letronna --help` prints the same list.

- `letronna` opens a conversation in the terminal. `--memory when-asked|ask|auto` sets when that session saves to memory, `--memory-scope auto|project|chat` where it keeps memory and what of it loads, and `--private` starts a private chat whose notes stay its own.
- `letronna cloud "task"` runs it on Letronna's servers (Pro, from credit); `--repo <url>`, `--tier light|normal|heavy`, `--thread <id>`; `letronna cloud list|show|stop|relay|apps|logs|down|up`.
- `letronna wallet` (/wallet): the crypto wallet, see `--help`; sends and keys need Windows Hello or Touch ID.
- `letronna -p "question"` (`--print`, or just `letronna "question"`) asks once, prints the answer and exits. With -p: `--output text|json|stream-json`, `--max-rounds <n>` (tool rounds a message gets, 100 unless set), `--tools a,b` (only those tools), `--budget <tokens>` (stops once that many are spent), `--input-format stream-json` (messages, interrupts and answers as JSON lines on stdin), `--permission-prompt-tool stdio` (asks stdin about each call that needs a yes), `--json-schema <file>` (answers as JSON that the schema checks; a new session only). `--dump-config` prints the settings and limits a -p run would get, as JSON. Its environment variables are in `settings-and-data.md`.
- `letronna --continue` (`-c`) picks up this folder's latest session, `letronna --resume` (`-r`) offers the recent ones, `letronna --resume <id>` opens the one whose id `/where` shows. `--worktree` starts a new session in a git worktree of its own, on a branch made from HEAD, leaving the checkout alone; not with -c or -r.
- `--mode <mode>`: default (asks before writing or running), accept-edits (project edits run, commands ask), plan (reads and searches only), auto (edits run, the user's model judges the rest), dont-ask (runs only what rules allow), bypass (runs without asking, bar risky calls and the user's rules). `--yes` is bypass, `--plan` is plan.
- `--model <id>` with `--gateway <name>` picks this session's model, and `--effort <level>` (or auto) how hard it thinks; the defaults stay as they are.
- `letronna gui` opens the window.
- `letronna serve` answers the chat apps and runs the schedules with no window.
- `letronna setup` asks for a name, sign-in, provider, model and what to install; run it again to change any answer.
- `letronna update` installs the newest version.
- `letronna doctor` checks the version, system, data folder, settings, gateway, model, sign-in, keys, logs and last crash. It exits 1 on any problem and never prints a key; `--offline` leaves the network alone. Suggest it first when something is wrong.
- `letronna mcp` manages MCP servers; `letronna mcp serve` offers Letronna's browser, media and search tools to another agent over stdio (`--yes` runs them without asking, `--allow-turn` adds a whole agent turn).
- `letronna acp` serves an editor over the Agent Client Protocol.
- `letronna --uninstall` takes it off again. `--help` (`-h`), `--version` (`-V`).
