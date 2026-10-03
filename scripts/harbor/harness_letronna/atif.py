"""Letronna's session files as ATIF, the trajectory format Harbor shows and keeps.

A run writes its whole conversation to `<data>/sessions/<id>.jsonl`, one
record a line, and each helper it starts to `<data>/agents/<id>/<n>.jsonl`,
with `<n>.json` beside it saying what the helper is. `--output stream-json`
carries only progress counts and the result, so the trajectory is built from
these files after the run. Only the stdlib is used, so the conversion runs
and is tested without Harbor installed.

Each reply becomes an agent step with its tool calls, and the results that
answer them become that step's observation. A notice -- something that
happened meanwhile, sent to the model as the user's turn -- is a system step,
not the user's. Helpers are embedded as subagent trajectories, but without a
reference from the call that started each one: their files do not record
which call that was. A fork starts with a copy of its parent's messages,
which is marked as copied context and counted only where it was made.
"""

import json
import sys
from datetime import datetime, timezone
from pathlib import Path

SCHEMA = "ATIF-v1.7"
AGENT = "letronna"
NOTES = (
    "final_metrics count every model call the run paid for: the helpers' "
    "(subagent_trajectories), summaries of turns the context no longer held, "
    "and replies taken back. A fork's copy of its parent's replies counts once, "
    "where they were made. total_steps counts this trajectory's own steps."
)


def convert(session: Path, version: str, model: str | None = None) -> dict | None:
    """The ATIF trajectory of the session file at `session` with its helpers, or None when
    it holds no messages."""
    run = replay(session)
    steps = steps_of(run["messages"])
    if not steps:
        return None
    session_id = str(run["id"] or session.stem)
    helpers, spent = helpers_of(session, session_id, version)
    calls = run["paid"] + spent + calls_in(run["messages"])
    for _, helper_calls in helpers:
        calls += helper_calls
    trajectory = {
        "schema_version": SCHEMA,
        "session_id": session_id,
        "agent": {"name": AGENT, "version": version, "model_name": model or run["model"] or None},
        "steps": steps,
        "notes": NOTES,
        "final_metrics": totals(calls, len(steps)),
    }
    if helpers:
        trajectory["subagent_trajectories"] = [helper for helper, _ in helpers]
    return drop_none(trajectory)


def replay(path: Path) -> dict:
    """The session id, model, messages and taken-back replies the file at `path` holds,
    read as Letronna reads it."""
    run = {"id": None, "model": None, "messages": [], "paid": []}
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            # A line cut short by a crash; Letronna passes over it too.
            continue
        if not isinstance(record, dict):
            continue
        kind = record.get("type")
        if kind == "head":
            run["id"] = record.get("id")
        elif kind == "meta":
            run["model"] = record.get("model") or run["model"]
        elif kind == "message":
            run["messages"].append(record)
        elif kind == "rewind":
            del run["messages"][record.get("to", 0) :]
        elif kind == "paid":
            run["paid"] += calls_of(record)
        elif kind == "summary" and isinstance(record.get("call"), dict):
            # The call that made a summary the user asked for, which no reply
            # carries; only the file it was made in has it.
            run["paid"].append(saved(record["call"].get("usage") or {}))
    return run


def steps_of(messages: list[dict], copied: int = 0) -> list[dict]:
    """The ATIF steps for `messages`, the first `copied` of them marked as copied context."""
    steps: list[dict] = []
    # Models that send no ids get call_0 and so on in every reply, so a
    # result belongs to the latest reply that made its id.
    callers: dict[str, dict] = {}
    for at, message in enumerate(messages):
        role = message.get("role")
        content = message.get("content") or ""
        if role == "tool":
            call_id = message.get("tool_call_id") or ""
            result = {"source_call_id": call_id, "content": content}
            if message.get("is_error"):
                result["extra"] = {"is_error": True}
            caller = callers.get(call_id)
            if caller is not None:
                caller.setdefault("observation", {"results": []})["results"].append(result)
                continue
            # A result whose call is not here can name it only in `extra`,
            # since a source_call_id has to be one of its own step's calls.
            del result["source_call_id"]
            result["extra"] = {
                **result.get("extra", {}),
                "tool_call_id": call_id,
                "name": message.get("name"),
            }
            step = {"source": "system", "message": "", "observation": {"results": [result]}}
        elif role == "assistant":
            step = reply(message)
            for call in step.get("tool_calls", []):
                callers[call["tool_call_id"]] = step
        elif role == "user":
            step = {"source": "system" if message.get("notice") else "user", "message": content}
        else:
            # A failure the run met, kept for the window to explain.
            step = {"source": "system", "message": content, "extra": {"role": role}}
        if at < copied:
            step["is_copied_context"] = True
            # Paid for where it was first made: Harbor sums every step's
            # metrics into the trial's model usage.
            step.pop("metrics", None)
        steps.append(step)
    for number, step in enumerate(steps, start=1):
        step["step_id"] = number
    return steps


def reply(message: dict) -> dict:
    """The agent step for one reply: what it said and thought, its calls and what it cost."""
    step = {"source": "agent", "message": message.get("content") or "", "llm_call_count": 1}
    if message.get("model"):
        step["model_name"] = message["model"]
    if message.get("at"):
        step["timestamp"] = datetime.fromtimestamp(message["at"], tz=timezone.utc).isoformat()
    if message.get("reasoning"):
        step["reasoning_content"] = message["reasoning"]
    calls = [tool_call(call) for call in message.get("tool_calls") or []]
    if calls:
        step["tool_calls"] = calls
    spent = usage(message)
    if spent["input"] or spent["output"]:
        metrics = {
            "prompt_tokens": spent["input"],
            "completion_tokens": spent["output"],
            "cached_tokens": spent["cached"],
            "cost_usd": spent["cost"],
        }
        extra = {
            "cache_write_tokens": message.get("cache_write_tokens"),
            "reasoning_tokens": message.get("reasoning_tokens"),
            "cost_estimated": message.get("cost_estimated"),
            "summary": message.get("summary_usage"),
        }
        metrics["extra"] = {key: value for key, value in extra.items() if value} or None
        step["metrics"] = metrics
    if message.get("interrupted"):
        step["extra"] = {"interrupted": True}
    return step


