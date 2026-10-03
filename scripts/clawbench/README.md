# ClawBench for Letronna

Runs `letronna -p` on [ClawBench](https://github.com/TIGER-AI-Lab/ClawBench)
(TIGER-AI-Lab): everyday tasks on live websites -- V1's 152, the 20 of
V1-lite, V2's 129 -- each in a container with Chrome, a recorder extension,
and an interceptor that stops the run at the request that would commit it,
an order, a booking or a sent form, which is then scored. HarnessBench
(reacher-z), which compares harnesses on these tasks, hands its runs to
ClawBench and cannot run them itself yet, so this is the way to score
Letronna's browser.

Nothing in Letronna changes. The harness is a folder in a ClawBench fork, as
each of ClawBench's harnesses is, and drives Letronna's terminal agent from
outside.

| File | What it is |
| --- | --- |
| `Dockerfile.letronna` | The image: Letronna built from source at a pinned commit, on ClawBench's base |
| `setup-letronna.sh` | Writes Letronna's settings and key from the model ClawBench hands over |
| `run-letronna.sh` | Runs Letronna on the task, stops it when ClawBench says, and keeps its transcript |
| `usage-emitter.py` | Turns the transcript into ClawBench's usage rows |
| `fork.patch` | Adds the four, a registry row and its tests to ClawBench at `187cd252bc60af8ac3a2c98a87c9316e49a5ac75` |
| `test_scripts.py`, `test_usage_emitter.py` | Tests, with a fake `letronna` and no Docker or network |

## Set up

```sh
git clone https://github.com/TIGER-AI-Lab/ClawBench
cd ClawBench
git checkout 187cd252bc60af8ac3a2c98a87c9316e49a5ac75
git apply /path/to/letronna/scripts/clawbench/fork.patch
cp models/models.example.yaml models/models.yaml   # a base URL, API type and key per model
```

ClawBench builds its images before each run, but Letronna's repo is private,
so its builder needs a GitHub token that can read it, which ClawBench's build
does not pass. Build the two images once by hand:

```sh
docker build -f src/clawbench/runtime/harnesses/base/Dockerfile.base \
  -t clawbench-base src/clawbench/runtime
GITHUB_TOKEN=... docker build --secret id=letronna_token,env=GITHUB_TOKEN \
  -f src/clawbench/runtime/harnesses/letronna/Dockerfile.letronna \
  -t clawbench-letronna src/clawbench/runtime
```

ClawBench's own builds then take Letronna's layers from the cache. If they
have left it, its build fails at the fetch: build by hand again, or run with
`--no-build`. So does the fork's `check-harness-build` workflow, which is
given no token either. `LETRONNA_REF` at the top of the Dockerfile is the
commit built; `--build-arg LETRONNA_REF=<sha>` builds another.

## Run

```sh
uv run clawbench-run test-cases/v1-lite/<case-dir> <model-name> --harness letronna
uv run clawbench-batch --models <model-name> --cases-suite v1-lite --all-cases --harness letronna
```

Before trusting a score, run V1-lite with `--harness null` and
`--harness random-click` too. Those are the floors, and a Letronna run should
show what they cannot: some intercepted requests, a non-empty
`data/agent-messages.jsonl`, and no run classed `infra_failure`.

## What the harness does

Setup writes `settings.json` and `keys.json` into `/root/.letronna`, which is
`AGENT_DATA_DIR`. `letronna -p` stops at once with no model set up, and has
no flag or variable for an endpoint, so these files are the way in.

- The Custom gateway points at `BASE_URL` for `openai-completions`,
  `openai-responses` and OpenRouter; for OpenRouter the model's full id is
  looked up, since `models.yaml` may name it without its maker. For
  `anthropic-messages` and `google-generative-ai` it points at LiteLLM on
  port 4000, which the run starts, since Letronna speaks only the OpenAI chat
  API. LiteLLM's config holds the key, and sits in the data folder, which
  Letronna's tools refuse to read.
- `browser_shared` is on, so Letronna works in the browser's own context,
  where the recorder extension and ClawBench's stealth profile are. By default
  each conversation gets a private context, which has neither, and the
  recorder would miss its actions.
- Memory review is off: each is a model call the task did not ask for.

The run copies `/my-info` into the workspace, waits for Chrome, and starts:

```sh
letronna -p "$INSTRUCTION" --output json --yes \
  --tools browse,use_page,read_file,list_dir --max-rounds 150 [--effort ...]
```

- `LETRONNA_BROWSER_URL` is ClawBench's CDP address, so Letronna drives that
  Chrome rather than starting its own.
- The tools are the browser and reading ones, as the other harnesses that
  take a command line have: nothing runs a command, writes a file, or reaches
  the web other than through the browser, past the interceptor.
- 150 tool rounds a message, against Letronna's 25: the time limit, not the
  rounds, should end a slow run.
- `THINKING_LEVEL` becomes `--effort`: `off` is `none`, `adaptive` is `auto`.
- The instruction goes as ClawBench gives it, with no hints of the harness's
  own.

The watchdog stops Letronna when the interceptor matches (`eval_matched`),
after 300 s with no browser action (`agent_idle`), or at the task's time
limit (`time_limit_exceeded`). It stops it as Ctrl-C would, which ends the
run and lets go of the browser, then terminates and kills its process group
if it has not gone in 10 s. A script runs what it puts in the background with
Ctrl-C ignored, and Letronna leaves it ignored, so the run script starts it
with job control, which gives it the default back.

A run Letronna ends by itself is `agent_exited`, or `letronna_failed` when
it exits non-zero, which it does only when the run failed, such as on a model
call that kept failing. ClawBench counts `letronna_failed` as the model's
failure, as it does every reason it does not list as its own, unless the run
made no call, action or request at all. `data/letronna-result.json` holds the
run's own JSON: its `status`, `rounds`, tokens and `error`.

Afterwards the session file Letronna wrote as it went becomes
`data/agent-messages.jsonl`: `head` and `meta` records beside a `message`
record per message, with `role`, `content`, `tool_calls`, and each reply's
tokens and cost. `usage-emitter.py` makes one `data/usage.jsonl` row from each
reply, and from each summary a reply waited for.

## Reading the numbers

- Letronna reaches this Chrome by address, so it takes it for one on another
  machine: `use_page` refuses to upload a file, as one from this machine
  could not reach it, and downloads are left to Chrome. A task that needs a
  file uploaded, such as a resume from `./my-info`, fails at that step.
  Letting a browser at a loopback address upload would be a change to
  Letronna's core.
- A cost is Letronna's own figure: billed, for OpenRouter, else from the
  model's listed price. With neither, the row says `price_unavailable`.
- V1-lite's judge, `eval/agentic_eval.md`, reads `agent-messages.jsonl` with
  the recording, actions and requests, so Letronna's transcript is part of
  what it grades.
- The image has no Letronna account, so nothing of Letronna's hosted service
  is used; the tools given need none.

## Tests and the patch

```sh
python3 -m pip install pytest
PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=/path/to/ClawBench/src \
  python3 -m pytest -p no:cacheprovider scripts/clawbench
```

The run script's tests take about 25 seconds each, most of it the recording's
grace after a run. With ClawBench's `src` on `PYTHONPATH`, a test also sums the
usage rows with ClawBench's own code. In the ClawBench checkout, with the
patch applied, its tests cover the registry row:

```sh
uv run --with pytest --with jsonschema python -m pytest tests/test_harness_registry.py
```

`fork.patch` carries copies of the four harness files, and a test checks
that they are the same as the ones here. After changing one, copy them into a
checkout with the patch applied and make the patch again:

```sh
cp scripts/clawbench/{Dockerfile.letronna,setup-letronna.sh,run-letronna.sh,usage-emitter.py} \
  /path/to/ClawBench/src/clawbench/runtime/harnesses/letronna/
git -C /path/to/ClawBench add -A src tests AGENTS.md CHANGELOG.md
git -C /path/to/ClawBench diff --cached > scripts/clawbench/fork.patch
```

None of this has been run in Docker yet: the image build, Letronna against
ClawBench's Chrome, and the floors above are still to be checked.
