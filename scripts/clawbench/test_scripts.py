"""Tests for setup-letronna.sh and run-letronna.sh, with a fake `letronna`.

The run script is pointed at a folder standing in for the container's /
through LETRONNA_CLAWBENCH_ROOT, and the browser at an address nothing
answers, which it takes as ready since it is a socket's. Each run test takes
about 25 seconds: the watchdog looks every five, and the recording's grace
after the run is fifteen.

    PYTHONDONTWRITEBYTECODE=1 python3 -m pytest -p no:cacheprovider scripts/clawbench
"""

from __future__ import annotations

import json
import os
import shutil
import stat
import subprocess
import sys
import time
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
SETUP = HERE / "setup-letronna.sh"
RUN = HERE / "run-letronna.sh"
EMITTER = HERE / "usage-emitter.py"

# A stand-in for `letronna -p` that records how it was started, writes a
# session as Letronna does, and then exits with FAKE_EXIT or waits for an
# interrupt, answering it as Letronna does.
FAKE = """#!{python}
import json, os, signal, sys, time
data = os.environ["AGENT_DATA_DIR"]
record = {{
    "argv": sys.argv[1:],
    "cwd": os.getcwd(),
    "env": {{k: os.environ.get(k) for k in ("PATH", "AGENT_DATA_DIR", "LETRONNA_BROWSER_URL", "LETRONNA_NO_LOGIN_ENV")}},
    "settings": json.load(open(os.path.join(data, "settings.json"))),
}}
sessions = os.path.join(data, "sessions")
os.makedirs(sessions, exist_ok=True)
with open(os.path.join(sessions, "1700000000.jsonl"), "w") as session:
    session.write(json.dumps({{"type": "head", "id": 1700000000, "created": 1700000000}}) + "\\n")
    session.write(json.dumps({{"type": "message", "role": "user", "content": sys.argv[2]}}) + "\\n")
    session.write(json.dumps({{"type": "message", "role": "assistant", "content": "",
                               "prompt_tokens": 900, "tokens": 40, "model": "test-model"}}) + "\\n")
def save():
    with open(os.environ["FAKE_RECORD"], "w") as out:
        json.dump(record, out)
if os.environ.get("FAKE_EXIT"):
    save()
    print(json.dumps({{"status": "failed", "error": "The provider said no."}}))
    sys.exit(int(os.environ["FAKE_EXIT"]))
# As in Letronna, an interrupt it started with ignored stays ignored.
if signal.getsignal(signal.SIGINT) is not signal.SIG_IGN:
    def stopped(*_):
        record["interrupted"] = True
        save()
        print(json.dumps({{"status": "stopped"}}), flush=True)
        sys.exit(130)
    signal.signal(signal.SIGINT, stopped)
save()
time.sleep(120)
"""


@pytest.fixture
def root(tmp_path: Path) -> Path:
    """A folder laid out as the image is, with the fake on PATH beside it."""
    root = tmp_path / "root"
    (root / "data").mkdir(parents=True)
    (root / "my-info").mkdir()
    (root / "my-info" / "email_credentials.json").write_text('{"email": "user_a@example.com"}')
    shutil.copy(SETUP, root / "setup-letronna.sh")
    shutil.copy(EMITTER, root / "usage-emitter.py")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake = bin_dir / "letronna"
    fake.write_text(FAKE.format(python=sys.executable), encoding="utf-8")
    fake.chmod(fake.stat().st_mode | stat.S_IXUSR)
    return root


def model_env(**extra: str) -> dict[str, str]:
    """The variables the test driver sets for an OpenAI-compatible model."""
    env = {k: v for k, v in os.environ.items() if not k.startswith(("API_", "LETRONNA_"))}
    env.update(
        {
            "BASE_URL": "http://127.0.0.1:9/v1/",
            "MODEL_NAME": "test-model",
            "API_TYPE": "openai-completions",
            "API_KEYS": '["sk-first", "sk-second"]',
        }
    )
    env.update(extra)
    return env


def setup(data: Path, **extra: str) -> subprocess.CompletedProcess[str]:
    env = model_env(AGENT_DATA_DIR=str(data), **extra)
    return subprocess.run(["bash", str(SETUP)], env=env, capture_output=True, text=True)


def run(root: Path, **extra: str) -> subprocess.Popen[str]:
    env = model_env(
        LETRONNA_CLAWBENCH_ROOT=str(root),
        PATH=f"{root.parent / 'bin'}{os.pathsep}{os.environ['PATH']}",
        INSTRUCTION="Order one pizza",
        TIME_LIMIT_S="60",
        THINKING_LEVEL="off",
        CLAWBENCH_BROWSER_CDP_URL="ws://127.0.0.1:9/devtools/browser/test",
        FAKE_RECORD=str(root.parent / "record.json"),
        **extra,
    )
    return subprocess.Popen(
        ["bash", str(RUN)],
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        start_new_session=True,
    )


def finish(proc: subprocess.Popen[str]) -> str:
    try:
        output, _ = proc.communicate(timeout=90)
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, 9)
        raise
    assert proc.returncode == 0, output
    return output


def started(root: Path) -> dict:
    """The fake's record, once it has started."""
    path = root.parent / "record.json"
    deadline = time.monotonic() + 30
    while not path.exists() and time.monotonic() < deadline:
        time.sleep(0.1)
    return json.loads(path.read_text())


