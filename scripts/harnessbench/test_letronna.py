"""Tests for the Letronna adapter, against a fake `letronna` and no network.

The adapter imports Harness-Bench, so a checkout's src goes on PYTHONPATH:

    PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=<harness-bench>/src python3 -m pytest -p no:cacheprovider scripts/harnessbench
"""

from __future__ import annotations

import json
import os
import signal
import stat
import sys
import threading
import time
from pathlib import Path

import pytest

pytest.importorskip("harnessbench", reason="needs a Harness-Bench checkout's src on PYTHONPATH")

from harnessbench.models import AdapterRunContext, TaskSpec  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parent))
import letronna  # noqa: E402

# A stand-in for `letronna` that records what it was started with and does
# what the FAKE_* variables say, so each test sets up only its own case.
FAKE = """#!{python}
import json, os, signal, subprocess, sys, time
record = {{"argv": sys.argv[1:], "cwd": os.getcwd(), "env": dict(os.environ), "pid": os.getpid()}}
routes = os.environ.get("HARNESSBENCH_LLM_PROXY_ROUTES", "")
record["routes"] = json.load(open(routes)) if os.path.isfile(routes) else None
with open(os.environ["FAKE_RECORD"], "a") as out:
    out.write(json.dumps(record) + "\\n")
if os.environ.get("FAKE_PROXY_STATUS"):
    with open(os.environ["FAKE_PROXY_LOG"], "a") as log:
        log.write(json.dumps({{"path": "/chat/completions", "status": int(os.environ["FAKE_PROXY_STATUS"])}}) + "\\n")
if os.environ.get("FAKE_SLEEP"):
    # Ignored before the child starts, so the child ignores it too and only
    # the kill can end either.
    if os.environ.get("FAKE_ON_INTERRUPT") == "ignore":
        signal.signal(signal.SIGINT, signal.SIG_IGN)
    # In a process group of its own, as Letronna starts every command.
    child = subprocess.Popen(["sleep", "300"], preexec_fn=lambda: os.setpgid(0, 0))
    with open(os.environ["FAKE_CHILD"], "w") as pid:
        pid.write(str(child.pid))
    # As in Letronna, an interrupt it started with ignored stays ignored.
    if signal.getsignal(signal.SIGINT) is not signal.SIG_IGN:
        def stopped(*_):
            child.kill()
            print(json.dumps({{"answer": "", "status": "stopped"}}), flush=True)
            sys.exit(130)
        signal.signal(signal.SIGINT, stopped)
    time.sleep(float(os.environ["FAKE_SLEEP"]))
print("a line that is not JSON")
print(json.dumps({{"answer": "4", "rounds": 2, "files": ["out/linecount.txt"], "status": "done"}}))
"""


