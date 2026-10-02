"""Tests for validate.py, over result files shaped as Harness-Bench writes them.

    PYTHONDONTWRITEBYTECODE=1 python3 -m pytest -p no:cacheprovider scripts/harnessbench/test_validate.py
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import validate  # noqa: E402


def judged(**changes: object) -> dict:
    """A result whose trace was judged, with `changes` laid over it."""
    result = {
        "task_id": "001-file",
        "model_id": "letronna-local",
        "oracle_result": {"outcome_score": 1.0, "outcome_llm_weight": 0.0},
        "scoring": {
            "proxy_trace_error": None,
            "rubric": {
                "skipped": False,
                "parse_error": False,
                "scores": {"tool_use_appropriate": 0.9, "consistency": 0.9, "robustness": 0.9},
                "security_score": 1.0,
            },
            "process_score": 0.9,
            "combined_score": 0.9,
        },
        "adapter_results": [{"ok": True, "metadata": {"invalid_run": False}}],
    }
    result.update(changes)
    return result


def test_a_run_with_a_judged_trace_is_valid() -> None:
    assert validate.reasons(judged()) == []


def test_a_skipped_judge_marks_the_run_invalid() -> None:
    result = judged()
    result["scoring"]["rubric"] = {"skipped": True, "reason": "No RUBRIC_API_KEY"}
    assert validate.reasons(result) == ["the process judge was skipped: No RUBRIC_API_KEY"]


def test_a_judge_reply_that_is_not_json_marks_the_run_invalid() -> None:
    result = judged()
    result["scoring"]["rubric"] = {"skipped": False, "parse_error": True}
    assert validate.reasons(result) == ["the process judge's reply was not JSON"]


def test_a_missing_proxy_trace_marks_the_run_invalid_once() -> None:
    result = judged()
    result["scoring"]["proxy_trace_error"] = "empty responses/"
    result["scoring"]["rubric"] = {"skipped": True, "reason": "no proxy trace: empty responses/"}
    assert validate.reasons(result) == ["no proxy trace to judge: empty responses/"]


def test_a_judge_reply_without_its_scores_marks_the_run_invalid() -> None:
    result = judged()
    result["scoring"]["rubric"] = {"skipped": False, "parse_error": False, "scores": {}}
    result["scoring"]["process_score"] = None
    assert validate.reasons(result) == [
        "the process judge gave no process scores, so process counted as 1",
        "the process judge gave no security_gate, so security counted as 1",
    ]


def test_a_round_that_reached_no_model_marks_the_run_invalid() -> None:
    result = judged(adapter_results=[{"metadata": {"invalid_run": False}}, {"metadata": {"invalid_run": True}}])
    assert validate.reasons(result) == ["round 2 reached no model through the usage proxy"]


def test_an_outcome_that_should_blend_a_judged_quality_but_has_none_is_invalid() -> None:
    result = judged(oracle_result={"outcome_score": 1.0, "outcome_llm_weight": 0.9})
    assert validate.reasons(result) == ["the outcome judge gave no quality, so the outcome is the oracle's alone"]
    result["oracle_result"]["quality"] = 0.8
    assert validate.reasons(result) == []


def test_each_result_file_gets_its_verdict_and_the_exit_says_whether_any_is_invalid(tmp_path: Path, capsys) -> None:
    folder = tmp_path / "letronna-local" / "test-model"
    folder.mkdir(parents=True)
    (folder / "001-file.json").write_text(json.dumps(judged()))
    skipped = judged(task_id="002-exec")
    skipped["scoring"]["rubric"] = {"skipped": True, "reason": "HARNESSBENCH_SKIP_PROCESS_GRADE"}
    (folder / "002-exec.json").write_text(json.dumps(skipped))
    (folder / "notes.json").write_text("[1, 2]")

    assert validate.main([str(tmp_path)]) == 1
    good = json.loads((folder / "001-file.json").read_text())
    bad = json.loads((folder / "002-exec.json").read_text())
    assert good["validity"] == {"valid": True, "reasons": []}
    assert bad["validity"]["valid"] is False
    said = capsys.readouterr().out
    assert (
        "invalid letronna-local/test-model/002-exec.json: the process judge was skipped: HARNESSBENCH_SKIP_PROCESS_GRADE"
        in said
    )
    assert "2 runs, 1 valid\nletronna-local: mean combined_score 0.9000 over 1 valid runs\n" in said

    (folder / "002-exec.json").unlink()
    assert validate.main([str(tmp_path)]) == 0
