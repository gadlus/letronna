# OpenHuman's benchmark rig for Letronna

Runs `letronna -p` on [openhuman-benchmarks](https://github.com/tinyhumansai/openhuman-benchmarks),
the rig OpenHuman publishes its cross-harness numbers from. Every harness
there runs headless in an identically limited container (4 CPUs, 8 GB, no
swap) behind one metering proxy, which holds the only OpenRouter key and
pins the model, the reasoning level and the provider on every call. The same
graders score them all: the official SWE-bench evaluator, each DeepSWE
task's own verifier, and Harbor for Terminal-Bench. Its published lineup is
OpenHuman, Claude Code, Codex, OpenCode, OpenClaw, Hermes and DeepSeek's
harness, so a run here puts Letronna beside them on the same model, proxy
and graders, and on the rig's own measures: resolve rate, tokens, cost,
cache-hit %, time to first token, call latency, wall time, cold start to the
first model call, cgroup CPU and RAM, and static prompt size.

| File | What it is |
| --- | --- |
| `PIN` | The openhuman-benchmarks commit the patch is made against |
| `rig.patch` | Makes `letronna` a harness the rig knows: its adapter, bundle, build stamp, chart colour and lineup slot, and Terminal-Bench |
| `letronna.sh` | The adapter, which the patch puts in the rig as `bundles/adapters/letronna.sh` |

The rig has no license file, so none of its code is copied here: only these
files, and a patch applied to a checkout at `PIN`, the way
`scripts/harnessbench/fork.patch` pins Harness-Bench.

## What you need

- An x86_64 Linux host with Docker (and its compose plugin and BuildKit),
  cgroup v2, Node 22 or later, `uv`, `git` and `jq`. The rig is x86_64 only:
  the SWE-bench instance images exist for nothing else. At least 6 CPUs and
  12 GB free, so the task container's 4 and 8 GB plus the proxy fit.
- About 100 GB of disk for the SWE-bench, DeepSWE and Terminal-Bench images.
- An OpenRouter key with credit. Only the proxy sees it.
- For DeepSWE, a Hugging Face token with access to `datacurve/deep-swe`.
- A Linux `letronna`, built below.

Run nothing else heavy on the host meanwhile; the rig runs one harness at a
time so the CPU and latency columns stay comparable, and waits while it sees
another benchmark running.

## The Linux binary

Task images differ in their C library, so use the static musl build, which
starts on any of them. On an x86_64 Linux host, from a Letronna checkout:

```sh
sudo apt-get install -y musl-tools
rustup target add x86_64-unknown-linux-musl
TARGET_CC=musl-gcc cargo build --release --locked -p letronna-cli --target x86_64-unknown-linux-musl
# target/x86_64-unknown-linux-musl/release/letronna
```

That is the `static` job in `.github/workflows/release.yml`, and a release's
`letronna-linux-x64-musl` is that build. The other Linux build, made on
Ubuntu 22.04 against glibc 2.35 as `scripts/harbor/README.md` describes,
runs in the SWE-bench images (Ubuntu 22.04) but not in older ones, such as
the Debian 11 images some Terminal-Bench tasks use.

## Set up

```sh
git clone https://github.com/tinyhumansai/openhuman-benchmarks
cd openhuman-benchmarks
git checkout "$(cat /path/to/letronna/scripts/openhuman-bench/PIN)"
git apply /path/to/letronna/scripts/openhuman-bench/rig.patch
cp .env.example .env    # set OPENROUTER_API_KEY; leave the rest as it is
export BENCH_UID=$(id -u) BENCH_GID=$(id -g)

LETRONNA_LINUX_BIN=/path/to/letronna LETRONNA_GIT_SHA=$(git -C /path/to/letronna rev-parse --short HEAD) \
  ./bundles/build.sh letronna
```

The build copies the binary into the bundle and runs `letronna --version`
inside Ubuntu 22.04, so a binary that cannot start fails there rather than
in a task. `LETRONNA_GIT_SHA` is the commit the binary was built from, and
every `runs.jsonl` row records it as the build, as OpenHuman's is.

Leave `.env`'s controls as they are, so the numbers stand beside the
published ones: `BENCH_MODEL=deepseek/deepseek-v4.1-flash`,
`BENCH_REASONING=high`, `BENCH_PROVIDER=DeepSeek` (pinned, no fallbacks, so
the prompt cache is not split between backends), `BENCH_CPUS=4`,
`BENCH_MEM=8g`. Build the other harnesses' bundles too (`RUNBOOK.md`) to run
them on the same host; the published runs were measured on another one and
are not like for like with yours.

## Run

Use a fresh run id each time: the proxy's `results/meter.jsonl` is never
cleared, and a reused id counts twice.

Micro suite, five small tasks, three times each (a smoke test of the setup):

```sh
HARNESSES=letronna REPEAT=3 ./run-micro.sh letronna-micro-1
```

SWE-bench Verified, the rig's ten instances at 1200 s each. The grader keys
predictions by instance, so `--repeat` would grade only one of each; run
three ids instead:

```sh
uv venv --python 3.12 .cache/swebench-venv
uv pip install --python .cache/swebench-venv/bin/python swebench datasets
.cache/swebench-venv/bin/python swebench/prepare.py --n 10 --seed 20261001 --out tasks/generated/swe --pull
for r in 1 2 3; do HARNESSES=letronna TASK_TIMEOUT_S=1200 ./run-swe.sh letronna-swe10-r$r; done
```

DeepSWE, the rig's ten tasks at 3600 s each, three times (its grader keys
repeats apart):

