#!/usr/bin/env python3
"""Usage rows for ClawBench from Letronna's session file.

Letronna keeps a session as JSON lines, one record per message, and each
assistant message carries the tokens and cost of the model call that made
it, with a summary's call beside it when the context was compacted.
ClawBench reads one row per call from /data/usage.jsonl, in the shape its
other harnesses' emitters write, and adds them up for the run. The shared
emitter looks for a `usage` object, which Letronna's records do not have,
so this one reads them as they are.

Letronna counts the thinking inside the output and the cache inside the
input, where ClawBench adds each part to the total, so each is taken out of
the one it sits in and every token is counted once.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
from typing import Any

HARNESS = "letronna"


def number(value: Any) -> int:
    """A token count as a whole number, 0 for anything that is not one."""
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return 0
    return max(int(value), 0)


def reply_usage(record: dict[str, Any]) -> dict[str, Any]:
    """What the call that made an assistant message took, in the summary's terms."""
    prompt = number(record.get("prompt_tokens"))
    cached = number(record.get("cached_tokens"))
    written = number(record.get("cache_write_tokens"))
    # OpenAI counts cache hits and writes inside prompt_tokens, and some
    # gateways beside it, as Message::input_tokens in chat.rs allows for.
    total_input = prompt + cached + written if cached + written > prompt else prompt
    return {
        "input": total_input,
        "cached": cached,
        "cache_write": written,
        "output": number(record.get("tokens")),
        "reasoning": number(record.get("reasoning_tokens")),
        "cost": record.get("cost"),
    }


def row(
    call_id: str, usage: dict[str, Any], model: str, at: Any
) -> dict[str, Any] | None:
    """The usage row ClawBench reads for one call, or None when it took no tokens."""
    cached = number(usage.get("cached"))
    written = number(usage.get("cache_write"))
    fresh = max(number(usage.get("input")) - cached - written, 0)
    output = number(usage.get("output"))
    reasoning = min(number(usage.get("reasoning")), output)
    total = fresh + cached + written + output
    if total == 0:
        return None
    cost = usage.get("cost")
    cost = (
        float(cost)
        if isinstance(cost, (int, float)) and not isinstance(cost, bool)
        else None
    )
    return {
        "type": "usage",
        "source_harness": HARNESS,
        "call_id": call_id,
        "timestamp": at or None,
        "model": model,
        "input_tokens": fresh,
        "output_tokens": output - reasoning,
        "cache_read_tokens": cached,
        "cache_write_tokens": written,
        "reasoning_tokens": reasoning,
        "total_tokens": total,
        # Letronna's own figure: billed by OpenRouter, else from the model's
        # listed price.
        "estimated_cost_usd": round(cost, 6) if cost is not None else None,
        "cost_status": "estimated" if cost is not None else "price_unavailable",
        "pricing_source_url": None,
        "matched_model_id": None,
    }


def rows(lines: list[str], default_model: str) -> list[dict[str, Any]]:
    """One row for each model call the session's lines record, in order."""
    out: list[dict[str, Any]] = []
    for line_number, line in enumerate(lines, start=1):
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(record, dict) or record.get("role") != "assistant":
            continue
        # A "paid" record is a reply a rewind took out of the session, still
        # a call that was made.
        if record.get("type") not in ("message", "paid"):
            continue
        model = str(record.get("model") or default_model)
        at = record.get("at")
        made = row(f"{HARNESS}:{line_number}", reply_usage(record), model, at)
        if made:
            out.append(made)
        summary = record.get("summary_usage")
        if isinstance(summary, dict):
            made = row(f"{HARNESS}:{line_number}:summary", summary, model, at)
            if made:
                out.append(made)
    return out


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--input", type=Path, required=True, help="Letronna's session file"
    )
    parser.add_argument("--output", type=Path, default=Path("/data/usage.jsonl"))
    parser.add_argument("--model", default=os.environ.get("MODEL_NAME", ""))
    args = parser.parse_args()
    try:
        lines = args.input.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        lines = []
    args.output.parent.mkdir(parents=True, exist_ok=True)
    # Written whole each time, so running it again counts no call twice.
    with args.output.open("w", encoding="utf-8") as out:
        for made in rows(lines, args.model):
            out.write(
                json.dumps(made, ensure_ascii=False, separators=(",", ":")) + "\n"
            )


if __name__ == "__main__":
    main()
