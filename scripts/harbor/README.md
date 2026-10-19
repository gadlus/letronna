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
(`TB2_N`), and ends with `tb2/report.py`'s table. Letronna calls the relay
at `http://host.docker.internal:18080/v1`, which `LETRONNA_ALLOW_HTTP=1`
admits as a local name. `TB2_AGENT=oracle` runs each task's own solution
instead, with no model, to show which tasks this host can pass at all:
`report.py RUN --oracle ORACLE` then sets those apart. A stopped run picks
up when `run.sh` is run again with the same job name, or with
`harbor job resume -p tb2-work/jobs/<job>`. Harbor locks a job to its
config and to its overlay's content, so it refuses to resume one after
`TB2_MODEL`, `TB2_N`, `TB2_CA`, `TB2_ALONE`, `TB2_ALONE_START`,
`TB2_RELAY_PORT` or `LETRONNA_BIN` has changed, and `run.sh` then stops
that job, setting nothing aside. A job begun by an older `run.sh`, whose
overlay had no `UV_HTTP_TIMEOUT`, cannot be resumed by this one or by
`harbor job resume`: finish it with the kit it began with, or start a new
job name. `tb2/triage.py JOB` says, for each trial that lost points, how
Letronna's run ended, the tests the verifier failed and the start of its
answer.

All 89 images take 49 GB compressed, so `tb2/janitor.py` removes each
task's image once its trial is done, and again after a rerun pulls it
back. The four of 6-9 GB run one at a time after the rest, with four times
Harbor's 600 s to start (`TB2_ALONE_START`) so that their pull fits, and
`run.sh` warns when Docker has under 20 GB free. When a start times out,
Harbor stops the docker CLI but not the compose plugin under it, which goes
on pulling, so the janitor stops the compose processes still bringing up
the environment of a trial that has ended, and lets a teardown finish. A
Harbor killed outright leaves its trials' networks behind, and once Docker
runs out of address pools no trial starts: `docker network prune` clears
them.

A meter line carries the call's `cost` too when the upstream says what it
charged in a response header whose name ends in `-cost`, read as dollars,
so check that an upstream other than the one shown means dollars by it and
not, say, rate-limit points. Those charges, or a `cost` in the usage where
the upstream reports it there, are what `tb2/spend.py METER ...` adds up;
it counts the calls answered without a charge apart rather than pricing
their tokens, as Harbor's `cost_usd` in the report does, which can fall
well short of what a key was charged. `TB2_METER` names the meters a run
reads (`tb2-work/meter.jsonl`), so a run or a key given its own relay and
meter is counted alone. `TB2_BUDGET=25` stops Harbor once they reach $25,
as a Ctrl-C would, writing the trials it stops as cancelled, and starts no
more; it is checked each minute, so set it a little under what the key
holds. Some upstreams send a charge only with a plain reply and none with
a streamed one, and Letronna streams every reply, so read spend.py's count
of calls without a charge before leaning on the budget. Calls answered
without a charge add nothing to it, so a key can run dry first: once the
upstream answers the last call a run began with 402, `run.sh` starts no
more Harbor runs, and run again after a top-up it sets aside the trials
the 402s failed while their reruns are still to come.
`run.sh` warns when a meter it reads is missing after its first call to
the relay, and ends with spend.py's total.

A trial the infrastructure cost says nothing of the model, so `run.sh` runs
it again, each task at most twice more (`TB2_RERUNS`). After each Harbor
run, `tb2/aside.py` moves to `tb2-work/aside/<job>/` every trial that
failed before the agent started, that a stop cancelled, whose verifier ran
no test as the tests' own install failed on the network, or whose Letronna
run failed with the upstream refusing calls (402, 429 or 5xx in the meters)
in its last 30 s, and Harbor, run again on the job, runs the trials it is
missing. A refusal that names a content filter stands, as does any 400 or
403. The meter does not say which trial made a call, so give each run on
the host its own relay and meter, or one run's refusals can set aside
another's failures. `aside.py JOB --meter METER` lists the trials without
moving them. After a budget stop, raise or unset `TB2_BUDGET` and run
`run.sh` again: Harbor resumes each job, then the cancelled trials are set
aside and run, and they do not count toward `TB2_RERUNS`, as a stop says
nothing of the task. Trials are set aside only between two Harbor runs of
a job Harbor has taken, since the report counts only the trials left in
the job. Each task's `main` container gets `UV_HTTP_TIMEOUT=120`, so a
verifier's uv waits out a stalled download rather than giving up after
30 s; Letronna's commands get it too.

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
janitor's bookkeeping, the spend, the trials set aside and the report.
`test_agent.py`, and the converter's check against Harbor's own ATIF models,
run only where `harbor` can be imported.
