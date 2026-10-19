"""Removes each Terminal-Bench task's image once its trials are done, so a
full run fits a disk that cannot hold all 89 images (49 GB compressed).

    python3 janitor.py TASKS_DIR JOBS_DIR [--every 20] [--once]

A task's image goes once every trial of it under JOBS_DIR has its
result.json and no container is running from it. Harbor's own teardown
removes only images it built, not those it pulled. Runs until killed, or
with --once sweeps once.
"""

from __future__ import annotations

import argparse
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


def sweep(images: dict[str, str | None], jobs: Path, removed: set[str]) -> list[str]:
    busy = docker_lines("ps", "--format", "{{.Image}}")
    have = docker_lines("images", "--format", "{{.Repository}}:{{.Tag}}")
    gone = []
    for task, done in finished(jobs).items():
        image = images.get(task)
        if not done or not image or image in removed:
            continue
        names = {image, f"docker.io/{image}"}
        if names & busy or not names & have:
            continue
        ran = subprocess.run(["docker", "rmi", "-f", image], capture_output=True, check=False)
        if ran.returncode == 0:
            removed.add(image)
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
    removed: set[str] = set()
    while True:
        for image in sweep(images, Path(a.jobs), removed):
            print(f"{time.strftime('%H:%M:%S')} removed {image}", flush=True)
        if a.once:
            return
        time.sleep(a.every)


if __name__ == "__main__":
    main()
