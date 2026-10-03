"""Tests for usage-emitter.py, on session lines written here.

With a ClawBench checkout's src on PYTHONPATH, the rows are also summed by
ClawBench's own code; without it that test is skipped:

    PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=<ClawBench>/src python3 -m pytest -p no:cacheprovider scripts/clawbench
"""

from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
EMITTER = HERE / "usage-emitter.py"


def load():
    spec = importlib.util.spec_from_file_location("letronna_usage_emitter", EMITTER)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


emitter = load()


def line(**fields) -> str:
    return json.dumps(fields)


# A session as `letronna -p` writes it: its head, the question, one reply
# that called a tool, the tool's result, and the closing reply.
SESSION = [
    line(type="head", id=1700000000, created=1700000000),
    line(type="meta", title="Order a pizza"),
    line(type="message", role="user", content="Order a pizza"),
    line(
        type="message",
        role="assistant",
        content="",
        model="test-model",
        prompt_tokens=1000,
        cached_tokens=600,
        cache_write_tokens=100,
        tokens=300,
        reasoning_tokens=120,
        cost=0.0123456789,
        at=1700000100,
        tool_calls=[{"id": "c1", "name": "browse", "arguments": "{}"}],
    ),
    line(type="message", role="tool", content="page", tool_call_id="c1", name="browse"),
    line(type="message", role="assistant", content="Done.", prompt_tokens=1500, tokens=20),
]


def test_a_reply_becomes_one_row_that_counts_each_token_once() -> None:
    first = emitter.rows(SESSION, "fallback-model")[0]
    assert first == {
        "type": "usage",
        "source_harness": "letronna",
        "call_id": "letronna:4",
        "timestamp": 1700000100,
        "model": "test-model",
        "input_tokens": 300,
        "output_tokens": 180,
        "cache_read_tokens": 600,
        "cache_write_tokens": 100,
        "reasoning_tokens": 120,
        "total_tokens": 1300,
        "estimated_cost_usd": 0.012346,
        "cost_status": "estimated",
        "pricing_source_url": None,
        "matched_model_id": None,
    }


def test_records_that_made_no_call_are_left_out() -> None:
    lines = [
        *SESSION[:3],
        "not json",
        line(type="message", role="tool", content="x", prompt_tokens=5),
        line(type="message", role="failure", content="The provider said no."),
        # A reply stopped before the provider said what it took.
        line(type="message", role="assistant", content="", interrupted=True),
        line(type="summary", upto=2, text="earlier"),
    ]
    assert emitter.rows(lines, "m") == []


def test_a_reply_with_no_cost_is_priced_as_unavailable() -> None:
    last = emitter.rows(SESSION, "fallback-model")[-1]
    assert last["model"] == "fallback-model"
    assert (last["estimated_cost_usd"], last["cost_status"]) == (None, "price_unavailable")
    assert (last["input_tokens"], last["output_tokens"], last["total_tokens"]) == (1500, 20, 1520)


def test_a_gateway_that_counts_the_cache_beside_the_prompt_is_read_the_same() -> None:
    beside = line(type="message", role="assistant", prompt_tokens=400, cached_tokens=600, tokens=10)
    made = emitter.rows([beside], "m")[0]
    assert (made["input_tokens"], made["cache_read_tokens"], made["total_tokens"]) == (400, 600, 1010)


def test_a_summary_the_reply_waited_for_is_a_call_of_its_own() -> None:
    reply = line(
        type="message",
        role="assistant",
        model="test-model",
        prompt_tokens=200,
        tokens=10,
        summary_usage={"input": 5000, "cached": 1000, "output": 400, "reasoning": 100, "cost": 0.5},
    )
    made = emitter.rows([reply], "m")
    assert [row["call_id"] for row in made] == ["letronna:1", "letronna:1:summary"]
    summary = made[1]
    assert (summary["input_tokens"], summary["cache_read_tokens"]) == (4000, 1000)
    assert (summary["output_tokens"], summary["reasoning_tokens"], summary["total_tokens"]) == (300, 100, 5400)
    assert summary["estimated_cost_usd"] == 0.5


def test_running_it_again_writes_the_same_rows_rather_than_more(tmp_path: Path) -> None:
    session = tmp_path / "agent-messages.jsonl"
    session.write_text("\n".join(SESSION) + "\n")
    out = tmp_path / "usage.jsonl"
    command = [sys.executable, str(EMITTER), "--input", str(session), "--output", str(out), "--model", "m"]
    subprocess.run(command, check=True)
    once = out.read_text()
    subprocess.run(command, check=True)
    assert out.read_text() == once
    assert len(once.splitlines()) == 2


def test_a_missing_session_leaves_an_empty_usage_file(tmp_path: Path) -> None:
    out = tmp_path / "usage.jsonl"
    command = [sys.executable, str(EMITTER), "--input", str(tmp_path / "none.jsonl"), "--output", str(out)]
    subprocess.run(command, check=True)
    assert out.read_text() == ""


def test_clawbench_sums_the_rows_into_the_runs_calls_tokens_and_cost() -> None:
    usage = pytest.importorskip(
        "clawbench.runner.run_support.usage", reason="needs a ClawBench checkout's src on PYTHONPATH"
    )
    summary = usage.summarize_usage_lines(json.dumps(made) for made in emitter.rows(SESSION, "m"))
    assert summary["api_calls"] == 2
    assert summary["total_tokens"] == 1300 + 1520
    assert summary["reasoning_tokens"] == 120
    # One reply has no cost, so the run's total is not known in full.
    assert summary["status"] == "price_unavailable"
    assert summary["estimated_cost_usd"] == 0.012346
