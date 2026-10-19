# Letronna on Harbor

`harness_letronna` runs Letronna's terminal as a [Harbor](https://github.com/harbor-framework/harbor)
installed agent, so Terminal-Bench, SWE-bench Pro and the other Harbor
datasets can score it. It was written against Harbor 0.23.0 and needs a
Harbor with `AgentCapabilities` (August 2026 on), Python 3.12 or later, and
Docker or another Harbor sandbox.

```sh
export OPENAI_API_KEY=...
PYTHONPATH=scripts/harbor harbor run -d terminal-bench@2.0 \
  -a harness_letronna.agent:Letronna \
  -m openai/gpt-5.4 \
  --ak binary=/absolute/path/to/letronna-linux-x64 \
  --ak sha256=<the binary's SHA-256>
```

The model is `provider/model`. Letronna runs on its Custom gateway at the
endpoint Harbor gives that provider, with the key Harbor finds for it:
`OPENAI_API_KEY` for `openai/`, `OPENROUTER_API_KEY` for `openrouter/`, and
so on. That endpoint has to take OpenAI's chat completions, as Harbor's
does for `openai/`, `openrouter/`, `groq/`, `deepseek/`, `mistral/`, `xai/`
and `together/`. For `google/` (or `gemini/`) and `minimax/` it does not, so
give the provider's OpenAI-compatible URL in its base URL variable, such as
`--ae GOOGLE_BASE_URL=https://generativelanguage.googleapis.com/v1beta/openai`,
or `MINIMAX_BASE_URL` for `minimax/`. A provider Harbor has no endpoint for
needs its `*_BASE_URL` the same way. For any other server, name the model
`openai/<the id it serves>` and give `--ae OPENAI_BASE_URL=https://.../v1`.
Letronna takes HTTPS, or HTTP on localhost only, and inside the task
container localhost is the container.

| `--ak` | |
| --- | --- |
| `binary` | A Linux `letronna` for the task image's architecture. Required. |
| `sha256` | What the binary's SHA-256 must be; for a release, `platforms["linux-x64"].terminal.sha256` in its `latest.json`. Each trial records the hash in its metadata either way. |
| `max_rounds` | Tool rounds for the task, 250 by default as SWE-bench Pro's scaffold allows; `-p` alone stops at 25. |
| `tools` | Only these tools, such as `read_file,list_dir,search,write_file,edit_file,apply_patch,run_command` for SWE tasks. |
| `deadline_margin` | Seconds Letronna's deadline is set short of the trial's agent timeout, 120 by default; never more than half of it. |

The release's `letronna-linux-x64` is built on Ubuntu 22.04 against glibc
2.35, so it does not start on images with an older glibc. Install then fails
at `letronna --version` rather than the trial scoring zero unnoticed.

## In the container

`install` makes sure curl is there: Terminal-Bench verifiers fetch their test
runner with it, so an image without curl scores zero whatever the agent did.
On a Debian release that has moved to archive.debian.org, apt is pointed
there first. The binary and `drive.sh` go into `/installed-agent/letronna`.

`run` writes Letronna's settings and the key into
`/installed-agent/letronna/data`, its `AGENT_DATA_DIR`, and has `drive.sh`
run one turn of

```sh
letronna -p --input-format stream-json --output stream-json \
  --yes --max-rounds 250 --model <model>
```

`LETRONNA_DEADLINE` is the trial's agent timeout, as Harbor works it out
from the task's `task.toml` and the job's overrides and multipliers, less
`deadline_margin`. At 80% of it Letronna is told to write its outputs, and
at the deadline it stops with its result line, its session kept, before
Harbor's timeout. A trial whose timeout cannot be read gets no deadline.
`LETRONNA_NO_LOGIN_ENV=1` keeps the task image's PATH and variables rather
than laying a login shell's over them, as a Debian or Ubuntu
`/etc/profile` resets PATH.

It runs as a run nobody watches, as on OpenHuman's rig, so it is given what
Letronna gives such a run: deciding for itself, keeping to the task and its
folder, the checks before it ends, and no `ask_user`. Approvals are not
answered over stdio, since a program that answers them counts as someone
who can be asked, which turns all of that off. With nobody to ask, `-p`
refuses commands that are hard to take back, such as `rm -r`,
`git reset --hard`, `sudo` or `curl ... | sh`, even with `--yes`; in a
container thrown away after the trial that would fail tasks that need them,
so `LETRONNA_DISPOSABLE=1` says the machine is thrown away and they run.
What Letronna never runs, it still refuses. `drive.sh` still allows any
request for approval that does come.

