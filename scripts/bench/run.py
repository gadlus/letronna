"""Paired Harness-Bench runs of a candidate Letronna binary against a baseline.

    python3 scripts/bench/run.py --bench ~/harness-bench --entry letronna.json \\
        --baseline ./letronna-before --candidate ./letronna-after \\
        --tasks scripts/bench/subsets/harnessbench-dev.txt \\
        --models claude-sonnet-4.6 --k 3 --out ~/bench-runs/today
    python3 scripts/bench/report.py ~/bench-runs/today

Every run goes through the bench's own `python3 -m harnessbench.cli run-task`
and its letronna adapter, so the scores are the bench's. It is run-task and not
run-suite because run-suite only takes a range of task numbers, and an infra
failure has to be rerun on its own. Each arm, model and repeat gets its own app
and harness config, written as JSON (which the bench's loader reads before
YAML), so no result overwrites another. The arms take turns task by task, so a
provider that slows down for an hour slows both, and the arm that goes first
swaps each repeat, so neither always starts in the rate-limit window or cache
the other left. A run that infra trouble spoiled is run at most twice more, and
whatever is still broken counts as 0 in the report. A run that already has a
clean result is skipped, so an interrupted run carries on where it stopped --
but only with the same settings and the same binaries, or the pairs would mix.
It will not start on a bench at any commit but the one the known-issues ledger
was checked against, as its ceilings and wrong checks may not hold there.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
from collections.abc import Callable
from functools import partial
from pathlib import Path

from buckets import KNOWN_ISSUES, infra_reason, load_known_issues

ARMS = ("baseline", "candidate")

# The key the Letronna entry gets in the generated harness config; the bench
# names its result folders after it.
HARNESS_ID = "letronna"

# Raven's ladder (self-evolution-loop SOP section 0): an infra failure is rerun
# at most twice, then scored 0 and kept in the denominator.
MAX_RERUNS = 2

# The judge the Harness-Bench paper fixed for every trajectory, so scores line
# up with its tables.
JUDGE = "claude-sonnet-4.6"


def read_list(path: Path) -> list[str]:
    """Reads the lines of a list file, leaving out blanks and # comments."""
    lines = path.read_text(encoding="utf-8").splitlines()
    return [kept for line in lines if (kept := line.split("#", 1)[0].strip())]


def run_dir(out: Path, model: str, arm: str, repeat: int) -> Path:
    """The folder one repeat of one arm with one model keeps its configs and results in."""
    return out / re.sub(r"[^A-Za-z0-9._-]+", "-", model) / arm / f"run{repeat}"


