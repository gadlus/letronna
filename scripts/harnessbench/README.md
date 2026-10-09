# Harness-Bench for Letronna

Runs `letronna -p` on [Harness-Bench](https://github.com/Qihoo360/harness-bench)
(Qihoo360, 106 tasks) so its scores can be trusted: every model call goes
through the harness's usage proxy, so the process and security judge has a
trace to read; multi-round tasks carry the conversation on with `-c`; a task
that runs out of time is stopped by the adapter and still scored by its
oracle; and a pass afterwards marks the runs whose grading did not happen.

| File | What it is |
| --- | --- |
| `letronna.py` | The adapter, `LetronnaAdapter`, for Harness-Bench's `adapters/` |
| `fork.patch` | Adds that adapter, its registry branch and an example entry to Harness-Bench at `1025086a446653702b80cfb48babbeec35db6b2c` |
| `validate.py` | Marks results invalid after a suite and says which to leave out |
| `stub_server.py` | A stand-in model server for a smoke run with no provider |
| `test_letronna.py`, `test_validate.py` | Tests, with a fake `letronna` and no network |

## Set up

```sh
git clone https://github.com/Qihoo360/harness-bench
cd harness-bench
git checkout 1025086a446653702b80cfb48babbeec35db6b2c
git apply /path/to/letronna/scripts/harnessbench/fork.patch
python3 -m pip install -e .   # PyYAML: the tasks' task.yaml files are YAML
```

Build Letronna's terminal agent, or use the installed `letronna`:

```sh
cargo build --release -p letronna-cli   # target/release/letronna
```

Add an entry to `config/harness.yaml` (the patch puts the same one, as
`letronna-local`, in `config/harness.example.yaml`):

```json
{
  "models": {
    "letronna-gpt-5.4": {
      "adapter": "letronna",
      "command": "/path/to/letronna",
      "session_prefix": "harnessbench-letronna",
      "model": "gpt-5.4",
      "upstream": "https://api.openai.com/v1",
      "api_key_env": "OPENAI_API_KEY"
    }
  }
}
```

- `upstream` is the provider's OpenAI-compatible base URL. Letronna's Custom
  gateway is pointed at the proxy, which forwards to it.
- `api_key_env` names the variable holding the provider's key. The adapter
  writes it to the sandbox's own `keys.json`; leave it out for a server that
  takes no key.
- Leave `timeout_sec` out, so each task's own timeout (180 to 3600 s)
  applies; set here, it would override all of them. Letronna gets
  `LETRONNA_DEADLINE` 120 s short of it (at least half of a short one), so
  at 80% it is told to write its outputs and at the deadline it ends with
  its result line, before the adapter's interrupt.
- `max_rounds` is the tool rounds a message gets, 150 by default rather than
  Letronna's 25. `permission_mode` is `bypass` (`--yes`) by default; `auto`
  runs Letronna's approval judge instead, worth one run as an ablation. That
  judge asks the session's own model through the same proxy route, so its
  verdict calls land in the trace the process judge grades and count in
  `model_calls`: an `auto` run's trace is not the agent's calls alone, and
  its process score is not like for like with a `bypass` run's.
  `extra_args` are added to the command line as they are.

`work_root` in `config/app.yaml` must be an absolute path **outside any git
checkout**. Letronna reads `AGENTS.md` or `CLAUDE.md` in every folder from the
workspace up to the root of the checkout it sits in, and looks for skills up
to there too; the default, `data_try6/sandbox`, is inside the Harness-Bench
checkout, so the agent would take the harness's own files for the project's.

```json
{
  "tasks_dir": "tasks",
  "results_dir": "/srv/hb/run1/results",
  "work_root": "/srv/hb/run1/sandbox"
}
```

## Run

The judge that scores process and security is any OpenAI-compatible chat
API, set with `RUBRIC_*`. The paper used Claude Sonnet 4.6; give
`RUBRIC_MODEL` the name your `RUBRIC_BASE_URL` has for it:

```sh
export OPENAI_API_KEY=...            # the provider key api_key_env names
export RUBRIC_API_KEY=...
export RUBRIC_BASE_URL=https://...   # serving /chat/completions
export RUBRIC_MODEL=claude-sonnet-4.6
PYTHONPATH=src python3 -m harnessbench.cli run-task --task 001-file --harness letronna-gpt-5.4
PYTHONPATH=src python3 -m harnessbench.cli run-suite --harness letronna-gpt-5.4
python3 /path/to/letronna/scripts/harnessbench/validate.py /srv/hb/run1/results
```

Set all three. Without `RUBRIC_API_KEY` the judge falls back to
`OPENAI_API_KEY`, then to the key in `~/.openclaw/openclaw.json`, and
without `RUBRIC_MODEL` it is gpt-4o-mini, not the paper's judge. With no key
at all it is skipped: process and security then default to 1 and the
combined score is the outcome alone.

The harness says nothing when that happens, so run `validate.py` after every
suite. It writes a `validity` verdict into each result and exits 1 if any
run is invalid: no proxy trace, a judge skipped, answering with something
that is not JSON, or leaving out its process scores or security gate, a
vision task's outcome judge giving no quality, or a round that reached no
model through the proxy. It names each invalid run by its path under the
folder and gives the mean combined score of the valid ones per harness
entry. A task that raised an error writes no result at all; `run-suite`
lists it with `"ok": false`.

The patch makes the judge ask again when the gateway is busy: a 429, a 5xx
or a dropped connection is tried three more times, 10, 30 and 60 seconds
apart, and a reply with no JSON in it, or with no scores in its JSON, is
asked for once more. A 400 is not retried.

Each round's metadata in the result holds the run's own JSON (`status`,
`rounds`, tokens, `files`), `timed_out` (also true when Letronna stopped
at its deadline, which ends the task's rounds as a timeout does), and
`model_calls`, the calls the proxy saw answered in that round.

## Reading the numbers

- Run a suite three times before trusting a difference under 2 points. A
  task's result file is overwritten by the next run of it, so give each
  repeat its own `results_dir` and `work_root` (`HARNESSBENCH_APP_CONFIG`
  points at another app config).
- Only 007-session-memory depends on the conversation carrying over between
  rounds; the other multi-round tasks carry their state in files.
- The sandbox's HOME has no Letronna account, so the tools that make
  pictures, speech and video on Letronna's hosted models are not offered.
- The proxy passes a reply on only once it is whole, so a reply reads as
  silent until then. The adapter sets `LETRONNA_READ_TIMEOUT=1200`, the
  proxy's own wait on its upstream, unless the environment already sets
  it; at Letronna's default of five minutes, a reply that took longer to
  generate failed.
- Sandboxes are secret: each holds the provider key in `.letronna/keys.json`.
  The proxy's records in `usage-proxy/responses/` keep each request's
  headers with `Authorization` and the other key headers blanked, so they
  can be kept for grading a run again. `--delete-sandbox` removes it all
  after scoring.

## Smoke run with no provider

`stub_server.py` answers just enough to run 001-file and 007-session-memory
with canned replies. In 007 it writes the passphrase only when round 1's
message comes back in round 2's request, which is what `-c` is for. It also
stands in for the judge, so these runs read as valid:

```sh
python3 /path/to/letronna/scripts/harnessbench/stub_server.py 8771 &
# in config/harness.yaml: "letronna-stub": {"adapter": "letronna", "command": "/path/to/letronna",
#   "model": "stub-model", "upstream": "http://127.0.0.1:8771/v1"}
export RUBRIC_API_KEY=stub RUBRIC_BASE_URL=http://127.0.0.1:8771/v1 RUBRIC_MODEL=stub
PYTHONPATH=src python3 -m harnessbench.cli run-task --task 001-file --harness letronna-stub
PYTHONPATH=src python3 -m harnessbench.cli run-task --task 007-session-memory --harness letronna-stub
```

Then the sandbox's `usage-proxy/requests.jsonl` lists the calls, and the
oracle passes both tasks.

## Tests and the patch

```sh
python3 -m pip install pytest
PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=/path/to/harness-bench/src \
  python3 -m pytest -p no:cacheprovider scripts/harnessbench
```

The two settings keep `__pycache__/` and `.pytest_cache/` out of the
checkout, which does not ignore them.

`fork.patch` carries a copy of `letronna.py`. After changing it, copy it into
a checkout with the patch applied and make the patch again:

```sh
cp scripts/harnessbench/letronna.py /path/to/harness-bench/src/harnessbench/adapters/
git -C /path/to/harness-bench add -A src config
git -C /path/to/harness-bench diff --cached > scripts/harnessbench/fork.patch
```