```sh
HF_TOKEN=... uvx --from huggingface_hub hf download datacurve/deep-swe --repo-type dataset --local-dir .cache/deepswe
node deepswe/prepare.mjs --only "$(grep -v '^#' deepswe/instances-10.txt | paste -sd, -)" --pull
HARNESSES=letronna REPEAT=3 TASK_TIMEOUT_S=3600 ./run-deepswe.sh letronna-deepswe10-1
```

Terminal-Bench 2.0, the rig's five-task sample, through Harbor with each
task's own limits, network and timeout. Harbor files trials by task, so
again three ids:

```sh
uv tool install harbor
tbench/prepull.sh 2 tbench/instances-tb2-5.txt
docker network prune -f
for r in 1 2 3; do HARNESS=letronna ./run-tbench.sh 2 letronna-tb2-r$r; done
```

Each run ends with `node report.mjs`, which writes
`results/<run>/summary.md` and `summary.json`. `node charts.mjs --run <run>
--png` draws them, with Letronna highlighted in violet, and
`node viewer/server.mjs` shows every captured prompt and response.

## What the adapter does

`letronna.sh` follows the contract of the rig's other adapters: the runner
starts it in the task's working directory with the prompt in `PROMPT_FILE`,
under `TASK_TIMEOUT_S`, and keeps its output as the task's `harness.log`.

