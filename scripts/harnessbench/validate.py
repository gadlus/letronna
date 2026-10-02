"""Marks the Harness-Bench runs whose scores cannot be trusted, after a suite.

The harness scores a run whatever happened to its grading. With no proxy
trace, or a judge that was skipped for want of a key, answered with
something that is not JSON, or left out its process scores or security
gate, process or security quietly defaults to 1, so the combined score
reads higher than a judged run would. The judge runs only after the
adapter has returned, so the adapter cannot see this; this pass reads
each results/<harness>/<model>/<task>.json afterwards, writes a
`validity` verdict into it, lists the runs to leave out by their path
under the folder, and gives each harness entry's mean over the rest.

    python3 validate.py data/results/letronna-local
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any


def reasons(result: dict[str, Any]) -> list[str]:
    """Why a run's scores cannot be trusted, or nothing for a run that can be."""
    why: list[str] = []
    scoring = result.get("scoring") or {}
    rubric = scoring.get("rubric")
    if scoring.get("error"):
        why.append(f"scoring failed: {scoring['error']}")
    elif scoring.get("proxy_trace_error"):
        why.append(f"no proxy trace to judge: {scoring['proxy_trace_error']}")
    elif not isinstance(rubric, dict) or rubric.get("skipped"):
        reason = rubric.get("reason") if isinstance(rubric, dict) else None
        why.append(f"the process judge was skipped: {reason}" if reason else "the process judge was skipped")
    elif rubric.get("parse_error"):
        why.append("the process judge's reply was not JSON")
    else:
        if scoring.get("process_score") is None:
            why.append("the process judge gave no process scores, so process counted as 1")
        if not isinstance(rubric.get("security_score"), (int, float)):
            why.append("the process judge gave no security_gate, so security counted as 1")
    oracle = result.get("oracle_result") or {}
    weight = oracle.get("outcome_llm_weight")
    if isinstance(weight, (int, float)) and weight > 0 and not isinstance(oracle.get("quality"), (int, float)):
        why.append("the outcome judge gave no quality, so the outcome is the oracle's alone")
    for number, item in enumerate(result.get("adapter_results") or [], start=1):
        if (item.get("metadata") or {}).get("invalid_run"):
            why.append(f"round {number} reached no model through the usage proxy")
    return why


def main(argv: list[str]) -> int:
    """Marks every result under the folder named in `argv` and says which are invalid, exiting 1 if any is."""
    if len(argv) != 1:
        print("usage: validate.py <results folder>", file=sys.stderr)
        return 2
    folder = Path(argv[0])
    runs, invalid = 0, 0
    scores: dict[str, list[float]] = {}
    for path in sorted(folder.rglob("*.json")):
        try:
            result = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        if not isinstance(result, dict) or "scoring" not in result:
            continue
        runs += 1
        why = reasons(result)
        result["validity"] = {"valid": not why, "reasons": why}
        path.write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8")
        if why:
            invalid += 1
            print(f"invalid {path.relative_to(folder)}: {'; '.join(why)}")
            continue
        score = (result.get("scoring") or {}).get("combined_score")
        if isinstance(score, (int, float)):
            scores.setdefault(str(result.get("model_id")), []).append(float(score))
    print(f"{runs} runs, {runs - invalid} valid")
    for model_id, valid in sorted(scores.items()):
        print(f"{model_id}: mean combined_score {sum(valid) / len(valid):.4f} over {len(valid)} valid runs")
    return 1 if invalid else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
