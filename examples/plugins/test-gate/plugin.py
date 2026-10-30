"""test-gate: when the model says it is done, run the project's tests, and
send it back with what failed until they pass (Letronna allows three sends
back a turn).

Set the command on the Plugins page, or in the terminal with
`/plugins set test-gate command cargo test`. Python's own library only.
"""

import hashlib
import subprocess

import letronna

# Each project's state when its tests last passed, to skip a run that
# would only say so again.
passed = {}

# Less than the five minutes Letronna gives a plugin to answer turn.stop.
TIME = 280
# The lines of the tests' output the model gets: the end, where the
# failures and the summary are.
LINES = 60


def state(folder):
    """What the project's files are now, as git sees them; None when it is
    not a git repository, which never matches."""
    try:
        status = subprocess.run(
            ["git", "status", "--porcelain"], cwd=folder, capture_output=True, timeout=10
        )
        diff = subprocess.run(["git", "diff"], cwd=folder, capture_output=True, timeout=10)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if status.returncode != 0:
        return None
    return hashlib.sha256(status.stdout + diff.stdout).hexdigest()


@letronna.on("turn.stop")
def check(turn):
    command = (letronna.settings.get("command") or "").strip()
    folder = turn.get("workspace")
    if not command or not folder:
        return None
    now = state(folder)
    if letronna.settings.get("only_after_changes") and now is not None and passed.get(folder) == now:
        return None
    letronna.status("testing…")
    try:
        done = subprocess.run(
            command, shell=True, cwd=folder, capture_output=True, text=True, timeout=TIME
        )
    except subprocess.TimeoutExpired:
        letronna.status("tests timed out")
        return f"`{command}` ran past {TIME} s and was stopped. Make the change smaller, or the tests faster, then finish."
    if done.returncode == 0:
        passed[folder] = now
        letronna.status("tests pass")
        return None
    letronna.status("tests fail")
    output = (done.stdout + done.stderr).strip().splitlines()[-LINES:]
    return (
        f"`{command}` failed (exit {done.returncode}). Fix what it reports, then finish.\n"
        + "\n".join(output)
    )
