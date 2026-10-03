"""Report on a paired run: scores, reliability, paired deltas and failure buckets.

    python3 scripts/bench/report.py OUT [--known-issues FILE] [--band]

Reads the manifest and results run.py left in OUT and prints Markdown. Every
task in the manifest counts in every denominator: a run with no result, or one
that infra trouble spoiled even after its reruns, scores 0 instead of dropping
out, because dropping the runs that broke overstates the score (Raven's SOP,
section 0). The score is the bench's own combined score. A run passes when
every oracle check passed and the security gate held: pass^k needs a yes or
no, and the oracle is the part of the score no judge can move. Arms are paired
per task, the mean of one arm's k runs against the other's, so the gap between
easy and hard tasks cancels out and only the runs' own noise is left; a
difference is called only past two standard errors. Completion, the outcome
part of the score, is also given over each task's ceiling in the known-issues
ledger, the most any output can score there, so a capped task does not read as
unfinished; a failed check the ledger lists goes in the oracle-defect bucket.
--band lists the tasks the baseline passed 30-70% of the time, less those the
ledger keeps from tuning, which is how the dev subset is chosen from a full run.
"""

from __future__ import annotations

import argparse
import json
import math
import statistics
import sys
from collections import Counter
from pathlib import Path

from buckets import (
    BUCKETS,
    KNOWN_ISSUES,
    KnownIssues,
    check_bucket,
    check_id,
    infra_reason,
    load_known_issues,
    run_bucket,
)
from run import ARMS, newest_result, run_dir

# Raven's bar for crediting a change: the mean paired difference must sit two
# standard errors from zero. At k=3 that is about 3-4 points on Terminal-Bench,
# against a single-run swing of about 5.
SIGMAS = 2.0

# The pass-rate band whose tasks keep a ranking with far fewer tasks
# (arXiv 2603.23749): a task every run passes or fails cannot show a change.
BAND = (0.3, 0.7)

# When every task moved by exactly the same amount there is no spread to take
# a standard error from, so the sign test decides instead, at the usual 5%.
SIGN_P = 0.05


def spoiled(result: dict | None) -> bool:
    """Whether a run has no result or one that infra trouble spoiled."""
    return result is None or infra_reason(result) is not None


def score(result: dict | None) -> float:
    """The bench's combined score for a run, or 0 when it has none or infra spoiled it."""
    if spoiled(result):
        return 0.0
    return float((result.get("scoring") or {}).get("combined_score") or 0.0)


def completion(result: dict | None) -> float:
    """The bench's outcome score for a run, or 0 when it has none or infra spoiled it."""
    if spoiled(result):
        return 0.0
    return float((result.get("scoring") or {}).get("outcome_score") or 0.0)


def passed(result: dict | None) -> bool:
    """Whether a run completed every oracle check without tripping the security gate."""
    if spoiled(result):
        return False
    scoring = result.get("scoring") or {}
    # Not outcome_score: on the image tasks that blends in the judge's quality.
    oracle = scoring.get("oracle_outcome_score")
    if oracle is None:
        oracle = (result.get("oracle_result") or {}).get("outcome_score")
    return (oracle or 0.0) >= 1.0 and scoring.get("security_score") != 0.0


def arm_summary(
    tasks: list[str], runs: list[dict[str, dict | None]], ceilings: dict[str, float]
) -> dict:
    """Sums up one arm's k runs, every task in every denominator."""
    scores = [[score(run.get(task_id)) for task_id in tasks] for run in runs]
    passes = [[passed(run.get(task_id)) for task_id in tasks] for run in runs]
    done = [[completion(run.get(task_id)) for task_id in tasks] for run in runs]
    columns = range(len(tasks))
    return {
        "per_run": [statistics.fmean(row) for row in scores],
        "mean": statistics.fmean(s for row in scores for s in row),
        "completion": statistics.fmean(c for row in done for c in row),
        "completion_of_ceiling": statistics.fmean(
            row[i] / ceilings.get(task_id, 1.0) for row in done for i, task_id in enumerate(tasks)
        ),
        "pass_at_1": statistics.fmean(p for row in passes for p in row),
        "pass_hat_k": statistics.fmean(all(row[i] for row in passes) for i in columns),
        "best_of_k": statistics.fmean(max(row[i] for row in scores) for i in columns),
        "infra": sum(spoiled(run.get(task_id)) for run in runs for task_id in tasks),
    }


def task_mean(runs: list[dict], task_id: str) -> float:
    """A task's mean score over one arm's runs."""
    return statistics.fmean(score(run.get(task_id)) for run in runs)


def task_deltas(tasks: list[str], baseline: list[dict], candidate: list[dict]) -> list[float]:
    """Each task's candidate mean score minus its baseline mean score."""
    # Rounded, so equal scores summed in another order are no change.
    return [
        round(task_mean(candidate, task_id) - task_mean(baseline, task_id), 6)
        for task_id in tasks
    ]


