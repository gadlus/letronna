"""Removes each Terminal-Bench task's image once its trials are done, so a
full run fits a disk that cannot hold all 89 images (49 GB compressed).

    python3 janitor.py TASKS_DIR JOBS_DIR [--every 20] [--once]

A task's image goes once every trial of it under JOBS_DIR has its
result.json and no container is running from it, and again after a rerun
pulls it back. Harbor's own teardown removes only images it built, not
those it pulled. Runs until killed, or with --once sweeps once.

Each sweep also stops the docker compose processes still bringing up the
environment of trials that have ended: when a start times out, Harbor
stops the docker CLI but not the compose plugin under it, which goes on
pulling. A teardown is left to finish, as a cancelled trial can write its
result.json while its `down` still runs.
"""

from __future__ import annotations

import argparse
import os
import re
import signal
import subprocess
import time
import tomllib
from pathlib import Path


def image_of(task: Path) -> str | None:
    try:
        return tomllib.loads((task / "task.toml").read_text())["environment"].get("docker_image")
    except (OSError, KeyError, tomllib.TOMLDecodeError):
        return None


def docker_lines(*args: str) -> set[str]:
    out = subprocess.run(["docker", *args], capture_output=True, text=True, check=False)
    return {line.strip() for line in out.stdout.splitlines() if line.strip()}


def finished(jobs: Path) -> dict[str, bool]:
    """Each task with a trial under `jobs`, and whether all its trials are done."""
    tasks: dict[str, bool] = {}
    for trial in jobs.glob("*/*__*"):
        if trial.is_dir():
            task = trial.name.rsplit("__", 1)[0]
            tasks[task] = tasks.get(task, True) and (trial / "result.json").exists()
    return tasks


def project(trial: str) -> str:
    """The compose project Harbor runs a trial's environment as, named as Harbor names it."""
    name = f"{trial}__env".lower()
    if not re.match(r"[a-z0-9]", name):
        name = "0" + name
    return re.sub(r"[^a-z0-9_-]", "-", name)


def compose_project(argv: list[str]) -> str | None:
    """The project a docker compose command works on, or the compose plugin under it."""
    if "compose" not in argv:
        return None
    for i, arg in enumerate(argv):
        if arg == "--project-name" and i + 1 < len(argv):
            return argv[i + 1]
        if arg.startswith("--project-name="):
            return arg.split("=", 1)[1]
    return None


def ended(jobs: Path) -> dict[str, str]:
    """The compose project of each trial under `jobs` that has its result, and the trial."""
    return {project(r.parent.name): r.parent.name for r in jobs.glob("*/*__*/result.json")}


# The compose commands that bring an environment up, as against those that
# work in it or take it down.
STARTING = {"up", "pull", "build", "create"}


def stop_composes(jobs: Path) -> list[str]:
    """Stops the compose processes still starting ended trials' environments, and names those trials."""
    projects = ended(jobs)
    stopped = set()
    for cmdline in Path("/proc").glob("[0-9]*/cmdline"):
        try:
            argv = cmdline.read_bytes().decode(errors="replace").split("\0")
        except OSError:
            continue
        trial = projects.get(compose_project(argv))
        if trial and STARTING & set(argv):
            try:
                os.kill(int(cmdline.parent.name), signal.SIGTERM)
            except OSError:
                continue
            stopped.add(trial)
    return sorted(stopped)


def sweep(images: dict[str, str | None], jobs: Path) -> list[str]:
    busy = docker_lines("ps", "--format", "{{.Image}}")
    have = docker_lines("images", "--format", "{{.Repository}}:{{.Tag}}")
    gone = []
    for task, done in finished(jobs).items():
        image = images.get(task)
        if not done or not image:
            continue
        names = {image, f"docker.io/{image}"}
        if names & busy or not names & have:
            continue
        ran = subprocess.run(["docker", "rmi", "-f", image], capture_output=True, check=False)
        if ran.returncode == 0:
            gone.append(image)
    return gone


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("tasks")
    p.add_argument("jobs")
    p.add_argument("--every", type=float, default=20)
    p.add_argument("--once", action="store_true")
    a = p.parse_args()
    images = {d.name: image_of(d) for d in Path(a.tasks).iterdir() if (d / "task.toml").exists()}
    while True:
        for trial in stop_composes(Path(a.jobs)):
            print(f"{time.strftime('%H:%M:%S')} stopped compose of ended trial {trial}", flush=True)
        for image in sweep(images, Path(a.jobs)):
            print(f"{time.strftime('%H:%M:%S')} removed {image}", flush=True)
        if a.once:
            return
        time.sleep(a.every)


if __name__ == "__main__":
    main()
