"""A Terminal-Bench run's results, one row per task, from Harbor job folders.

    python3 report.py JOB_DIR [JOB_DIR ...] [--oracle ORACLE_JOB_DIR ...] [--csv OUT.csv]

Every trial under the job folders counts; a task run more than once is
reported by its mean reward. With --oracle, a task the oracle could not
pass in the same environment is marked, as its failure says nothing of the
agent. Prints a Markdown summary.
"""

from __future__ import annotations

import argparse
import csv
import json
import sys
from pathlib import Path

from triage import result_line


def trials(job_dirs: list[str]) -> list[dict]:
    rows = []
    for job in job_dirs:
        for result in sorted(Path(job).glob("*__*/result.json")):
            try:
                data = json.loads(result.read_text())
            except ValueError:
                continue
            rewards = (data.get("verifier_result") or {}).get("rewards") or {}
            error = data.get("exception_info") or {}
            agent = data.get("agent_result") or {}
            timing = data.get("agent_execution") or {}
            # What Letronna itself says of the run in its result line, where
            # Harbor's figures stop at tokens and the trial's whole time.
            ran = result_line(result.parent)
            spent = ran.get("timing") or {}
            first = spent.get("first_token") or {}
            rows.append(
                {
                    "task": data.get("task_name") or result.parent.name.rsplit("__", 1)[0],
                    "trial": result.parent.name,
                    "reward": rewards.get("reward"),
                    "error": error.get("exception_type") or "",
                    "input": agent.get("n_input_tokens"),
                    "cached": agent.get("n_cache_tokens"),
                    "output": agent.get("n_output_tokens"),
                    "cost": agent.get("cost_usd"),
                    "seconds": seconds(timing),
                    "rounds": ran.get("rounds"),
                    "model_seconds": spent.get("model_seconds"),
                    "tool_seconds": spent.get("tool_seconds"),
                    "first_token": (first["seconds"] / first["count"]) if first.get("count") else None,
                    "cache_misses": (ran.get("cache_misses") or {}).get("count"),
                }
            )
    return rows


def seconds(timing: dict) -> float | None:
    from datetime import datetime

    try:
        start = datetime.fromisoformat(timing["started_at"])
        end = datetime.fromisoformat(timing["finished_at"])
    except (KeyError, TypeError, ValueError):
        return None
    return (end - start).total_seconds()


def by_task(rows: list[dict]) -> dict[str, list[dict]]:
    tasks: dict[str, list[dict]] = {}
    for row in rows:
        tasks.setdefault(row["task"], []).append(row)
    return tasks


def mean(values):
    values = [v for v in values if v is not None]
    return sum(values) / len(values) if values else None


def minutes(seconds):
    return None if seconds is None else seconds / 60


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("jobs", nargs="+")
    p.add_argument("--oracle", nargs="*", default=[])
    p.add_argument("--csv")
    a = p.parse_args()
    tasks = by_task(trials(a.jobs))
    oracle = {t: mean(r["reward"] for r in rs) for t, rs in by_task(trials(a.oracle)).items()}
    if not tasks:
        sys.exit("No trials with a result.json under " + ", ".join(a.jobs))

    table = []
    for task, rs in sorted(tasks.items()):
        table.append(
            {
                "task": task,
                "runs": len(rs),
                "reward": mean(r["reward"] if r["reward"] is not None else 0.0 for r in rs),
                "errors": ";".join(sorted({r["error"] for r in rs if r["error"]})),
                "oracle": oracle.get(task),
                "input": mean(r["input"] for r in rs),
                "cached": mean(r["cached"] for r in rs),
                "output": mean(r["output"] for r in rs),
                "cost": mean(r["cost"] for r in rs),
                "minutes": (mean(r["seconds"] for r in rs) or 0) / 60,
                "rounds": mean(r["rounds"] for r in rs),
                "model_minutes": minutes(mean(r["model_seconds"] for r in rs)),
                "tool_minutes": minutes(mean(r["tool_seconds"] for r in rs)),
                "first_token_seconds": mean(r["first_token"] for r in rs),
                "cache_misses": mean(r["cache_misses"] for r in rs),
            }
        )
    if a.csv:
        with open(a.csv, "w", newline="") as f:
            writer = csv.DictWriter(f, fieldnames=list(table[0]))
            writer.writeheader()
            writer.writerows(table)

    n = len(table)
    passed = sum(1 for t in table if t["reward"] == 1.0)
    score = mean(t["reward"] for t in table)
    print(f"**{n} tasks, {sum(t['runs'] for t in table)} trials.** "
          f"Mean reward {score:.3f}; {passed} passed every run.")
    if oracle:
        broken = [t["task"] for t in table if t["oracle"] is not None and t["oracle"] < 1.0]
        fair = [t for t in table if t["task"] not in broken]
        print(f"The oracle failed {len(broken)} of these here ({', '.join(broken) or 'none'}); "
              f"on the other {len(fair)}, mean reward {mean(t['reward'] for t in fair):.3f}.")
    errors: dict[str, int] = {}
    for t in table:
        for e in filter(None, t["errors"].split(";")):
            errors[e] = errors.get(e, 0) + 1
    if errors:
        print("Errors: " + ", ".join(f"{k} {v}" for k, v in sorted(errors.items())) + ".")
    tokens = mean((t["input"] or 0) + (t["output"] or 0) for t in table)
    cost = sum(t["cost"] or 0 for t in table)
    print(f"Per task: {tokens / 1000:.0f}K tokens with cache reads, "
          f"{mean(t['minutes'] for t in table):.1f} min; cost ${cost:.2f} in all.\n")
    # Where the time went, for a build whose result line says: an older
    # build's table is as it was.
    timed = any(t["model_minutes"] is not None for t in table)
    if timed:
        waited = mean(t["first_token_seconds"] for t in table)
        misses = sum(t["cache_misses"] or 0 for t in table)
        print(f"Of that, per task: {mean(t['model_minutes'] for t in table):.1f} min waiting on "
              f"the model ({waited or 0:.1f} s to a reply's first words) and "
              f"{mean(t['tool_minutes'] for t in table) or 0:.1f} min in tools; "
              f"{misses:.0f} cache misses in all.\n")
    more = " Rounds | Model min | Tools min | First words s | Cache misses |" if timed else ""
    print("| Task | Runs | Reward | Oracle | Error | Tokens | Minutes |" + more)
    print("|---|---|---|---|---|---|---|" + ("---|---|---|---|---|" if timed else ""))
    num = lambda v, f: "" if v is None else format(v, f)  # noqa: E731
    for t in table:
        tok = ((t["input"] or 0) + (t["output"] or 0)) / 1000
        orc = "" if t["oracle"] is None else f"{t['oracle']:.0f}"
        more = ""
        if timed:
            more = (f" {num(t['rounds'], '.0f')} | {num(t['model_minutes'], '.1f')} "
                    f"| {num(t['tool_minutes'], '.1f')} | {num(t['first_token_seconds'], '.1f')} "
                    f"| {num(t['cache_misses'], '.0f')} |")
        print(f"| {t['task']} | {t['runs']} | {t['reward']:.2f} | {orc} | {t['errors']} "
              f"| {tok:.0f}K | {t['minutes']:.1f} |" + more)


if __name__ == "__main__":
    main()