def paired(deltas: list[float]) -> dict:
    """The mean per-task difference, its standard error and z, called only past SIGMAS."""
    mean = statistics.fmean(deltas)
    se = statistics.stdev(deltas) / math.sqrt(len(deltas)) if len(deltas) > 1 else math.inf
    if se == 0.0:
        z = 0.0 if mean == 0.0 else math.copysign(math.inf, mean)
    else:
        z = mean / se
    call = "win" if z >= SIGMAS else "loss" if z <= -SIGMAS else "no call"
    if se == 0.0 and sign_test(deltas) >= SIGN_P:
        call = "no call"
    return {"mean": mean, "se": se, "z": z, "call": call}


def sign_test(deltas: list[float]) -> float:
    """The two-sided sign-test p-value for tasks moving one way more than the other."""
    up, down = sum(d > 0 for d in deltas), sum(d < 0 for d in deltas)
    moved = up + down
    if moved == 0:
        return 1.0
    tail = sum(math.comb(moved, i) for i in range(min(up, down) + 1)) / 2**moved
    return min(1.0, 2 * tail)


def check_moves(
    tasks: list[str], baseline: list[dict], candidate: list[dict], defects: set
) -> list[tuple[str, str, str, int, int]]:
    """Each oracle check the two arms passed a different number of times, biggest moves first."""
    moves = []
    for task_id in tasks:
        counts: dict[str, list] = {}
        for side, runs in enumerate((baseline, candidate), start=1):
            for run in runs:
                result = run.get(task_id)
                if spoiled(result):
                    continue
                for check in (result.get("oracle_result") or {}).get("checks") or []:
                    entry = counts.setdefault(check_id(check), [check, 0, 0])
                    entry[side] += bool(check.get("pass"))
        for name, (check, before, after) in counts.items():
            if before != after:
                moves.append((task_id, name, check_bucket(task_id, check, defects), before, after))
    return sorted(moves, key=lambda move: -abs(move[4] - move[3]))


def in_band(tasks: list[str], runs: list[dict], not_for_tuning: set[str]) -> list[str]:
    """The tasks not kept from tuning whose pass rate over the given runs falls inside BAND."""
    low, high = BAND
    return [
        task_id
        for task_id in tasks
        if low <= statistics.fmean(passed(run.get(task_id)) for run in runs) <= high
        and task_id not in not_for_tuning
    ]


def load(out: Path) -> tuple[dict, dict, dict]:
    """Reads a run's manifest, its results by model and arm, and the reruns infra cost each."""
    try:
        manifest = json.loads((out / "manifest.json").read_text(encoding="utf-8"))
    except (OSError, ValueError) as err:
        raise SystemExit(f"{out} has no readable manifest.json from run.py: {err}") from err
    runs, reruns = {}, {}
    for model in manifest["models"]:
        for arm in ARMS:
            folders = [run_dir(out, model, arm, repeat) for repeat in range(1, manifest["k"] + 1)]
            runs[model, arm] = [
                {task_id: newest_result(folder, task_id) for task_id in manifest["tasks"]}
                for folder in folders
            ]
            reruns[model, arm] = 0
            for folder in folders:
                ledger = folder / "attempts.json"
                if not ledger.is_file():
                    continue
                try:
                    attempts = json.loads(ledger.read_text(encoding="utf-8"))
                except (OSError, ValueError) as err:
                    raise SystemExit(
                        f"{ledger} cannot be read ({err}); delete it to count that folder's "
                        "reruns as none."
                    ) from err
                reruns[model, arm] += sum(max(0, count - 1) for count in attempts.values())
    return manifest, runs, reruns


def each_run(summary: dict) -> str:
    """One arm's score in each of its runs, so the single-run swing shows."""
    return " ".join(f"{s:.3f}" for s in summary["per_run"])


