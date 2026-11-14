"""Two builds of Letronna on the same Terminal-Bench tasks, task by task.

    python3 pair.py --base BASE_JOB_DIR [...] --candidate CANDIDATE_JOB_DIR [...] [--csv OUT.csv]

Every task either side ran is a row: its reward under each build, tokens and
minutes beside them, and whether the candidate gained or lost it. A task run
more than once counts by its mean reward, and passes only at 1.0. One trial
a task says little by itself: a task or two either way is noise, so read
which tasks moved and why (triage.py), not the difference in the totals.
"""

from __future__ import annotations

import argparse
import csv
import sys

from report import by_task, mean, trials


def side(job_dirs: list[str]) -> dict[str, dict]:
    """Each task's mean reward, tokens and minutes over `job_dirs`."""
    tasks = {}
    for task, rows in by_task(trials(job_dirs)).items():
        tasks[task] = {
            "reward": mean(r["reward"] if r["reward"] is not None else 0.0 for r in rows),
            "tokens": mean((r["input"] or 0) + (r["output"] or 0) for r in rows),
            "minutes": (mean(r["seconds"] for r in rows) or 0) / 60,
        }
    return tasks


def paired(base: dict[str, dict], candidate: dict[str, dict]) -> list[dict]:
    """A row for each task either side ran, with what the candidate did to it."""
    rows = []
    for task in sorted(set(base) | set(candidate)):
        b, c = base.get(task), candidate.get(task)
        if b is None or c is None:
            moved = "base only" if c is None else "candidate only"
        elif c["reward"] == 1.0 and b["reward"] < 1.0:
            moved = "gained"
        elif b["reward"] == 1.0 and c["reward"] < 1.0:
            moved = "lost"
        else:
            moved = ""
        rows.append(
            {
                "task": task,
                "base": None if b is None else b["reward"],
                "candidate": None if c is None else c["reward"],
                "moved": moved,
                "base_tokens": None if b is None else b["tokens"],
                "candidate_tokens": None if c is None else c["tokens"],
                "base_minutes": None if b is None else b["minutes"],
                "candidate_minutes": None if c is None else c["minutes"],
            }
        )
    return rows


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--base", nargs="+", required=True)
    p.add_argument("--candidate", nargs="+", required=True)
    p.add_argument("--csv")
    a = p.parse_args()
    rows = paired(side(a.base), side(a.candidate))
    both = [r for r in rows if r["base"] is not None and r["candidate"] is not None]
    if not both:
        sys.exit("No task has a result under both builds.")
    if a.csv:
        with open(a.csv, "w", newline="") as f:
            writer = csv.DictWriter(f, fieldnames=list(rows[0]))
            writer.writeheader()
            writer.writerows(rows)

    gained = [r["task"] for r in both if r["moved"] == "gained"]
    lost = [r["task"] for r in both if r["moved"] == "lost"]
    passed = lambda key: sum(1 for r in both if r[key] == 1.0)  # noqa: E731
    print(f"**{len(both)} tasks under both builds.** Passed: base {passed('base')}, "
          f"candidate {passed('candidate')}.")
    print(f"Gained {len(gained)} ({', '.join(gained) or 'none'}); "
          f"lost {len(lost)} ({', '.join(lost) or 'none'}).")
    for key, what, unit in (("tokens", "tokens with cache reads", "K"), ("minutes", "minutes", "")):
        b = mean(r[f"base_{key}"] for r in both)
        c = mean(r[f"candidate_{key}"] for r in both)
        scale = 1000 if unit else 1
        print(f"Per task, {what}: base {b / scale:.1f}{unit}, candidate {c / scale:.1f}{unit}.")
    alone = [r["task"] for r in rows if r not in both]
    if alone:
        print(f"Under one build only, and left out of the above: {', '.join(alone)}.")
    print("\n| Task | Base | Candidate | Moved | Tokens | Minutes |")
    print("|---|---|---|---|---|---|")
    num = lambda v, f: "" if v is None else format(v, f)  # noqa: E731
    for r in rows:
        tokens = "/".join(num(None if v is None else v / 1000, ".0f") + "K"
                          for v in (r["base_tokens"], r["candidate_tokens"]))
        minutes = "/".join(num(v, ".1f") for v in (r["base_minutes"], r["candidate_minutes"]))
        print(f"| {r['task']} | {num(r['base'], '.2f')} | {num(r['candidate'], '.2f')} "
              f"| {r['moved']} | {tokens} | {minutes} |")


if __name__ == "__main__":
    main()
