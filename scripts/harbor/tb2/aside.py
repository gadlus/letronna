"""Sets aside the trials of a Terminal-Bench run that the infrastructure cost, so
Harbor runs them again.

    python3 aside.py JOB_DIR [JOB_DIR ...] [--meter METER ...] [--to ASIDE_DIR] [--max 2]

Prints `job/trial<TAB>why` for each trial whose outcome says nothing of the agent:

- environment: it failed before the agent started;
- cancelled: Harbor was stopped while it ran;
- verifier: no test ran, as the tests' own install failed on the network;
- gateway: Letronna's run failed with calls refused (402, 429 or 5xx in the
  relay's meters) in its last 30 s. A refusal that names a content filter
  stands, as the same run would meet it again.

With --to each moves to ASIDE_DIR/<job>/<trial>, and Harbor, run again on
the job, runs every trial it is missing. A task with --max trials already
set aside there is left, and its failure counts; trials a stop cancelled
are not counted, as they say nothing of the task, and always move. Run it
only while no Harbor runs on the job.
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import sys
from datetime import datetime
from pathlib import Path

from triage import result_line

NETWORK = (
    "failed to download",
    "network timeout",
    "could not resolve host",
    "temporary failure in name resolution",
    "connection reset",
)
FILTER = re.compile(r"content[_ ]filter|content policy|moderation|flagged|safety", re.IGNORECASE)
WINDOW = 30


def when(text) -> float | None:
    try:
        return datetime.fromisoformat(text).timestamp()
    except (TypeError, ValueError):
        return None


def refusals(meters: list[Path]) -> list[tuple[float, str]]:
    """When each call the upstream refused ended, and what it said. 402, 429 and
    5xx say nothing of the request; 400 and 403 may well."""
    found = []
    for meter in meters:
        try:
            lines = meter.read_text(errors="replace").splitlines()
        except FileNotFoundError:
            continue
        for line in lines:
            try:
                row = json.loads(line)
            except ValueError:
                continue
            if not isinstance(row, dict) or row.get("method") != "POST":
                continue
            status = row.get("status")
            if isinstance(status, int) and (status in (402, 429) or status >= 500):
                ended = float(row.get("t") or 0) + float(row.get("s") or 0)
                found.append((ended, str(row.get("error") or "")))
    return found


def cancelled(trial: Path) -> bool:
    try:
        result = json.loads((trial / "result.json").read_text())
    except (OSError, ValueError):
        return False
    return (result.get("exception_info") or {}).get("exception_type") == "CancelledError"


def why(trial: Path, refused: list[tuple[float, str]]) -> str | None:
    """Why `trial`'s outcome says nothing of the agent, or None when it stands."""
    try:
        result = json.loads((trial / "result.json").read_text())
    except (OSError, ValueError):
        return None
    if ((result.get("verifier_result") or {}).get("rewards") or {}).get("reward"):
        return None
    error = (result.get("exception_info") or {}).get("exception_type") or ""
    agent = result.get("agent_execution") or {}
    if error == "CancelledError":
        return "cancelled"
    if error and not agent.get("started_at"):
        return "environment"
    verifier = trial / "verifier"
    if not (verifier / "ctrf.json").exists():
        out = " ".join(p.read_text(errors="replace") for p in verifier.glob("test-*.txt")).lower()
        if "test session starts" not in out and (
            error == "VerifierTimeoutError" or any(n in out for n in NETWORK)
        ):
            return "verifier"
    ran = result_line(trial)
    finished = when(agent.get("finished_at"))
    if ran.get("status") != "failed" or finished is None or FILTER.search(str(ran.get("error") or "")):
        return None
    near = [said for ended, said in refused if finished - WINDOW <= ended <= finished]
    if near and not any(FILTER.search(said) for said in near):
        return "gateway"
    return None


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("jobs", nargs="+")
    p.add_argument("--meter", action="append", default=[], help="a relay meter; give each one")
    p.add_argument("--to", help="move each trial here, under its job's name")
    p.add_argument("--max", type=int, default=2, help="trials of one task set aside at most")
    a = p.parse_args()
    refused = refusals([Path(m) for m in a.meter])
    for job in map(Path, a.jobs):
        for trial in sorted(job.glob("*__*")):
            reason = why(trial, refused) if trial.is_dir() else None
            if reason is None:
                continue
            if a.to:
                into = Path(a.to) / job.name
                task = trial.name.rsplit("__", 1)[0]
                before = [
                    d for d in into.glob("*__*") if d.name.rsplit("__", 1)[0] == task and not cancelled(d)
                ]
                if reason != "cancelled" and len(before) >= a.max:
                    kept = f"kept: {task} has been run again {len(before)} times"
                    print(f"{job.name}/{trial.name}\t{reason}, {kept}", file=sys.stderr)
                    continue
                into.mkdir(parents=True, exist_ok=True)
                shutil.move(trial, into / trial.name)
            print(f"{job.name}/{trial.name}\t{reason}", flush=True)


if __name__ == "__main__":
    main()