def test_setup_points_letronna_straight_at_an_openai_compatible_server(tmp_path: Path) -> None:
    data = tmp_path / ".letronna"
    done = setup(data)
    assert done.returncode == 0, done.stdout + done.stderr
    settings = json.loads((data / "settings.json").read_text())
    assert settings["gateway"] == "Custom"
    assert settings["endpoint"] == settings["custom_endpoint"] == "http://127.0.0.1:9/v1"
    assert settings["model"] == "test-model"
    assert settings["keys_in"] == "files"
    assert settings["browser_shared"] is True
    assert settings["memory_review"] is False
    keys = data / "keys.json"
    assert json.loads(keys.read_text()) == {"Custom": "sk-first"}
    assert stat.S_IMODE(keys.stat().st_mode) == 0o600
    assert not (data / "litellm-config.yaml").exists()


def test_setup_sends_anthropic_and_gemini_models_through_litellm(tmp_path: Path) -> None:
    anthropic = tmp_path / "anthropic"
    done = setup(anthropic, API_TYPE="anthropic-messages", BASE_URL="https://api.anthropic.com")
    assert done.returncode == 0, done.stdout + done.stderr
    proxy = json.loads((anthropic / "litellm-config.yaml").read_text())
    assert proxy["model_list"] == [
        {"model_name": "test-model", "litellm_params": {"model": "anthropic/test-model", "api_key": "sk-first"}}
    ]
    assert json.loads((anthropic / "settings.json").read_text())["endpoint"] == "http://127.0.0.1:4000"
    # LiteLLM holds the key, so Letronna is given none.
    assert not (anthropic / "keys.json").exists()

    gemini = tmp_path / "gemini"
    done = setup(gemini, API_TYPE="google-generative-ai", BASE_URL="https://gemini.example/v1")
    assert done.returncode == 0, done.stdout + done.stderr
    params = json.loads((gemini / "litellm-config.yaml").read_text())["model_list"][0]["litellm_params"]
    assert params["model"] == "gemini/test-model"
    assert params["api_base"] == "https://gemini.example/v1"


def test_setup_stops_with_no_key_and_writes_nothing(tmp_path: Path) -> None:
    data = tmp_path / ".letronna"
    done = setup(data, API_KEYS="[]")
    assert done.returncode != 0
    assert "no API key" in done.stdout + done.stderr
    assert not (data / "settings.json").exists()


def test_a_matched_request_stops_letronna_with_an_interrupt_and_keeps_its_transcript(root: Path) -> None:
    proc = run(root)
    started(root)
    (root / "data" / ".stop-requested").touch()
    finish(proc)
    record = json.loads((root.parent / "record.json").read_text())

    # Heard only when it was not started with Ctrl-C ignored.
    assert record.get("interrupted") is True
    assert record["argv"] == [
        "-p",
        "Order one pizza",
        "--output",
        "json",
        "--yes",
        "--tools",
        "browse,use_page,read_file,list_dir",
        "--max-rounds",
        "150",
        "--effort",
        "none",
    ]
    assert Path(record["cwd"]).resolve() == (root / "root" / "workspace").resolve()
    assert record["env"] == {
        "PATH": str(root / "tmp" / "safe-bin"),
        "AGENT_DATA_DIR": str(root / "root" / ".letronna"),
        "LETRONNA_BROWSER_URL": "ws://127.0.0.1:9/devtools/browser/test",
        "LETRONNA_NO_LOGIN_ENV": "1",
    }
    assert record["settings"]["browser_shared"] is True
    assert (root / "root" / "workspace" / "my-info" / "email_credentials.json").exists()

    data = root / "data"
    assert (data / ".stop-reason").read_text().strip() == "eval_matched"
    assert not (data / ".stop-requested").exists()
    session = root / "root" / ".letronna" / "sessions" / "1700000000.jsonl"
    assert (data / "agent-messages.jsonl").read_text() == session.read_text()
    usage = [json.loads(line) for line in (data / "usage.jsonl").read_text().splitlines()]
    assert [(row["input_tokens"], row["output_tokens"]) for row in usage] == [(900, 40)]
    assert json.loads((data / "letronna-result.json").read_text())["status"] == "stopped"


def test_a_run_that_fails_by_itself_says_so_and_keeps_its_transcript(root: Path) -> None:
    proc = run(root, FAKE_EXIT="1")
    finish(proc)
    data = root / "data"
    assert (data / ".stop-reason").read_text().strip() == "letronna_failed"
    assert '"role": "user"' in (data / "agent-messages.jsonl").read_text()
    assert json.loads((data / "letronna-result.json").read_text())["status"] == "failed"


def test_the_fork_patch_carries_these_very_harness_files() -> None:
    patch = (HERE / "fork.patch").read_text()
    for name in ("Dockerfile.letronna", "setup-letronna.sh", "run-letronna.sh", "usage-emitter.py"):
        header = f"+++ b/src/clawbench/runtime/harnesses/letronna/{name}\n"
        assert header in patch, f"fork.patch does not add {name}"
        hunk = patch.split(header, 1)[1].split("\ndiff --git ", 1)[0]
        added = [line[1:] for line in hunk.splitlines() if line.startswith("+")]
        assert "\n".join(added) + "\n" == (HERE / name).read_text(), f"fork.patch has an old {name}"
