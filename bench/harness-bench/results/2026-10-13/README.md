# Harness-Bench, 13 October 2026

Letronna main `847fa57c` on all 106 tasks of [Harness-Bench](https://github.com/Qihoo360/harness-bench) at commit `1025086` with [`scripts/harnessbench/fork.patch`](../../../../scripts/harnessbench/fork.patch), one run per task per model, the models one after another. The judge was Claude Sonnet 4.6, and every model and the judge were reached through the api.pool.example gateway. A task with no graded result after its run and one rerun was run once more.

| File | What it holds |
|---|---|
| `summary.csv` | One row per model: graded tasks, score, passes, fails only on recorded broken checks, real fails, tokens and calls per task, web searches, and the model's earlier run on the same tasks |
| `per-task.csv` | One row per task and model: status (`pass`, `fail_grader_only`, `real_fail`, `not_graded`), score, outcome, tokens, calls, web searches and the checks that failed for real |
| `paper-comparison.csv` | The paper's Figure 3 and Table 2 numbers beside Letronna's |
| `results.json` | All of the above per task and model, with the bench's source and commit |

Scores are 0–1 here; [`docs/benchmarks.md`](../../../../docs/benchmarks.md) gives them × 100. A failed check counts as broken when [`known_issues.json`](../../known_issues.json) records it.