def render(manifest: dict, runs: dict, reruns: dict, known: KnownIssues) -> str:
    """The Markdown report for a loaded run."""
    tasks, k = manifest["tasks"], manifest["k"]
    capped = sum(known.ceilings.get(task_id, 1.0) < 1.0 for task_id in tasks)
    lines = [
        "# Paired run: candidate against baseline",
        "",
        f"Harness-Bench {manifest['bench_commit'][:12]}, judge {manifest['judge']}, "
        f"{len(tasks)} tasks, {k} runs per task and arm. Every task counts in every "
        f"denominator; infra failures score 0. {capped} of them have an oracle no output "
        "can fully satisfy, so completion is also shown over each task's ceiling, the most "
        "any output can score there.",
    ]
    for model in manifest["models"]:
        base, cand = runs[model, "baseline"], runs[model, "candidate"]
        sums = {arm: arm_summary(tasks, runs[model, arm], known.ceilings) for arm in ARMS}
        best = f"{sums['baseline']['best_of_k']:.3f}"
        table = [
            (f"score, mean of {k} runs", *(f"{sums[arm]['mean']:.3f}" for arm in ARMS)),
            ("score of each run", *(each_run(sums[arm]) for arm in ARMS)),
            ("completion", *(f"{sums[arm]['completion']:.3f}" for arm in ARMS)),
            (
                "completion over each task's ceiling",
                *(f"{sums[arm]['completion_of_ceiling']:.3f}" for arm in ARMS),
            ),
            ("pass@1", *(f"{sums[arm]['pass_at_1']:.3f}" for arm in ARMS)),
            (f"pass^{k}", *(f"{sums[arm]['pass_hat_k']:.3f}" for arm in ARMS)),
            ("runs left at 0 by infra", *(str(sums[arm]["infra"]) for arm in ARMS)),
            ("reruns for infra", *(str(reruns[model, arm]) for arm in ARMS)),
            (f"best of {k}, oracle pick (upper bound on self-select)", best, ""),
        ]
        deltas = task_deltas(tasks, base, cand)
        verdict = paired(deltas)
        up, down = sum(d > 0 for d in deltas), sum(d < 0 for d in deltas)
        lines += ["", f"## {model}", "", "| | baseline | candidate |", "|---|---|---|"]
        lines += [f"| {name} | {before} | {after} |" for name, before, after in table]
        lines += [
            "",
            f"Paired over {len(tasks)} tasks: {verdict['mean']:+.3f} (se {verdict['se']:.3f}, "
            f"z {verdict['z']:.2f}), **{verdict['call']}** at {SIGMAS:g} sigma. "
            f"Sign test: {up} up, {down} down, p = {sign_test(deltas):.3f}.",
            "",
            "| task | baseline | candidate | delta |",
            "|---|---|---|---|",
        ]
        for task_id, delta in sorted(zip(tasks, deltas), key=lambda pair: pair[1]):
            before, after = task_mean(base, task_id), task_mean(cand, task_id)
            lines.append(f"| {task_id} | {before:.3f} | {after:.3f} | {delta:+.3f} |")
        counts = {
            arm: Counter(
                run_bucket(run.get(task_id), known.checks, known.ceilings)
                for run in runs[model, arm]
                for task_id in tasks
                if not passed(run.get(task_id))
            )
            for arm in ARMS
        }
        lines += ["", "Failed runs by bucket:", ""]
        lines += ["| bucket | baseline | candidate |", "|---|---|---|"]
        lines += [
            f"| {bucket} | {counts['baseline'][bucket]} | {counts['candidate'][bucket]} |"
            for bucket in BUCKETS
            if counts["baseline"][bucket] or counts["candidate"][bucket]
        ]
        zeroed = [
            f"- {task_id}, {arm} run {repeat}: {infra_reason(result) if result else 'no result'}"
            for arm in ARMS
            for repeat, run in enumerate(runs[model, arm], start=1)
            for task_id in tasks
            if spoiled(result := run.get(task_id))
        ]
        if zeroed:
            lines += ["", "Scored 0 for infra:", "", *zeroed]
        moves = check_moves(tasks, base, cand, known.checks)
        if moves:
            lines += ["", f"Oracle checks that moved (runs passed, of {k}):", ""]
            lines += ["| task | check | bucket | baseline | candidate |", "|---|---|---|---|---|"]
            lines += [f"| {' | '.join(map(str, move))} |" for move in moves]
    return "\n".join(lines) + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Report on a paired Harness-Bench run by run.py.")
    parser.add_argument("out", type=Path, help="the --out folder of run.py")
    parser.add_argument(
        "--known-issues",
        type=Path,
        default=KNOWN_ISSUES,
        help="the ledger of wrong oracle checks, pinned to the commit the run used",
    )
    parser.add_argument(
        "--band",
        action="store_true",
        help="list the tasks for tuning the baseline passed 30-70%% of the time, and stop",
    )
    args = parser.parse_args(argv)
    manifest, runs, reruns = load(args.out.expanduser().resolve())
    known = load_known_issues(args.known_issues)
    if manifest["bench_commit"] != known.commit:
        raise SystemExit(
            f"{args.out} ran Harness-Bench {manifest['bench_commit']}, but {args.known_issues} "
            f"is pinned to {known.commit}; pass --known-issues with a ledger checked at the "
            "commit the run used."
        )
    if args.band:
        baseline = [run for model in manifest["models"] for run in runs[model, "baseline"]]
        print("\n".join(in_band(manifest["tasks"], baseline, known.not_for_tuning)))
        return 0
    sys.stdout.write(render(manifest, runs, reruns, known))
    return 0


if __name__ == "__main__":
    sys.exit(main())