@pytest.fixture
def fake(tmp_path: Path) -> Path:
    path = tmp_path / "fake-letronna"
    path.write_text(FAKE.format(python=sys.executable), encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return path


def context(tmp_path: Path, fake: Path, **env: str) -> AdapterRunContext:
    sandbox = tmp_path / "sandbox"
    workspace = sandbox / "workspace"
    workspace.mkdir(parents=True, exist_ok=True)
    proxy = sandbox / "usage-proxy"
    return AdapterRunContext(
        task=TaskSpec(task_id="001-file", title="t"),
        workspace=workspace,
        sandbox=sandbox,
        prompt="Count the lines.",
        prompt_file=sandbox / "prompt-round1.txt",
        session_id="harnessbench-001-file-1",
        timeout_sec=30,
        env={
            "HARNESSBENCH_LLM_PROXY_URL": "http://127.0.0.1:9",
            "HARNESSBENCH_LLM_PROXY_ROUTES": str(proxy / "routes.json"),
            "FAKE_RECORD": str(tmp_path / "record.jsonl"),
            "FAKE_PROXY_LOG": str(proxy / "requests.jsonl"),
            "FAKE_PROXY_STATUS": "200",
            **env,
        },
        model_id="letronna-test",
        model_config={
            "adapter": "letronna",
            "command": str(fake),
            "model": "test-model",
            "upstream": "https://upstream.example/v1",
        },
        mode="live",
    )


def records(tmp_path: Path) -> list[dict]:
    return [json.loads(line) for line in (tmp_path / "record.jsonl").read_text().splitlines()]


def gone(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return True
    return False


def ended(pid: int) -> bool:
    """Whether the fake, a child of this process, has exited, reaping it if so."""
    try:
        return os.waitpid(pid, os.WNOHANG)[0] == pid
    except ChildProcessError:
        return True


def soon(check, pid: int) -> bool:
    """Whether `check(pid)` holds within five seconds, the time a killed process takes to be reaped."""
    deadline = time.monotonic() + 5
    while not check(pid) and time.monotonic() < deadline:
        time.sleep(0.05)
    return check(pid)


def test_the_adapter_registers_its_proxy_route_before_running(tmp_path: Path, fake: Path) -> None:
    letronna.LetronnaAdapter().run(context(tmp_path, fake))
    routes = records(tmp_path)[0]["routes"]
    assert routes["/letronna/main"] == {
        "framework": "letronna",
        "provider": "main",
        "upstream": "https://upstream.example/v1",
    }


def test_settings_are_written_once_and_left_for_later_rounds(
    tmp_path: Path, fake: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("TEST_PROVIDER_KEY", "sk-test")
    ctx = context(tmp_path, fake)
    ctx.model_config["api_key_env"] = "TEST_PROVIDER_KEY"
    letronna.LetronnaAdapter().run(ctx)
    data = ctx.sandbox / ".letronna"
    settings = json.loads((data / "settings.json").read_text())
    assert settings["gateway"] == "Custom"
    assert settings["endpoint"] == settings["custom_endpoint"] == "http://127.0.0.1:9/letronna/main"
    assert settings["model"] == "test-model"
    assert settings["keys_in"] == "files"
    assert json.loads((data / "keys.json").read_text()) == {"Custom": "sk-test"}
    assert stat.S_IMODE((data / "keys.json").stat().st_mode) == 0o600

    # Letronna saves its own fields into the file as it runs.
    saved = dict(settings, active_id=1)
    (data / "settings.json").write_text(json.dumps(saved))
    letronna.LetronnaAdapter().run(ctx)
    assert json.loads((data / "settings.json").read_text()) == saved


def test_continue_comes_before_print_in_the_command(tmp_path: Path, fake: Path) -> None:
    result = letronna.LetronnaAdapter().run(context(tmp_path, fake))
    argv = records(tmp_path)[0]["argv"]
    assert argv[:3] == ["-c", "-p", "Count the lines."]
    assert argv[3:] == ["--output", "json", "--mode", "bypass", "--model", "test-model", "--max-rounds", "150"]
    assert result.command[1:] == argv


def test_the_round_runs_in_the_sandbox_with_none_of_the_operators_setup(
    tmp_path: Path, fake: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("LETRONNA_VAULT_SERVICE", "letronna-scratch")
    ctx = context(tmp_path, fake)
    letronna.LetronnaAdapter().run(ctx)
    seen = records(tmp_path)[0]
    assert seen["cwd"] == str(ctx.workspace.resolve())
    assert seen["env"]["HOME"] == str(ctx.sandbox)
    assert seen["env"]["AGENT_DATA_DIR"] == str(ctx.sandbox / ".letronna")
    assert seen["env"]["BROWSER"] == "/usr/bin/false"
    assert seen["env"]["LETRONNA_NO_LOGIN_ENV"] == "1"
    assert "LETRONNA_VAULT_SERVICE" not in seen["env"]


def test_the_last_json_line_becomes_metadata(tmp_path: Path, fake: Path) -> None:
    result = letronna.LetronnaAdapter().run(context(tmp_path, fake))
    assert result.ok
    assert result.metadata["returncode"] == 0
    assert result.metadata["timed_out"] is False
    assert result.metadata["result"]["status"] == "done"
    assert result.metadata["result"]["files"] == ["out/linecount.txt"]


def test_an_empty_proxy_log_marks_the_run_invalid(tmp_path: Path, fake: Path) -> None:
    quiet = letronna.LetronnaAdapter().run(context(tmp_path, fake, FAKE_PROXY_STATUS=""))
    assert quiet.metadata["invalid_run"] is True
    assert quiet.metadata["model_calls"] == 0
    # A call the upstream refused reached no model either.
    refused = letronna.LetronnaAdapter().run(context(tmp_path, fake, FAKE_PROXY_STATUS="401"))
    assert refused.metadata["invalid_run"] is True
    answered = letronna.LetronnaAdapter().run(context(tmp_path, fake))
    assert answered.metadata["invalid_run"] is False
    assert answered.metadata["model_calls"] == 1


def test_a_call_the_proxy_logs_just_after_the_round_ends_still_counts(
    tmp_path: Path, fake: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # Before the round, as it ends, and once the proxy has written its line.
    counts = iter([0, 0, 1])
    monkeypatch.setattr(letronna, "answered_calls", lambda _log: next(counts))
    monkeypatch.setattr(letronna, "LOG_SETTLE_SEC", 0.0)
    result = letronna.LetronnaAdapter().run(context(tmp_path, fake))
    assert result.metadata["model_calls"] == 1
    assert result.metadata["invalid_run"] is False


def test_a_timeout_returns_not_ok_and_kills_the_process_group(
    tmp_path: Path, fake: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(letronna, "STOP_GRACE_SEC", 1.0)
    ctx = context(tmp_path, fake, FAKE_SLEEP="60", FAKE_ON_INTERRUPT="ignore", FAKE_CHILD=str(tmp_path / "child"))
    ctx.timeout_sec = 1
    started = time.monotonic()
    result = letronna.LetronnaAdapter().run(ctx)
    assert time.monotonic() - started < 20
    assert not result.ok
    assert result.metadata["timed_out"] is True
    # In a group of its own, so only the sweep of the session reaches it.
    assert soon(gone, int((tmp_path / "child").read_text()))


def test_a_timed_out_run_is_interrupted_first_so_it_can_end_itself(
    tmp_path: Path, fake: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(letronna, "STOP_GRACE_SEC", 10.0)
    ctx = context(tmp_path, fake, FAKE_SLEEP="60", FAKE_CHILD=str(tmp_path / "child"))
    ctx.timeout_sec = 1
    started = time.monotonic()
    result = letronna.LetronnaAdapter().run(ctx)
    # It ended on the interrupt, well before the grace ran out.
    assert time.monotonic() - started < 8
    assert not result.ok
    assert result.metadata["returncode"] == 130
    assert result.metadata["result"]["status"] == "stopped"


def test_a_harness_started_with_ctrl_c_ignored_still_interrupts_a_timed_out_run(
    tmp_path: Path, fake: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(letronna, "STOP_GRACE_SEC", 10.0)
    ctx = context(tmp_path, fake, FAKE_SLEEP="60", FAKE_CHILD=str(tmp_path / "child"))
    ctx.timeout_sec = 1
    # As a script's `&` leaves it for the harness, and for what it starts.
    before = signal.signal(signal.SIGINT, signal.SIG_IGN)
    try:
        started = time.monotonic()
        result = letronna.LetronnaAdapter().run(ctx)
        assert signal.getsignal(signal.SIGINT) is signal.SIG_IGN
    finally:
        signal.signal(signal.SIGINT, before)
    assert time.monotonic() - started < 8
    assert result.metadata["returncode"] == 130
    assert result.metadata["result"]["status"] == "stopped"


def test_a_harness_stopped_by_ctrl_c_stops_the_round_with_it(
    tmp_path: Path, fake: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(letronna, "STOP_GRACE_SEC", 1.0)
    child = tmp_path / "child"
    ctx = context(tmp_path, fake, FAKE_SLEEP="60", FAKE_ON_INTERRUPT="ignore", FAKE_CHILD=str(child))

    def ctrl_c_once_running() -> None:
        deadline = time.monotonic() + 10
        while not child.exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        signal.pthread_kill(threading.main_thread().ident, signal.SIGINT)

    threading.Thread(target=ctrl_c_once_running).start()
    started = time.monotonic()
    with pytest.raises(KeyboardInterrupt):
        letronna.LetronnaAdapter().run(ctx)
    assert time.monotonic() - started < 20
    assert soon(ended, records(tmp_path)[0]["pid"])
    assert soon(gone, int(child.read_text()))


def test_a_key_variable_that_is_not_set_stops_before_running(
    tmp_path: Path, fake: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("TEST_PROVIDER_KEY", raising=False)
    ctx = context(tmp_path, fake)
    ctx.model_config["api_key_env"] = "TEST_PROVIDER_KEY"
    result = letronna.LetronnaAdapter().run(ctx)
    assert not result.ok
    assert "TEST_PROVIDER_KEY" in result.stderr
    assert not (tmp_path / "record.jsonl").exists()