def newest_result(run: Path, task_id: str) -> dict | None:
    """The latest result Harness-Bench wrote for a task in a run folder, if one is readable."""
    found = sorted((run / "results").glob(f"*/*/{task_id}.json"), key=lambda p: p.stat().st_mtime)
    if not found:
        return None
    try:
        return json.loads(found[-1].read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None


def run_until_clean(task_id: str, run: Path, run_once: Callable[[], None]) -> None:
    """Runs a task until one run is free of infra trouble or it has had MAX_RERUNS reruns."""
    ledger = run / "attempts.json"
    try:
        attempts = json.loads(ledger.read_text(encoding="utf-8")) if ledger.is_file() else {}
    except (OSError, ValueError) as err:
        raise SystemExit(
            f"{ledger} cannot be read ({err}). Delete it to carry on; "
            "the tasks in that folder then get their reruns afresh."
        ) from err
    while attempts.get(task_id, 0) <= MAX_RERUNS:
        result = newest_result(run, task_id)
        if result is not None and infra_reason(result) is None:
            return
        run_once()
        # Counted after the run, so stopping the runner mid-task costs no rerun.
        attempts[task_id] = attempts.get(task_id, 0) + 1
        ledger.write_text(json.dumps(attempts, indent=1), encoding="utf-8")
        result = newest_result(run, task_id)
        reason = f"no result, see logs/{task_id}.log" if result is None else infra_reason(result)
        if reason is not None:
            print(f"  spoiled: {reason}", flush=True)


def bench_env(bench: Path, run: Path, judge: str) -> dict[str, str]:
    """The environment the bench's command line runs with for one run folder."""
    return os.environ | {
        "PYTHONPATH": str(bench / "src"),
        "HARNESSBENCH_APP_CONFIG": str(run / "app.json"),
        "HARNESSBENCH_HARNESS_CONFIG": str(run / "harness.json"),
        "RUBRIC_MODEL": judge,
    }


def run_task(bench: Path, run: Path, task_id: str, judge: str) -> None:
    """Runs one task once through the bench's command line, logging what it prints."""
    print(f"{'/'.join(run.parts[-3:])} {task_id}", flush=True)
    logs = run / "logs"
    logs.mkdir(exist_ok=True)
    with (logs / f"{task_id}.log").open("a", encoding="utf-8") as log:
        command = ["-m", "harnessbench.cli", "run-task", "--task", task_id, "--harness", HARNESS_ID]
        subprocess.run(
            [sys.executable, *command],
            cwd=bench,
            env=bench_env(bench, run, judge),
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=subprocess.STDOUT,
            check=False,
        )


def write_configs(bench: Path, run: Path, entry: dict, binary: Path, model: str) -> None:
    """Writes the app and harness config the bench runs one run folder's tasks with."""
    run.mkdir(parents=True, exist_ok=True)
    app = {
        "tasks_dir": str(bench / "tasks"),
        "results_dir": str(run / "results"),
        "work_root": str(run / "sandbox"),
        "data_dir": str(run / "data"),
    }
    (run / "app.json").write_text(json.dumps(app, indent=1), encoding="utf-8")
    harness = {"models": {HARNESS_ID: entry | {"command": str(binary), "model": model}}}
    (run / "harness.json").write_text(json.dumps(harness, indent=1), encoding="utf-8")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Paired Harness-Bench runs of two binaries.")
    add = parser.add_argument
    add("--bench", type=Path, required=True, help="Harness-Bench checkout with a letronna adapter")
    add(
        "--entry",
        type=Path,
        required=True,
        help="the bench's letronna entry as a JSON object; command and model are filled in",
    )
    add("--baseline", type=Path, required=True, help="the Letronna binary to compare against")
    add("--candidate", type=Path, required=True, help="the Letronna binary being tried")
    add("--tasks", type=Path, required=True, help="file of task ids, one per line")
    add("--models", required=True, help="model ids, separated by commas")
    add("--k", type=int, default=3, help="runs per task, arm and model (default 3)")
    add("--judge", default=JUDGE, help=f"the bench's judge model (default {JUDGE})")
    add("--out", type=Path, required=True, help="folder for the runs, outside any git checkout")
    add(
        "--known-issues",
        type=Path,
        default=KNOWN_ISSUES,
        help="the ledger of wrong oracle checks; the bench must be at the commit it names",
    )
    args = parser.parse_args(argv)

    bench = args.bench.expanduser().resolve()
    if not (bench / "src" / "harnessbench" / "cli.py").is_file():
        raise SystemExit(f"{bench} is not a Harness-Bench checkout: no src/harnessbench/cli.py.")
    binaries = {arm: getattr(args, arm).expanduser().resolve() for arm in ARMS}
    for arm, binary in binaries.items():
        if not (binary.is_file() and os.access(binary, os.X_OK)):
            raise SystemExit(f"The {arm} binary {binary} is missing or cannot be run.")
    try:
        entry = json.loads(args.entry.read_text(encoding="utf-8"))
        tasks = read_list(args.tasks)
    except (OSError, ValueError) as err:
        raise SystemExit(f"Could not read the harness entry or the task list: {err}") from err
    if not isinstance(entry, dict):
        raise SystemExit(f"{args.entry} should hold a JSON object: the letronna entry.")
    models = [model.strip() for model in args.models.split(",") if model.strip()]
    if not tasks or not models or args.k < 1:
        raise SystemExit("Give at least one task, one model, and a --k of 1 or more.")
    repeated = sorted({task_id for task_id in tasks if tasks.count(task_id) > 1})
    if repeated:
        raise SystemExit(f"{args.tasks} lists these tasks more than once: {', '.join(repeated)}")
    unknown = [task_id for task_id in tasks if not (bench / "tasks" / task_id).is_dir()]
    if unknown:
        raise SystemExit(f"These tasks are not in {bench / 'tasks'}: {', '.join(unknown)}")
    # The bench's own reading of the switch, so the manifest says what it did.
    switch = os.environ.get("HARNESSBENCH_SKIP_PROCESS_GRADE", "")
    skip_judge = switch.strip().lower() in ("1", "true", "yes")
    if not (skip_judge or os.environ.get("RUBRIC_API_KEY") or os.environ.get("OPENAI_API_KEY")):
        raise SystemExit(
            "The judge has no key, so every run would be spoiled and rerun: set RUBRIC_API_KEY "
            "(and RUBRIC_BASE_URL), or HARNESSBENCH_SKIP_PROCESS_GRADE=1 to score without it."
        )

    git = ["git", "-C", str(bench), "rev-parse", "HEAD"]
    try:
        commit = subprocess.run(git, capture_output=True, text=True, check=False).stdout.strip()
    except OSError:
        commit = ""
    known = load_known_issues(args.known_issues)
    if commit != known.commit:
        raise SystemExit(
            f"{bench} is at {commit or 'no git commit'}, but {args.known_issues} was checked "
            f"against Harness-Bench {known.commit}. Check that commit out there and apply the "
            "adapter patch again, or check every entry against the new commit and move the pin."
        )
    manifest = {
        "bench": str(bench),
        "bench_commit": commit or "unknown",
        "judge": args.judge,
        "arms": {
            arm: {"binary": str(binary), "sha256": hashlib.sha256(binary.read_bytes()).hexdigest()}
            for arm, binary in binaries.items()
        },
        "models": models,
        "k": args.k,
        "tasks": tasks,
        # Hashed, as the entry may hold an API key.
        "entry_sha256": hashlib.sha256(json.dumps(entry, sort_keys=True).encode()).hexdigest(),
        "skip_process_grade": skip_judge,
    }
    out = args.out.expanduser().resolve()
    # Letronna reads every AGENTS.md and CLAUDE.md from the workspace up to
    # the repository's root, which would put a checkout's own into every run.
    checkout = next((up for up in (out, *out.parents) if os.path.lexists(up / ".git")), None)
    if checkout is not None:
        raise SystemExit(
            f"{out} is inside the git checkout {checkout}, whose AGENTS.md and CLAUDE.md "
            "Letronna would read into every run; use an --out outside any checkout."
        )
    folders: dict[Path, str] = {}
    for model in models:
        folder = run_dir(out, model, ARMS[0], 1)
        if folder in folders:
            raise SystemExit(
                f"The models {folders[folder]} and {model} would share {folder.parents[1]}; "
                "give each model once."
            )
        folders[folder] = model
    manifest_path = out / "manifest.json"
    if manifest_path.is_file():
        try:
            kept = json.loads(manifest_path.read_text(encoding="utf-8"))
        except (OSError, ValueError) as err:
            raise SystemExit(f"{manifest_path} cannot be read ({err}); use a new --out.") from err
        if kept != manifest:
            raise SystemExit(f"{out} holds a run with other settings or binaries; use a new --out.")
    first = run_dir(out, models[0], ARMS[0], 1)
    write_configs(bench, first, entry, binaries[ARMS[0]], models[0])
    # Listing the tasks loads the configs and task files the way a run does,
    # so a setup that can run nothing stops here, not after every rerun.
    listing = subprocess.run(
        [sys.executable, "-m", "harnessbench.cli", "tasks"],
        cwd=bench,
        env=bench_env(bench, first, args.judge),
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        check=False,
    )
    if listing.returncode != 0:
        printed = (listing.stderr or listing.stdout).strip()
        raise SystemExit(f"The bench cannot list its tasks, so no run was started:\n{printed}")
    out.mkdir(parents=True, exist_ok=True)
    manifest_path.write_text(json.dumps(manifest, indent=1), encoding="utf-8")

    for model in models:
        for repeat in range(1, args.k + 1):
            runs = {arm: run_dir(out, model, arm, repeat) for arm in ARMS}
            for arm, run in runs.items():
                write_configs(bench, run, entry, binaries[arm], model)
            for task_id in tasks:
                for arm in ARMS if repeat % 2 else ARMS[::-1]:
                    once = partial(run_task, bench, runs[arm], task_id, args.judge)
                    run_until_clean(task_id, runs[arm], once)
    print(f"Done. Report: python3 {Path(__file__).with_name('report.py')} {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