- It gives Letronna a fresh data folder and points it at the proxy with
  `AGENT_BASE_URL`, `AGENT_API_KEY` (the rig's dummy key) and
  `AGENT_MODEL`, so nothing is set up or saved. The proxy pins the model
  and reasoning whatever Letronna asks for.
- It runs `letronna -p "$PROMPT" --output json --mode bypass --max-rounds
  300`. The result line lands in `harness.log` and alone in
  `letronna.json` beside it.
- `LETRONNA_DEADLINE`, in whole seconds from the start, follows the rig's
  turn budget (`runner/turn-budget.mjs`): the task's budget less
  `BENCH_TURN_MARGIN_S`, 120 s unless set, but at least half of it. At 80%
  Letronna is told to write its outputs, and at the deadline it stops with
  its result line, before the runner's kill. Under Harbor the budget is the
  task's own agent timeout; one Harbor cannot tell sets no deadline, as for
  OpenHuman.
- `LETRONNA_READ_TIMEOUT=1200` lets a reply stay silent for 20 minutes
  rather than 5, so a call the pinned provider is slow to start under load
  is waited out, as the other harnesses wait.
- `LETRONNA_DISPOSABLE=1`: the container is thrown away after the task, so
  an unwatched Bypass run may run what is hard to take back, such as
  `rm -r` or `git reset --hard`, as the other harnesses' permission
  bypasses do. What Letronna never runs, it still refuses.
- `LETRONNA_DISPOSABLE_FILES=1`: its files are thrown away with it, so a
  run that has read a web page or a task document may still move, write
  over and delete the task's files. Nothing that sends data off the machine
  is let through by it.
- `LETRONNA_NO_LOGIN_ENV=1`, `BROWSER=/usr/bin/false` and `NO_COLOR=1`: the
  task image's PATH and python, no browser, no colour codes.

### Plain HTTP to the proxy

OpenHuman refuses to send its key to a plain-`http` endpoint that is not
loopback, so its adapter goes through a loopback forwarder. Letronna is
stricter: `chat::validate` (`crates/core/src/chat.rs`) refuses any `http://`
endpoint other than `localhost`, `127.0.0.1` or `[::1]`, with a key or
without one, unless `LETRONNA_ALLOW_HTTP=1`. That admits local-network hosts
only -- private and link-local addresses, names without a dot such as
`meter-proxy`, and `.internal`, `.local` or `.lan` names (`web::local`) --
never a public one. So on the compose network the adapter sets it and calls
`http://meter-proxy:8080` directly, with no forwarder in the cold start or
the latency; the key it sends is the rig's dummy one. Under Harbor the proxy
is a unix socket, which the rig's `forward.mjs` puts on `127.0.0.1:18080`,
as for OpenHuman.

## The patch

`rig.patch`, against `PIN`:

- `bundles/adapters/letronna.sh`: a copy of `letronna.sh` here.
- `bundles/Dockerfile`: a `letronna` target on the rig's base image (Ubuntu
  22.04 with the Node that runs `runner/entry.mjs`), holding the binary,
  its `GIT_SHA`, `forward.mjs` and the adapter.
- `bundles/build.sh`: `letronna` stages `LETRONNA_LINUX_BIN` as a build
  context and passes `LETRONNA_GIT_SHA`.
- `run-meta.mjs`: reads the bundle's `letronna/GIT_SHA` as the build, as it
  reads OpenHuman's, with a test in `run-meta.test.mjs`.
- `charts.mjs` and `format.mjs`: Letronna's column in the charts' spare
  violet highlight, and its place in the lineup, after OpenHuman.
- `tbench/run.mjs`, `tbench/openhuman_agent.py` and `run-tbench.sh`:
  `--harness letronna` (`HARNESS=letronna`) runs that bundle through the
  same Harbor agent and entry, with its calls tagged and its results filed
  under its own name.

After changing `letronna.sh`, copy it into a checkout at `PIN` with the
patch applied and make the patch again:

```sh
cp scripts/openhuman-bench/letronna.sh /path/to/openhuman-benchmarks/bundles/adapters/
git -C /path/to/openhuman-benchmarks add -A
git -C /path/to/openhuman-benchmarks diff --cached > scripts/openhuman-bench/rig.patch
```

With the patch applied, the rig's own tests pass where they did before
(`node --test *.test.mjs viewer/*.test.mjs runner/*.test.mjs
meter-proxy/*.test.mjs compaction/*.test.mjs`; on macOS the one that reads
`/proc` fails either way).

## Reading the numbers

- Ten tasks and three runs is still a small sample: treat a difference of a
  task or two as noise.
- The SWE and DeepSWE containers reach only the proxy, so Letronna's web
  search and browser fail there, as every harness's web tools do.
  Terminal-Bench tasks have the network they declare, usually the internet,
  and their solutions are public: read the captures before trusting a pass.
- Tokens, cost, cache and timing are the proxy's, measured on the wire, not
  the counts in `letronna.json`.
