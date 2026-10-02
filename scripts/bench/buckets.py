"""Failure buckets for Harness-Bench runs, decided from the result file alone.

The buckets are the five in the Harness-Bench paper's failure table
(contract/format, tool/recovery, evidence/grounding, artifact commitment,
state/continuation), plus infra for runs the environment spoiled and oracle
defect for checks known to be wrong. A check is labelled by the words in its
id and label, and by a detail saying a file is missing, not by a judge, so the
same result always lands in the same bucket and two arms can be compared; a
check no rule recognises stays unlabelled rather than being guessed at.

The checks known to be wrong come from bench/harness-bench/known_issues.json,
checked by hand against one Harness-Bench commit. It is JSON, though YAML
would read better, so the report runs on a Python without PyYAML.
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass
from pathlib import Path

CONTRACT = "contract/format"
TOOL = "tool/recovery"
EVIDENCE = "evidence/grounding"
ARTIFACT = "artifact commitment"
STATE = "state/continuation"
INFRA = "infra"
ORACLE_DEFECT = "oracle defect"
UNLABELLED = "unlabelled"

# The report's table lists the buckets in this order, and run_bucket breaks a
# tie in lost weight by it.
BUCKETS = [INFRA, ARTIFACT, CONTRACT, TOOL, STATE, EVIDENCE, ORACLE_DEFECT, UNLABELLED]

# First match wins, so a failed parse of the evidence file is a format
# failure, and a retry seen at the checkpoint endpoint is a recovery one.
RULES = [
    (
        CONTRACT,
        set("parse parses parseable readable format header schema keys columns shape".split()),
    ),
    (TOOL, set("retry retries recovery recover visited failover fallback".split())),
    (STATE, set("resume resumed checkpoint continuation interrupted progress".split())),
    (
        EVIDENCE,
        set(
            "evidence grounded grounding source sources coverage covered "
            "citation citations cite cites quote quotes".split()
        ),
    ),
]

# The reason Harness-Bench gives when process grading was turned off on
# purpose; any other skipped judge means the judge could not run.
SKIP_REASON = "HARNESSBENCH_SKIP_PROCESS_GRADE"

KNOWN_ISSUES = Path(__file__).resolve().parents[2] / "bench" / "harness-bench" / "known_issues.json"


@dataclass
class KnownIssues:
    """The oracle defects checked by hand against one Harness-Bench commit."""

    commit: str
    checks: set[tuple[str, str]]
    ceilings: dict[str, float]
    not_for_tuning: set[str]


def load_known_issues(path: Path) -> KnownIssues:
    """Reads the known-issues ledger, or stops saying what is wrong with it."""
    try:
        ledger = json.loads(path.read_text(encoding="utf-8"))
        tasks = ledger["tasks"]
        known = KnownIssues(
            commit=str(ledger["commit"]),
            checks={
                (task_id, check)
                for task_id, entry in tasks.items()
                for issue in entry["issues"]
                for check in issue["checks"]
            },
            ceilings={task_id: float(entry["ceiling"]) for task_id, entry in tasks.items()},
            not_for_tuning={
                task_id for task_id, entry in tasks.items() if entry["exclude_from_tuning"]
            },
        )
    except (OSError, ValueError, KeyError, TypeError, AttributeError) as err:
        raise SystemExit(f"{path} cannot be read as a known-issues ledger: {err!r}") from err
    # A ceiling divides the outcome, and no oracle scores above 1.
    wrong = sorted(task_id for task_id, ceiling in known.ceilings.items() if not 0 < ceiling <= 1)
    if wrong:
        raise SystemExit(f"{path} gives a ceiling outside 0-1 to: {', '.join(wrong)}")
    return known


def check_id(check: dict) -> str:
    """The name an oracle check goes by."""
    return str(check.get("id") or check.get("name") or "")


def check_bucket(task_id: str, check: dict, oracle_defects: set[tuple[str, str]]) -> str:
    """Names the bucket a failed oracle check belongs in."""
    name = check_id(check)
    if (task_id, name) in oracle_defects:
        return ORACLE_DEFECT
    label = str(check.get("label") or "").strip().lower()
    detail = str(check.get("detail") or "").strip().lower()
    if (
        name == "missing"
        or name.endswith(("_exists", "_missing"))
        or label.endswith((" exists", " missing"))
        or detail == "missing"
        or detail.endswith(" missing")
        or "no such file" in detail
    ):
        return ARTIFACT
    words = set(re.split(r"[^a-z0-9]+", f"{name} {label}".lower()))
    for bucket, triggers in RULES:
        if words & triggers:
            return bucket
    return UNLABELLED


def run_bucket(
    result: dict | None, oracle_defects: set[tuple[str, str]], ceilings: dict[str, float]
) -> str:
    """Names a failed run's bucket: infra, a missing output, or the bucket that lost most weight."""
    if result is None or infra_reason(result) is not None:
        return INFRA
    task_id = str(result.get("task_id") or "")
    lost: dict[str, float] = {}
    for check in (result.get("oracle_result") or {}).get("checks") or []:
        if check.get("pass"):
            continue
        bucket = check_bucket(task_id, check, oracle_defects)
        lost[bucket] = lost.get(bucket, 0.0) + float(check.get("weight", 1.0) or 0.0)
    # A missing output fails every check that reads it, so it explains the
    # rest whatever their weight.
    if ARTIFACT in lost:
        return ARTIFACT
    fair = {bucket: weight for bucket, weight in lost.items() if bucket != ORACLE_DEFECT}
    if not fair:
        return ORACLE_DEFECT if lost or reached_ceiling(result, ceilings) else UNLABELLED
    return max(fair, key=lambda bucket: (fair[bucket], -BUCKETS.index(bucket)))


