"""git-status: the session's branch in the status bar, what changed in a
panel that refreshes after every turn, and a tool the model can ask.

Python's own library only: it runs `git` in the session's project folder.
"""

import subprocess

import letronna

# The project the last turn worked in, for the panel's Refresh.
last = {"folder": None}


def git(folder, *args):
    done = subprocess.run(
        ["git", *args], cwd=folder, capture_output=True, text=True, timeout=10
    )
    if done.returncode != 0:
        raise RuntimeError(done.stderr.strip() or "git failed")
    return done.stdout


def status_of(folder):
    # Unlike rev-parse, this answers in a repository with no commits yet.
    branch = git(folder, "branch", "--show-current").strip() or "detached"
    changed = [line for line in git(folder, "status", "--porcelain").splitlines() if line.strip()]
    return branch, changed


def show(folder):
    """Puts the branch in the status bar and the changes in the panel."""
    if not folder:
        letronna.ui.panel({"type": "text", "text": "This session has no project folder.", "muted": True})
        return
    try:
        branch, changed = status_of(folder)
    except Exception as error:
        letronna.status("")
        letronna.ui.panel({"type": "text", "text": str(error), "tone": "warning"})
        return
    letronna.status(f"{branch} · {len(changed)} changed" if changed else f"{branch} ✓")
    files = [{"type": "text", "text": line, "mono": True, "size": "small"} for line in changed[:200]]
    letronna.ui.panel({
        "type": "column",
        "children": [
            {"type": "text", "text": branch, "size": "large"},
            {"type": "text", "text": folder, "muted": True, "size": "small"},
            {"type": "divider"},
            *(files or [{"type": "text", "text": "Nothing changed.", "tone": "success"}]),
            {"type": "row", "children": [{"type": "button", "id": "refresh", "label": "Refresh"}]},
        ],
    })


@letronna.on("turn.end")
def turn_ended(turn):
    last["folder"] = turn.get("workspace")
    show(last["folder"])


@letronna.on("prompt.submit")
def with_each_message(sent):
    """When switched on, tells the model where the project stands, so it
    need not ask git itself."""
    folder = sent.get("workspace")
    if not letronna.settings.get("tell_model") or not folder:
        return None
    try:
        branch, changed = status_of(folder)
    except Exception:
        return None
    if not changed:
        return f"The project is on branch {branch}, with nothing changed."
    return f"The project is on branch {branch}, with {len(changed)} changed:\n" + "\n".join(changed[:30])


@letronna.on("panel.open")
def opened(_):
    show(last["folder"])


@letronna.ui.on_action("refresh")
def refresh(_):
    show(last["folder"])


@letronna.tool("summary")
def summary(args, ctx):
    if not ctx.workspace:
        return "This session has no project folder."
    branch, changed = status_of(ctx.workspace)
    if not changed:
        return f"On {branch}; nothing changed."
    return f"On {branch}; {len(changed)} changed:\n" + "\n".join(changed)


@letronna.action("changed")
def changed(text, ctx):
    return summary({}, ctx)
