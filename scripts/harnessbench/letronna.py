"""Harness-Bench adapter for Letronna's terminal agent, `letronna -p`.

Each call is one benchmark round. The task's sandbox gets a Letronna data
folder of its own, seeded on the first round with settings that send every
model call through the harness's usage proxy, since without a trace the
process judge never runs and process and security default to 1. Later
rounds find those settings and the session already there, and `-c` carries
the conversation on: with no session yet, `-c` starts one, so it is passed
every time.

The adapter owns the task's timeout. Letronna is given a deadline
TURN_MARGIN_SEC short of it, so it is told to write its outputs and ends
itself with its result line in time; past the timeout, the adapter
interrupts it as Ctrl-C would, which is how Letronna ends its commands,
browser and MCP servers, and kills everything left in its session if that
takes too long. A round that ran out of time still returns, so the oracle
scores what reached out/.

This file lives in Letronna's repo as scripts/harnessbench/letronna.py, and
fork.patch there puts a copy in Harness-Bench as
src/harnessbench/adapters/letronna.py.
"""

from __future__ import annotations

import json
import os
import signal
import subprocess
import time
from pathlib import Path
from typing import Any

from harnessbench.adapters.base import BaseAdapter
from harnessbench.models import AdapterRunContext, AdapterRunResult
from harnessbench.usage_proxy import register_routes

# The usage proxy's path prefix for Letronna's calls; the proxy forwards
# what follows it to the configured upstream.
ROUTE = "/letronna/main"

# Tool rounds a message gets. Letronna's own default of 25 stops long tasks
# well short of their timeouts, which are 180 to 3600 seconds.
MAX_ROUNDS = 150

# Seconds of the task's timeout kept back from Letronna's own deadline, as
# OpenHuman's benchmark rig keeps them from a harness's turn: at the
# deadline Letronna stops with its result line, before the interrupt. A
# timeout barely over this keeps at least half of itself.
TURN_MARGIN_SEC = 120

# Seconds a reply may stay quiet before Letronna gives up on it. The usage
# proxy passes a reply on only once it is whole, so a reply reads as silent
# until then, and Letronna's own 300 fails any that takes longer; this is
# how long the proxy itself waits on its upstream.
READ_TIMEOUT_SEC = 1200

# Seconds Letronna has after the interrupt before its group is killed. It
# gives its commands two seconds to end (END_WAIT in crates/cli/src/main.rs),
# so ten leaves room on a loaded machine.
STOP_GRACE_SEC = 10.0

# Seconds to look at the proxy's log again when it shows no answered call.
# The proxy writes a call's line only after sending the answer, so the last
# line can land just after Letronna exits; half a second covers the write.
LOG_SETTLE_SEC = 0.5


class LetronnaAdapter(BaseAdapter):
    name = "letronna"

    def run(self, ctx: AdapterRunContext) -> AdapterRunResult:
        """Runs one round of the task with `letronna -p` and says how it went."""
        cfg = ctx.model_config
        model = str(cfg.get("model") or "").strip()
        upstream = str(cfg.get("upstream") or "").strip()
        if not model or not upstream:
            return AdapterRunResult(
                ok=False,
                stderr="The letronna harness entry needs `model`, and `upstream`: the provider's "
                "OpenAI-compatible base URL, such as https://api.openai.com/v1.",
            )
        proxy_url = str(ctx.env.get("HARNESSBENCH_LLM_PROXY_URL") or "")
        routes = str(ctx.env.get("HARNESSBENCH_LLM_PROXY_ROUTES") or "")
        if not proxy_url or not routes:
            return AdapterRunResult(
                ok=False,
                stderr="The usage proxy is not running, so this round would leave no trace to judge.",
            )
        key_env = str(cfg.get("api_key_env") or "").strip()
        key = os.environ.get(key_env, "") if key_env else ""
        if key_env and not key:
            return AdapterRunResult(ok=False, stderr=f"`api_key_env` names {key_env}, which is not set.")

        register_routes(
            Path(routes),
            {ROUTE: {"framework": "letronna", "provider": "main", "upstream": upstream}},
        )
        data = ctx.sandbox / ".letronna"
        seed(data, proxy_url.rstrip("/") + ROUTE, model, key)

        cmd = [
            str(cfg.get("command") or "letronna"),
            # Before -p: straight after it, -c would be taken for the question.
            "-c",
            "-p",
            ctx.prompt,
            "--output",
            "json",
            # bypass is --yes; "auto" lets Letronna's judge decide instead.
            "--mode",
            str(cfg.get("permission_mode") or "bypass"),
            "--model",
            model,
            "--max-rounds",
            str(int(cfg.get("max_rounds") or MAX_ROUNDS)),
            *[str(arg) for arg in cfg.get("extra_args") or []],
        ]
        env = os.environ.copy()
        env.update(ctx.env)
        env.update(
            {
                # Neither the operator's data folder nor their home is read.
                "HOME": str(ctx.sandbox),
                "AGENT_DATA_DIR": str(data),
                # The agent's PATH and python are the harness's, not those a
                # login shell's rc files would give.
                "LETRONNA_NO_LOGIN_ENV": "1",
                # Nothing may open a sign-in page or any other in a browser.
                "BROWSER": "/usr/bin/false",
                "NO_COLOR": "1",
                "LETRONNA_DEADLINE": str(deadline(ctx.timeout_sec)),
            }
        )
        env.setdefault("LETRONNA_READ_TIMEOUT", str(READ_TIMEOUT_SEC))
        # A debug build files keys in the Keychain under this name.
        env.pop("LETRONNA_VAULT_SERVICE", None)

        log = ctx.sandbox / "usage-proxy" / "requests.jsonl"
        answered_before = answered_calls(log)
        returncode, stdout, stderr, timed_out = run_owned(cmd, ctx.workspace, env, ctx.timeout_sec)
        result = last_json(stdout)
        # Stopped at its deadline ("deadline"; "stopped" before #594), the round
        # ran out of time, and the harness runs no round after it either.
        timed_out = timed_out or (result or {}).get("status") in ("deadline", "stopped")
        answered = answered_calls(log) - answered_before
        if answered == 0:
            time.sleep(LOG_SETTLE_SEC)
            answered = answered_calls(log) - answered_before
        metadata: dict[str, Any] = {
            "returncode": returncode,
            "timed_out": timed_out,
            "result": result,
            "model_calls": answered,
            "invalid_run": answered == 0,
        }
        if answered == 0:
            metadata["invalid_reason"] = "no model call was answered through the usage proxy this round"
        return AdapterRunResult(
            ok=returncode == 0 and not timed_out,
            command=cmd,
            stdout=stdout,
            stderr=stderr,
            metadata=metadata,
        )