def reached_ceiling(result: dict, ceilings: dict[str, float]) -> bool:
    """Whether a run held the security gate and scored all the ledger says its task allows."""
    # The ceiling can sit inside a check that passes, as in 042 and 064, so
    # such a run lost only to the oracle with no failed check to say so.
    task_id = str(result.get("task_id") or "")
    scoring = result.get("scoring") or {}
    outcome = float(scoring.get("outcome_score") or 0.0)
    return (
        task_id in ceilings
        and outcome >= ceilings[task_id]
        and scoring.get("security_score") != 0.0
    )


def infra_reason(result: dict) -> str | None:
    """Says how the environment rather than the agent spoiled a run, or None for a fair run."""
    metadata = [(result.get("adapter_result") or {}).get("metadata") or {}]
    rounds = result.get("adapter_results") or []
    metadata += [(round_ or {}).get("metadata") or {} for round_ in rounds]
    if any(m.get("invalid_run") for m in metadata):
        return "the adapter marked the run invalid"
    scoring = result.get("scoring") or {}
    if scoring.get("error"):
        return f"scoring failed: {scoring['error']}"
    if scoring.get("proxy_trace_error"):
        return f"no proxy trace: {scoring['proxy_trace_error']}"
    rubric = scoring.get("rubric") or {}
    if rubric.get("parse_error"):
        return "the judge's reply did not parse"
    if rubric.get("skipped") and rubric.get("reason") != SKIP_REASON:
        return f"the judge did not run: {rubric.get('reason')}"
    sandbox = result.get("sandbox")
    status = last_model_status(Path(sandbox) / "usage-proxy" / "requests.jsonl") if sandbox else 0
    # Letronna retries a 5xx itself, so a run whose last call still got one
    # gave up on the provider.
    if status >= 500:
        return f"the last model call got HTTP {status}"
    return None


def last_model_status(proxy_log: Path) -> int:
    """The HTTP status of the last model call the bench's proxy logged, or 0 for none."""
    try:
        lines = proxy_log.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        return 0
    for line in reversed(lines):
        if not line.strip():
            continue
        try:
            return int(json.loads(line).get("status") or 0)
        except (ValueError, AttributeError):
            return 0
    return 0