The trial's `agent/` folder then holds:

- `letronna.jsonl`: every line Letronna wrote, and `letronna.stderr`
- `message.json`: the task as it was sent
- `letronna/sessions/` and `letronna/agents/`: the session and helper files,
  copied out after the run; the data folder itself stays behind, as it holds
  the key
- `trajectory.json`: the session as ATIF v1.7, with token totals and cost

## The trajectory

`harness_letronna/atif.py` builds it, and works on any Letronna session:
from this folder, `python3 -m harness_letronna.atif <data>/sessions/<id>.jsonl`.
Each reply is an agent step with its tool calls and their results; a notice
Letronna sends the model mid-run is a system step, not the user's. Helpers
are embedded as subagent trajectories, but not referenced from the call that
started each one, which their files do not record. The totals count every
model call the run paid for once, the helpers' included: a fork's copy of
its parent's messages is marked as copied context, and its replies are
counted where they were made.

Not passed to Letronna: a task's MCP servers and skills folder. Windows task
images are not supported.

## All of Terminal-Bench 2.0 on one host

`tb2/` runs the 89 tasks on one x86_64 Linux host with Docker, `uv` and
Harbor (`uv tool install harbor`), from the static build in
`scripts/openhuman-bench/README.md`. The model is reached through
`tb2/relay.py` on the host, which holds the key and, like OpenHuman's
metering proxy, can pin the model, the provider and the reasoning effort on
every call and writes one meter line per call. For a gateway other than
OpenRouter, give `--upstream`, `--key-env` and `--model`; the reasoning
effort then goes as OpenAI's `reasoning_effort`, and the caller's
User-Agent is passed on, as some gateways turn away Python's:

```sh
OPENROUTER_API_KEY=... python3 scripts/harbor/tb2/relay.py \
  --meter tb2-work/meter.jsonl --provider DeepSeek --reasoning high &
LETRONNA_BIN=$PWD/target/x86_64-unknown-linux-musl/release/letronna \
  scripts/harbor/tb2/run.sh deepseek-v4.1-flash-1
```

`run.sh` downloads the dataset into `tb2-work/`, runs every task once with
`openai/deepseek/deepseek-v4.1-flash` (`TB2_MODEL`) three at a time
(`TB2_N`), and ends with `tb2/report.py`'s table. All 89 images take 49 GB
compressed, so `tb2/janitor.py` removes each task's image once its trial is
done, and the four of 6-9 GB run one at a time after the rest. Letronna
calls the relay at `http://host.docker.internal:18080/v1`, which
`LETRONNA_ALLOW_HTTP=1` admits as a local name. `TB2_AGENT=oracle` runs
each task's own solution instead, with no model, to show which tasks this
host can pass at all: `report.py RUN --oracle ORACLE` then sets those apart.
A stopped run picks up with `harbor job resume -p tb2-work/jobs/<job>`.
`tb2/triage.py JOB` says, for each trial that lost points, how Letronna's
run ended, the tests the verifier failed and the start of its answer.

A network that re-terminates TLS with its own CA, as a Claude Code cloud
container's does, breaks every verifier, which fetches uv and pytest over
HTTPS. `TB2_CA=/path/to/ca-bundle.crt` mounts that bundle into each task
container and points curl, pip, uv, git, Node and the rest at it. Those
variables are then in the agent's environment too, and Letronna's own
web_fetch still refuses such a certificate, since its roots are built in.
In such a container Docker Hub may also turn anonymous pulls away;
`"registry-mirrors": ["https://mirror.gcr.io"]` in
`/etc/docker/daemon.json` serves every Terminal-Bench 2.0 image.

## Tests

```sh
cd scripts/harbor && python3 -m unittest discover -s tests
```

`test_drive.py` runs `drive.sh` under `sh` against a stand-in for Letronna,
`test_atif.py` converts session folders laid out as Letronna writes them,
and `test_tb2.py` runs the relay against a stand-in provider and checks the
janitor's bookkeeping and the report.
`test_agent.py`, and the converter's check against Harbor's own ATIF models,
run only where `harbor` can be imported.
