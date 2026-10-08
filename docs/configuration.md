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
| `LETRONNA_CONTEXT_WINDOW`, `LETRONNA_MAX_OUTPUT` | Override the model's context window and output limit, in tokens |

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
| `AGENT_BASE_URL`, `AGENT_API_KEY`, `AGENT_MODEL` | A Custom gateway for this run only. Nothing is saved. |
| `LETRONNA_DEADLINE` | Seconds the run may take. Set it a little under the harness's kill time so the run ends with its result line, whose status is then `deadline`. |
| `LETRONNA_DISPOSABLE=1` | Declares the machine throwaway, as in a benchmark container. Unattended Bypass runs may then use commands that are hard to undo, such as `rm -r` or `git reset --hard`. Never set it on a machine whose files matter. |

Even without `LETRONNA_DISPOSABLE`, an unattended Bypass run on macOS and Linux may delete folders strictly inside its project. It still refuses:

- the project itself and `.git`
- anything outside the project, or reached through a link that leads out of it
- any path it cannot fully resolve
