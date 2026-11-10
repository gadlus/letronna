"""Why each trial of a Terminal-Bench run lost points, from its folders.

    python3 triage.py JOB_DIR [JOB_DIR ...] [--all] [--signs OUT.csv]

For every trial that did not pass (every trial with --all): Harbor's error,
how Letronna's run ended (its result line's status, rounds and seconds),
the tests the verifier failed, the start of Letronna's answer, and the signs
of Letronna's own handling in its sessions: how often it was sent back for
stopping short, had a reply stopped for thinking, a call refused or arrive
empty, a command moved to the background, or was stopped going round in
circles. --signs writes those counts for every trial, passed or not.

Terminal-Bench's tasks and solutions are public. A trial that searched the
web for its own task by name, or fetched a page about the benchmark, is
marked "looked up", passed or not, as its result may rest on that.
"""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path


def result_line(trial: Path) -> dict:
    """Letronna's result line, the last line of its stream that has one."""
    log = trial / "agent" / "letronna.jsonl"
    if not log.exists():
        return {}
    for line in reversed(log.read_text(errors="replace").splitlines()):
        if '"type":"result"' in line:
            try:
                return json.loads(line)
            except ValueError:
                return {}
    return {}


# Where the benchmark, its tasks or their solutions are published.
BENCHMARK = (
    "terminal-bench",
    "terminal_bench",
    "tbench",
    "harborframework",
    "marginlab",
    "spylab",
    "laude-institute",
)


def looked_up(trial: Path, task: str) -> list[str]:
    """The searches and fetches of `trial` that named its task or the benchmark."""
    found = []
    for session in sorted((trial / "agent" / "letronna" / "sessions").glob("*.jsonl")):
        for line in session.read_text(errors="replace").splitlines():
            try:
                record = json.loads(line)
            except ValueError:
                continue
            for call in record.get("tool_calls") or []:
                function = call.get("function") or {}
                name = function.get("name") or call.get("name")
                args = str(function.get("arguments") or call.get("arguments") or "")
                if name not in ("search_web", "web_fetch") and "http" not in args:
                    continue
                if names_it(name or "", args.lower(), task.lower()):
                    found.append(f"{name} {args[:160]}")
    return found


def names_it(tool: str, said: str, task: str) -> bool:
    """Whether a call that reaches the web names the benchmark, or this task as one: a
    task's name can be a tool's too, as pypi-server's is, so only in a URL or beside
    words asking for a task, its tests or its solution."""
    if any(mark in said for mark in BENCHMARK):
        return True
    if re.search(r"https?://[^\s\"']*" + re.escape(task), said):
        return True
    return tool == "search_web" and task in said and bool(re.search(r"\b(task|tests?|solution)\b", said))


# What Letronna's own handling left in a trial's sessions, by what it is
# called below. Each used to be found by reading the session through; counted,
# a run's failures sort themselves into those worth reading for it.
SIGNS = (
    "sent back for stopping short",
    "calls written as text",
    "replies stopped for thinking",
    "calls refused by the gate",
    "calls with empty arguments",
    "commands moved to the background",
    "stopped going round in circles",
)
# How a command moved to the background says so, ahead of what it printed:
# the same words after that are the command's own.
MOVED = "so it carries on in the background as job #"
SO_FAR = " Output so far:\n"
# Tools that do nothing without arguments, so a call of one with none is the
# arguments lost, not a call that needed none.
NEEDS_ARGUMENTS = ("read_file", "write_file", "edit_file", "run_command", "search_web", "web_fetch")


def signs(trial: Path) -> dict[str, int]:
    """How often each of `SIGNS` shows in `trial`'s sessions."""
    found = dict.fromkeys(SIGNS, 0)
    for session in sorted((trial / "agent" / "letronna" / "sessions").glob("*.jsonl")):
        for line in session.read_text(errors="replace").splitlines():
            try:
                record = json.loads(line)
            except ValueError:
                continue
            if not isinstance(record, dict):
                continue
            said = str(record.get("content") or "")
            role = record.get("role")
            if role == "user" and record.get("notice"):
                if said == "Continue now." or said.startswith("Your reply called no tool"):
                    found["sent back for stopping short"] += 1
                elif said.startswith("Your reply wrote a tool call as text"):
                    found["calls written as text"] += 1
                elif "without writing anything" in said:
                    found["replies stopped for thinking"] += 1
            elif role == "tool":
                if said.startswith("Refused:"):
                    found["calls refused by the gate"] += 1
                elif (
                    said.startswith("The call's arguments arrived empty")
                    and record.get("name") not in NEEDS_ARGUMENTS
                ):
                    # One of those tools is counted below, at its call.
                    found["calls with empty arguments"] += 1
                elif MOVED in said.partition(SO_FAR)[0] and SO_FAR in said:
                    found["commands moved to the background"] += 1
                if "Stopped going round in circles" in said:
                    found["stopped going round in circles"] += 1
            for call in record.get("tool_calls") or []:
                function = call.get("function") or {}
                name = function.get("name") or call.get("name")
                args = str(function.get("arguments") or call.get("arguments") or "").strip()
                # Counted at the call, so a build from before the result
                # said so is counted the same.
                if name in NEEDS_ARGUMENTS and args in ("", "{}"):
                    found["calls with empty arguments"] += 1
    return found


def failed_tests(trial: Path) -> list[str]:
    ctrf = trial / "verifier" / "ctrf.json"
    try:
        tests = json.loads(ctrf.read_text())["results"]["tests"]
    except (OSError, ValueError, KeyError, TypeError):
        return []
    return [t.get("name", "?").split("::")[-1] for t in tests if t.get("status") != "passed"]


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("jobs", nargs="+")
    p.add_argument("--all", action="store_true")
    p.add_argument("--signs", help="a CSV of each trial's signs of Letronna's own handling")
    a = p.parse_args()
    counted = []
    for job in a.jobs:
        for path in sorted(Path(job).glob("*__*/result.json")):
            trial = path.parent
            data = json.loads(path.read_text())
            reward = ((data.get("verifier_result") or {}).get("rewards") or {}).get("reward")
            lookups = looked_up(trial, str(data.get("task_name") or ""))
            seen = signs(trial)
            counted.append({"task": data.get("task_name"), "trial": trial.name, "reward": reward, **seen})
            if reward == 1.0 and not a.all and not lookups:
                continue
            error = (data.get("exception_info") or {}).get("exception_type") or ""
            ran = result_line(trial)
            failed = failed_tests(trial)
            answer = " ".join(str(ran.get("answer") or ran.get("error") or "").split())
            print(
                f"{data.get('task_name')}: reward {reward}"
                + (f", {error}" if error else "")
                + f"; letronna {ran.get('status', '-')} after {ran.get('rounds', '-')} rounds,"
                f" {ran.get('seconds', '-')} s"
            )
            for lookup in lookups:
                print(f"  looked up: {lookup}")
            if failed:
                print(f"  failed: {', '.join(failed[:8])}" + (" ..." if len(failed) > 8 else ""))
            if answer:
                print(f"  said: {answer[:240]}")
            if any(seen.values()):
                print("  signs: " + ", ".join(f"{n} {what}" for what, n in seen.items() if n))
    if a.signs:
        import csv

        with open(a.signs, "w", newline="") as f:
            writer = csv.DictWriter(f, fieldnames=["task", "trial", "reward", *SIGNS])
            writer.writeheader()
            writer.writerows(counted)


if __name__ == "__main__":
    main()