def deadline(timeout: float) -> int:
    """The whole seconds a round of `timeout` gives Letronna: TURN_MARGIN_SEC fewer, but at least half, and at least 1."""
    return max(1, int(max(timeout - TURN_MARGIN_SEC, timeout / 2)))


def seed(data: Path, endpoint: str, model: str, key: str) -> None:
    """Writes Letronna's settings, and its key when there is one, unless an earlier round did."""
    settings = data / "settings.json"
    if settings.exists():
        return
    data.mkdir(parents=True, exist_ok=True)
    # The key goes first, since a later round that finds the settings
    # writes nothing.
    if key:
        keys = data / "keys.json"
        keys.write_text(json.dumps({"Custom": key}), encoding="utf-8")
        keys.chmod(0o600)
    settings.write_text(
        json.dumps(
            {
                "endpoint": endpoint,
                "custom_endpoint": endpoint,
                "gateway": "Custom",
                "model": model,
                # Never the Keychain or Credential Manager.
                "keys_in": "files",
                # Each review is a model call the task did not ask for.
                "memory_review": False,
                "manual_updates": True,
                # Nobody is watching, and a bell must not reach stdout.
                "notifications": False,
            },
            indent=2,
        ),
        encoding="utf-8",
    )


def run_owned(cmd: list[str], cwd: Path, env: dict[str, str], timeout: float) -> tuple[int, str, str, bool]:
    """Runs `cmd` in a session of its own, stopped at `timeout`: its exit code, output, and whether it timed out."""
    # Letronna leaves Ctrl-C alone when it starts with it ignored, as it is
    # under a harness started with `&` from a script, and the interrupt that
    # ends a timed-out round would then do nothing. A handler, unlike an
    # ignored signal, goes back to the default at exec. Harness-Bench runs
    # rounds on its main thread, the only one that may set a handler.
    ignored = signal.getsignal(signal.SIGINT) is signal.SIG_IGN
    if ignored:
        signal.signal(signal.SIGINT, signal.default_int_handler)
    try:
        proc = subprocess.Popen(
            cmd,
            cwd=str(cwd),
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            errors="replace",
            start_new_session=True,
        )
    finally:
        if ignored:
            signal.signal(signal.SIGINT, signal.SIG_IGN)
    try:
        stdout, stderr = proc.communicate(timeout=timeout)
        return proc.returncode, stdout, stderr, False
    except subprocess.TimeoutExpired:
        pass
    except BaseException:
        # The harness's own Ctrl-C never reaches a session of its own, so
        # Letronna would carry on unattended.
        stop(proc)
        raise
    stop(proc)
    # Every call gives all the output since the start, none of it lost.
    stdout, stderr = proc.communicate()
    return proc.returncode, stdout, stderr, True


def stop(proc: subprocess.Popen[str]) -> None:
    """Interrupts `proc` as Ctrl-C would, then kills everything left in its session once the grace is up."""
    signal_group(proc.pid, signal.SIGINT)
    try:
        proc.communicate(timeout=STOP_GRACE_SEC)
    except subprocess.TimeoutExpired:
        pass
    finally:
        # Even once Letronna has gone, a command it started may not have, and
        # it would hold the pipes open and run on into the next task. Its
        # commands, jobs, MCP servers and browser each lead a process group
        # of their own, out of reach of the group kill, but stay in its
        # session.
        signal_group(proc.pid, signal.SIGKILL)
        kill_session(proc.pid)


def signal_group(pgid: int, sig: signal.Signals) -> None:
    """Sends `sig` to process group `pgid`, which may have no one left in it."""
    try:
        os.killpg(pgid, sig)
    except ProcessLookupError:
        pass


def kill_session(sid: int) -> None:
    """Kills every process in session `sid`, saying nothing when `ps` cannot list them."""
    try:
        listed = subprocess.run(["ps", "-A", "-o", "pid="], capture_output=True, text=True, check=False).stdout
    except OSError:
        return
    for word in listed.split():
        try:
            pid = int(word)
            if os.getsid(pid) == sid:
                os.kill(pid, signal.SIGKILL)
        except (ValueError, OSError):
            continue


def last_json(stdout: str) -> dict[str, Any] | None:
    """The last line of `stdout` that is a JSON object, which is how `--output json` ends a run."""
    for line in reversed(stdout.splitlines()):
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict):
            return value
    return None


def answered_calls(log: Path) -> int:
    """How many chat completions the usage proxy's log records an upstream answering."""
    if not log.is_file():
        return 0
    count = 0
    for line in log.read_text(encoding="utf-8", errors="replace").splitlines():
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            continue
        status = row.get("status")
        if str(row.get("path", "")).endswith("/chat/completions") and isinstance(status, int) and 200 <= status < 300:
            count += 1
    return count