def tool_call(call: dict) -> dict:
    """One call a reply made, its arguments as the object the model wrote."""
    raw = call.get("arguments") or ""
    try:
        arguments = json.loads(raw) if raw.strip() else {}
    except json.JSONDecodeError:
        arguments = None
    out = {"tool_call_id": call.get("id") or "", "function_name": call.get("name") or ""}
    if isinstance(arguments, dict):
        out["arguments"] = arguments
    else:
        # ATIF wants an object, and the model's text is kept as it came.
        out["arguments"] = {}
        out["extra"] = {"raw_arguments": raw}
    return out


def usage(message: dict) -> dict:
    """The tokens and cost of the model call that made one reply, as Letronna counts them."""
    prompt = message.get("prompt_tokens") or 0
    cached = message.get("cached_tokens") or 0
    # Some gateways count cache hits and writes beside prompt_tokens rather
    # than inside it, which shows as more cached than sent.
    cache = cached + (message.get("cache_write_tokens") or 0)
    return {
        "input": prompt + cache if cache > prompt else prompt,
        "cached": cached,
        "output": message.get("tokens") or 0,
        "cost": message.get("cost"),
        "estimated": bool(message.get("cost_estimated")),
    }


def calls_in(messages: list[dict]) -> list[dict]:
    """The model calls behind `messages`: each reply's, and each summary made on the way to one."""
    calls = []
    for message in messages:
        if message.get("role") == "assistant":
            calls += calls_of(message)
    return calls


def calls_of(record: dict) -> list[dict]:
    """The model calls behind one reply or paid record, as Letronna counts them: its own when
    it is a reply, and the summary made on the way to it."""
    calls = [usage(record)] if record.get("role") == "assistant" else []
    summary = record.get("summary_usage")
    if isinstance(summary, dict):
        calls.append(saved(summary))
    return calls


def saved(counts: dict) -> dict:
    """The call a usage object Letronna saved describes, such as a summary's."""
    return {
        "input": counts.get("input") or 0,
        "cached": counts.get("cached") or 0,
        "output": counts.get("output") or 0,
        "cost": counts.get("cost"),
        "estimated": bool(counts.get("estimated")),
    }


def helpers_of(
    session: Path, session_id: str, version: str
) -> tuple[list[tuple[dict, list[dict]]], list[dict]]:
    """The session's helpers, each as its trajectory and its model calls, and the calls
    kept apart from any helper."""
    folder = session.parent.parent / "agents" / session.stem
    helpers, spent = [], []
    for path in sorted(
        folder.glob("*.jsonl"), key=lambda path: (not path.stem.isdigit(), path.stem.zfill(20))
    ):
        run = replay(path)
        # calls.jsonl keeps only what side calls, such as a goal check, took.
        if not path.stem.isdigit():
            spent += calls_in(run["messages"])
            continue
        about = read_json(path.with_suffix(".json"))
        # A fork starts with its parent's messages, replies and what they
        # cost included, then its task: `from` is where its own start.
        start = about.get("from") or 0
        # The task closes what the fork starts with, but is its own.
        steps = steps_of(run["messages"], copied=max(start - 1, 0))
        if not steps:
            continue
        calls = run["paid"] + calls_in(run["messages"][start:])
        known = ("name", "type", "description", "depth", "fork")
        helper = {
            "schema_version": SCHEMA,
            "session_id": session_id,
            "trajectory_id": f"helper-{path.stem}",
            "agent": {
                "name": AGENT,
                "version": version,
                "model_name": about.get("model") or run["model"] or None,
                "extra": {key: about[key] for key in known if key in about},
            },
            "steps": steps,
            "final_metrics": totals(calls, len(steps)),
        }
        helpers.append((helper, calls))
    return helpers, spent


def totals(calls: list[dict], steps: int) -> dict:
    """ATIF's final metrics over `calls`, with a cost only when every call that spent
    tokens had one."""
    spent = [call for call in calls if call["input"] or call["output"]]
    priced = all(call["cost"] is not None for call in spent)
    metrics = {
        "total_prompt_tokens": sum(call["input"] for call in spent),
        "total_completion_tokens": sum(call["output"] for call in spent),
        "total_cached_tokens": sum(call["cached"] for call in spent),
        "total_cost_usd": sum(call["cost"] for call in spent) if spent and priced else None,
        "total_steps": steps,
    }
    if any(call["estimated"] for call in spent):
        # Letronna prices a call from the model's listed price when the
        # provider does not bill it in the reply.
        metrics["extra"] = {"cost_estimated": True}
    return metrics


def read_json(path: Path) -> dict:
    """The JSON object in the file at `path`, or an empty one when it is missing or is not one."""
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}
    return value if isinstance(value, dict) else {}


def drop_none(value):
    """`value` without the None fields, which Harbor writes out as missing rather than null."""
    if isinstance(value, dict):
        return {key: drop_none(item) for key, item in value.items() if item is not None}
    if isinstance(value, list):
        return [drop_none(item) for item in value]
    return value


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("usage: python3 -m harness_letronna.atif <data>/sessions/<id>.jsonl")
    print(json.dumps(convert(Path(sys.argv[1]), version="unknown"), indent=2))
