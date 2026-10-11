"""Scores saved Harness-Bench results again with another copy of the bench's graders.

    rescore.py BENCH_DIR SRC_DIR DST_DIR

Every result JSON under SRC_DIR (a run's results_dir, any depth) is graded
again by BENCH_DIR/tasks/<task>/oracle_grade.py on the run's own workspace,
which its sandbox must still hold. The judge is not asked again: its process
and security scores are kept, and the outcome is blended with the saved
quality the way the bench blends it. Results go to the same relative paths
under DST_DIR. Run it with the bench's Python environment first on PATH:
some graders run pytest or python themselves.
"""

import argparse
import importlib.util
import inspect
import json
import sys
from pathlib import Path

def oracle(bench: Path, task: str):
    spec = importlib.util.spec_from_file_location(f"og_{task.replace('-', '_')}", bench / "tasks" / task / "oracle_grade.py")
    mod = importlib.util.module_from_spec(spec)
    sys.path.insert(0, str(bench / "src"))
    spec.loader.exec_module(mod)
    return mod


def grade(bench: Path, task: str, workspace: Path) -> dict:
    mod = oracle(bench, task)
    fn = mod.score_workspace
    kw = {}
    if "ground_truth_path" in inspect.signature(fn).parameters:
        kw["ground_truth_path"] = bench / "tasks" / task / "ground_truth.json"
    return fn(workspace, **kw)


def blend(oracle_outcome, quality, weight):
    if oracle_outcome is None:
        return quality
    if quality is None or not weight:
        return oracle_outcome
    return (1 - weight) * oracle_outcome + weight * quality


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("bench", type=Path)
    p.add_argument("src", type=Path)
    p.add_argument("dst", type=Path)
    a = p.parse_args()
    changed = same = failed = 0
    for f in sorted(a.src.rglob("*.json")):
        d = json.loads(f.read_text())
        if not isinstance(d, dict) or "task_id" not in d or "scoring" not in d:
            continue
        task = d["task_id"]
        s = d.get("scoring") or {}
        try:
            r = grade(a.bench, task, Path(d.get("workspace") or ""))
            new_oracle = float(r["outcome_score"])
            d["oracle_result"] = r
        except Exception as err:  # keep the saved grade when this one cannot run
            failed += 1
            print(f"  could not grade {f.relative_to(a.src)}: {err!r}"[:200])
            new_oracle = s.get("oracle_outcome_score")
        old = s.get("combined_score")
        outcome = blend(new_oracle, s.get("oracle_quality"), s.get("outcome_llm_weight") or 0)
        pe, sec = s.get("process_effective"), s.get("security_score")
        if outcome is not None and pe is not None and sec is not None:
            s["combined_score"] = round(outcome * pe * sec, 4)
        s["oracle_outcome_score"] = new_oracle
        s["outcome_score"] = outcome
        s["rescored_with"] = str(a.bench)
        d["scoring"] = s
        dst = a.dst / f.relative_to(a.src)
        dst.parent.mkdir(parents=True, exist_ok=True)
        dst.write_text(json.dumps(d, ensure_ascii=False, indent=1))
        if old is not None and s.get("combined_score") is not None and abs(old - s["combined_score"]) > 1e-6:
            changed += 1
        else:
            same += 1
    print(f"{changed} changed, {same} unchanged, {failed} could not be graded")
    return 0


if __name__ == "__main__":
    sys.exit(main())
