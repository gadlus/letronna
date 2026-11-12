# Configuration

Most settings are in the window's Settings and in `/` commands. Both are stored in `settings.json` in the [data folder](data-and-privacy.md). The other files there are described in [Extending](extending.md).

## Environment variables

### Install

| Variable | Effect |
|---|---|
| `LETRONNA_DOWNLOAD` | Install from a different archive (a URL or a local file) |
| `LETRONNA_NO_SETUP=1` | Unpack without asking the setup questions |

### Data and gateways

| Variable | Effect |
|---|---|
| `AGENT_DATA_DIR` | Use a different data folder |
| `LETRONNA_API` | Point the Letronna gateway at another API, such as a local one |
| `LETRONNA_ALLOW_HTTP=1` | Allow plain-HTTP gateways on the local network, not just on localhost. The key travels unencrypted. |
| `LETRONNA_READ_TIMEOUT` | Seconds of silence before a reply is given up on (30–3600; default 300) |
| `LETRONNA_CONTEXT_WINDOW`, `LETRONNA_MAX_OUTPUT` | Override the model's context window and output limit, in tokens. When nothing gives a model's window, a server that takes a key is assumed to hold 64,000 tokens, and one that takes none, such as Ollama, a small window. Set it for a keyed proxy in front of a small local model, which may cut long prompts short without saying so. |
| `LETRONNA_REASONING_REPLAY` | `full` (the default) sends DeepSeek and Kimi the reasoning behind every tool call of the turn in progress; `chunked` sends an empty one with the turn's older calls once their reasoning comes to 8,000 tokens, and then a chunk at a time, so the requests in between keep the provider's prompt cache. The `reasoning_replay` setting says the same for every run. |

### Browser

| Variable | Effect |
|---|---|
| `LETRONNA_BROWSER` | Path of the browser to drive |
| `LETRONNA_BROWSER_HEADLESS=0` | Give the browser a visible window |
| `LETRONNA_BROWSER_URL` | Use a remote browser: `ws(s)://…` for a cloud service, or `http://host:9222` for a Chrome started with `--remote-debugging-port`. Prefer an SSH tunnel, because anyone who can reach that port can drive the browser. |

### Misc

| Variable | Effect |
|---|---|
| `LETRONNA_DISCORD_APP_ID` | Discord application for rich presence |

## Headless runs

`letronna -p` can run with nothing set up, for example inside a container:

| Variable | Effect |
|---|---|
| `AGENT_BASE_URL`, `AGENT_API_KEY`, `AGENT_MODEL` | A Custom gateway for this run only (`-p`, or the terminal with `LETRONNA_DISPOSABLE=1`). Nothing is saved. |
| `LETRONNA_COAUTHOR=0` | Name no co-author on the run's commits, whatever Settings › Privacy says. Letronna Cloud sets it, as it credits its sessions' commits itself. |
| `LETRONNA_DEADLINE` | Seconds the run may take. Set it a little under the harness's kill time so the run ends with its result line, whose status is then `deadline`. |
| `LETRONNA_DISPOSABLE=1` | Declares the machine throwaway, as in a benchmark container. Unattended Bypass runs may then use commands that are hard to undo, such as `rm -r` or `git reset --hard`. Never set it on a machine whose files matter. |
| `LETRONNA_DISPOSABLE_FILES=1` | Beside `LETRONNA_DISPOSABLE=1`, declares the machine's files throwaway too, as a benchmark's or a CI job's task folder is. Once a run has read a web page, a document or other words someone else wrote, Letronna holds back what deletes or writes over a file the run did not make, or writes outside the project; with this set, an unattended Bypass run does those. Anything that sends data off the machine is still held back. Letronna Cloud does not set it. |
| `LETRONNA_DUMP_STREAMS` | A folder to keep each request's body and every line of its reply's stream in, one `.sse` file a request, as they were sent and as they came. For settling whether a tool call that arrived with no arguments, or with another's, was sent so by the provider. The files hold all the model was sent, your files and words among it, but no header and so no key; a reply that comes whole, not streamed, is not kept. |
| `LETRONNA_HELPERS_SHARE_TOOLS=off` | Sends a helper of a run nobody watches every tool, as before, where it is now sent the tools its session's own run is, so the two share the provider's cached prefix. For comparing the two; the `helpers_share_tools` setting says the same for every run. |
| `LETRONNA_PARALLEL_READ_COMMANDS=on` | In a run nobody watches, runs at once the commands of one reply that only read (`ls`, `git log`, `cat`), as its file reads run. Off until paired runs show it sound; the `parallel_read_commands` setting says the same for every run. |
| `LETRONNA_PROGRESS_NOTE_ROUNDS` | Rounds a run nobody watches may go without changing the project before it is asked to say, in one line, what it has established and its next step (default 12; again at twice and three times that; 0 never asks). |
| `LETRONNA_SECOND_ATTEMPT=on` | In a run nobody watches whose work still fails a tester's tests after their last run, has a second helper make the change again from scratch in the project as it was, and puts its work in the first's place where it passes those tests and the first does not. Off until paired runs show it worth the quarter of an hour it takes; the `second_attempt` setting says the same for every run. |

Even without `LETRONNA_DISPOSABLE`, an unattended Bypass run on macOS and Linux may delete folders strictly inside its project. It still refuses:

- the project itself and `.git`
- anything outside the project, or reached through a link that leads out of it
- any path it cannot fully resolve
