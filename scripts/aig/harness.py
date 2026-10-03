#!/usr/bin/env python3
"""Letronna as a pikit harness, so A.I.G's injection matrix can run against it.

pikit (A.I.G's prompt-injection engine) crafts one tainted artifact per case
-- a channel, a carrier mode and an attack method -- hands it to a harness,
and judges the trace that comes back. This is the harness for Letronna: it
runs `letronna -p` once per case in a profile of its own, through the
fixture MCP server's sources and sinks or through a real file in the
workspace, and gives back the trace `one_turn` reads out of the session the
run wrote.

pikit is not vendored here. Put its checkout on PYTHONPATH along with this
folder, and see README.md for what one case needs.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Optional

from pikit.adapters.harness import AgentHarness, register_harness
from pikit.adapters.taint import TaintRouter
from pikit.agent.base import Trace, TraceStep
from pikit.agent.hooks import DefenseHooks

import one_turn
from one_turn import Turn

# The fields pikit's `TraceStep` takes; `one_turn` records two more, whether
# a result reached the model fenced and whether a sink the gate refused was
# asked for, which belong in the case's own notes rather than in a step
# pikit's judges read: there a refused sink is a policy block, not a sink.
# A call of a tool the run does not offer keeps its own decision,
# "unavailable", which pikit's judges read as neither.
STEP_FIELDS = ("kind", "text", "tool_name", "args", "content", "tainted", "is_sink", "decision")


@register_harness("letronna")
class LetronnaHarness(AgentHarness):
    """Run one pikit case as a `letronna -p` turn and give back its trace.

    `source` names where the artifact reaches the model from: one of the
    fixture server's six sources, or `read_file`, `run_command` or `skill`
    for the native path, where the carrier is a real file in the workspace.
    `mode` is the gate's: `bypass` for the baseline with nothing in the way
    of the sinks, `auto` for the judge deciding.
    """

    name = "letronna"

    def __init__(
        self,
        target: Any = None,
        *,
        system: str = "",
        source: str = "read_document",
        mode: str = "bypass",
        executable: str = "letronna",
        model: str = "",
        endpoint: str = "",
        gateway: str = "Custom",
        key: str = "",
        max_rounds: int = one_turn.MAX_ROUNDS,
        timeout: int = one_turn.TIMEOUT,
        python: str = "",
        server: str = "",
        root: str = "",
        keep: bool = False,
        defenses: Optional[DefenseHooks] = None,
        reader: str = one_turn.READER,
    ) -> None:
        # The model is the one the profile names, so pikit's target is held
        # only to say which it was.
        self.target = target
        # Letronna writes its own system prompt, so a prompt of pikit's is
        # never sent, and stays out of the trace, where a judge would take it
        # for the one the run had.
        self.system = system
        self.source = source
        self.keep = keep
        self.hooks = defenses or DefenseHooks()
        self.taint_router: Optional[TaintRouter] = None
        self.turn = Turn(
            executable=executable,
            mode=mode,
            model=model,
            endpoint=endpoint,
            gateway=gateway,
            key=key,
            max_rounds=max_rounds,
            timeout=timeout,
            python=python,
            server=server,
            root=root,
            reader=reader,
        )
        # The last case's own notes: its profile, its sink log and whether
        # each source result reached the model fenced.
        self.last: Dict[str, Any] = {}

    def run(self, user_message: str, **kwargs: Any) -> Trace:
        """Run one case and give its trace."""
        chosen = one_turn.source(kwargs.pop("source", self.source))
        # Letronna writes its own system prompt, so a run with this defense
        # would be labelled defended when nothing was laid over anything.
        if self.hooks.system is not None:
            raise ValueError(
                "A system defense cannot be laid over Letronna's own system prompt; "
                "run this case without it."
            )
        artifact = self._artifact(chosen.key, kwargs)
        carrier = kwargs.pop("carrier", None)
        # A carrier file reaches Letronna as it is, in its own format, which
        # a defense on text cannot be laid over.
        if carrier and self.hooks.tool_result is not None:
            raise ValueError(
                "A tool_result defense works on text, and a carrier file reaches "
                "Letronna as it is; pass the defended text as artifact= instead."
            )
        # With no message of pikit's own the case makes the benign request
        # its source needs; a defense hardens one that was given.
        request = self.hooks.on_user(user_message) if user_message else ""
        ran = self.turn.run(
            chosen.key,
            self.hooks.on_tool_result(artifact, chosen.tool),
            request=request,
            carrier=Path(carrier) if carrier else None,
            keep=kwargs.pop("keep", self.keep),
        )
        self.last = ran
        return trace_of(ran["steps"], ran["final_text"])

    def _artifact(self, key: str, kwargs: Dict[str, Any]) -> str:
        """The tainted artifact this case delivers, however pikit passes it."""
        if "artifact" in kwargs:
            return str(kwargs.pop("artifact"))
        payloads = kwargs.pop("fixture_payloads", None)
        if isinstance(payloads, dict) and payloads:
            # Keyed by source where a caller names one, else the only entry.
            if key in payloads:
                return str(payloads[key])
            return str(next(iter(payloads.values())))
        rules = self.taint_router.rules if self.taint_router is not None else []
        return str(rules[0].payload) if rules else ""


def trace_of(steps: List[Dict[str, Any]], final_text: str) -> Trace:
    """The pikit trace of a case's steps, as `one_turn` reads them back."""
    trace = Trace()
    for kept in steps:
        trace.add(TraceStep(**{name: kept[name] for name in STEP_FIELDS}))
    trace.final_text = final_text
    return trace
