"""Tests for the paired benchmark runner, on result files made up in a temporary folder.

    python3 -m unittest discover -s scripts/bench
"""

from __future__ import annotations

import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

import buckets  # noqa: E402
import report  # noqa: E402
import run  # noqa: E402

# Stands in for `python3 -m harnessbench.cli run-task`: it writes a result where
# the real bench would, scoring the candidate binary higher, and notes each call.
# Its `tasks` fails as the real one does without PyYAML when FAKE_BENCH_BROKEN is set.
FAKE_CLI = """
import json, os, sys
from pathlib import Path
args = sys.argv[1:]
if args == ["tasks"]:
    sys.exit("PyYAML is not installed" if os.environ.get("FAKE_BENCH_BROKEN") else 0)
task, harness = args[args.index("--task") + 1], args[args.index("--harness") + 1]
app = json.loads(Path(os.environ["HARNESSBENCH_APP_CONFIG"]).read_text())
config = json.loads(Path(os.environ["HARNESSBENCH_HARNESS_CONFIG"]).read_text())
entry = config["models"][harness]
with open(os.environ["FAKE_BENCH_CALLS"], "a") as calls:
    calls.write(f"{entry['command']} {entry['model']} {task} {os.environ['RUBRIC_MODEL']}\\n")
better = "candidate" in entry["command"]
checks = [{"id": "summary_exists", "label": "out/summary.json exists", "pass": better}]
scoring = {
    "combined_score": (0.8 if task == "001-file" else 0.7) if better else 0.5,
    "outcome_score": 1.0 if better else 0.0,
    "oracle_outcome_score": 1.0 if better else 0.0,
    "security_score": 1.0,
}
out = Path(app["results_dir"]) / harness / "fake-model" / f"{task}.json"
out.parent.mkdir(parents=True, exist_ok=True)
result = {"task_id": task, "oracle_result": {"checks": checks}, "scoring": scoring}
out.write_text(json.dumps(result))
"""


def made_up_result(
    task_id: str = "001-file",
    combined: float = 0.5,
    outcome: float = 0.5,
    checks: list | None = None,
    **scoring,
) -> dict:
    """A Harness-Bench result with only the fields the runner reads."""
    fair = {"combined_score": combined, "oracle_outcome_score": outcome, "security_score": 1.0}
    oracle = {"checks": checks or []}
    return {"task_id": task_id, "oracle_result": oracle, "scoring": fair | scoring}


def failed_check(check_id: str, label: str, weight: float = 1.0, detail: str = "") -> dict:
    """An oracle check that did not pass."""
    return {"id": check_id, "label": label, "pass": False, "weight": weight, "detail": detail}


def commit_all(folder: Path) -> str:
    """Makes a folder a git repository of one commit and gives that commit's id."""
    git = ["git", "-C", str(folder), "-c", "user.name=bench-test"]
    git += ["-c", "user.email=bench-test@example.invalid", "-c", "commit.gpgsign=false"]
    subprocess.run([*git, "init", "-q"], check=True)
    subprocess.run([*git, "add", "-A"], check=True)
    subprocess.run([*git, "commit", "-q", "--allow-empty", "-m", "bench"], check=True)
    head = subprocess.run([*git, "rev-parse", "HEAD"], capture_output=True, text=True, check=True)
    return head.stdout.strip()


def write_ledger(path: Path, commit: str, tasks: dict | None = None) -> Path:
    """Writes a known-issues ledger pinned to a commit."""
    path.write_text(json.dumps({"commit": commit, "tasks": tasks or {}}), encoding="utf-8")
    return path


def write_result(folder: Path, result: dict) -> None:
    """Puts a result where Harness-Bench would, under the harness id and a model slug."""
    path = folder / "results" / run.HARNESS_ID / "some-model" / f"{result['task_id']}.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(result), encoding="utf-8")


class RunnerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.folder = tempfile.TemporaryDirectory()
        self.tmp = Path(self.folder.name).resolve()

    def tearDown(self) -> None:
        self.folder.cleanup()

    def test_infra_failures_are_rerun_twice_and_then_count_as_zero(self) -> None:
        runs = []

        def spoiled_run() -> None:
            runs.append(1)
            spoiled = made_up_result(combined=0.9, proxy_trace_error="empty responses/")
            write_result(self.tmp, spoiled)

        printed = io.StringIO()
        with redirect_stdout(printed):
            run.run_until_clean("001-file", self.tmp, spoiled_run)
        self.assertEqual(len(runs), 1 + run.MAX_RERUNS)
        self.assertIn("spoiled: no proxy trace: empty responses/", printed.getvalue())
        result = run.newest_result(self.tmp, "001-file")
        self.assertEqual(report.score(result), 0.0)
        self.assertEqual(buckets.run_bucket(result, set(), {}), buckets.INFRA)
        run.run_until_clean("001-file", self.tmp, spoiled_run)
        self.assertEqual(len(runs), 1 + run.MAX_RERUNS, "carrying on gives it no more reruns")

    def test_a_clean_run_is_not_run_again(self) -> None:
        write_result(self.tmp, made_up_result())
        run.run_until_clean("001-file", self.tmp, lambda: self.fail("ran a finished task"))

    def test_the_denominator_stays_the_same_when_tasks_fail_to_run(self) -> None:
        tasks = ["001-file", "002-exec", "003-browser", "004-meeting-summary"]
        one_run = {
            "001-file": made_up_result("001-file", combined=1.0, outcome=1.0),
            "002-exec": made_up_result("002-exec", combined=0.6),
            "003-browser": None,
            "004-meeting-summary": made_up_result("004-meeting-summary", error="judge crashed"),
        }
        summary = report.arm_summary(tasks, [one_run], {})
        self.assertAlmostEqual(summary["mean"], 1.6 / 4)
        self.assertAlmostEqual(summary["pass_at_1"], 1 / 4)
        self.assertEqual(summary["infra"], 2)

    def test_a_paired_delta_needs_two_sigma_to_be_called_a_win(self) -> None:
        self.assertEqual(report.paired([0.10, -0.06, 0.08, -0.02])["call"], "no call")
        self.assertEqual(report.paired([0.10, 0.12, 0.09, 0.11])["call"], "win")
        self.assertEqual(report.paired([-0.10, -0.12, -0.09, -0.11])["call"], "loss")
        self.assertEqual(report.paired([0.5])["call"], "no call", "one task has no spread")
        self.assertEqual(report.paired([0.01, 0.01])["call"], "no call", "two alike are too few")
        self.assertEqual(report.paired([0.01] * 6)["call"], "win", "six alike pass the sign test")
        self.assertAlmostEqual(report.sign_test([0.1, 0.1, 0.1, 0.1, 0.0]), 0.125)

    def test_a_missing_output_check_is_bucketed_as_artifact_commitment(self) -> None:
        exists = failed_check("summary_exists", "out/summary.json exists")
        gone = "[Errno 2] No such file or directory: 'out/retry_ledger.csv'"
        unreadable = failed_check("ledger_read", "retry_ledger.csv parses", detail=gone)
        malformed = failed_check("ledger_read", "retry_ledger.csv parses", detail="bad quote")
        bare = failed_check("missing", "")
        said = failed_check("answers", "", detail="out/answers.json missing")
        task_id = "078-local-api-cursor-retry-ledger"
        self.assertEqual(buckets.check_bucket(task_id, exists, set()), buckets.ARTIFACT)
        self.assertEqual(buckets.check_bucket(task_id, unreadable, set()), buckets.ARTIFACT)
        self.assertEqual(buckets.check_bucket(task_id, bare, set()), buckets.ARTIFACT)
        self.assertEqual(buckets.check_bucket(task_id, said, set()), buckets.ARTIFACT)
        self.assertEqual(buckets.check_bucket(task_id, malformed, set()), buckets.CONTRACT)

    def test_checks_listed_as_oracle_defects_are_bucketed_apart(self) -> None:
        check = failed_check("report_missing", "final_report.md missing", weight=0.4)
        defects = {("012-doc-synthesis", "report_missing")}
        on_the_list = buckets.check_bucket("012-doc-synthesis", check, defects)
        off_the_list = buckets.check_bucket("014-task-decomposition", check, defects)
        self.assertEqual(on_the_list, buckets.ORACLE_DEFECT)
        self.assertEqual(off_the_list, buckets.ARTIFACT)
        only_defects = made_up_result("012-doc-synthesis", checks=[check])
        self.assertEqual(buckets.run_bucket(only_defects, defects, {}), buckets.ORACLE_DEFECT)

    def test_a_check_on_the_ledger_is_bucketed_as_oracle_defect(self) -> None:
        known = buckets.load_known_issues(buckets.KNOWN_ISSUES)
        self.assertEqual(known.commit, "1025086a446653702b80cfb48babbeec35db6b2c")
        self.assertEqual(known.not_for_tuning, set(known.ceilings))
        task_id = "025-meeting-action-tracker"
        never = failed_check("rationale_covers_merge_decisions", "rationale covers merge decisions")
        self.assertEqual(buckets.check_bucket(task_id, never, known.checks), buckets.ORACLE_DEFECT)
        self.assertEqual(buckets.check_bucket(task_id, never, set()), buckets.UNLABELLED)
        at_most = made_up_result(task_id, checks=[never], outcome_score=0.9444)
        self.assertEqual(
            buckets.run_bucket(at_most, known.checks, known.ceilings), buckets.ORACLE_DEFECT
        )
        task_id = "064-service-dependency-triage"
        at_ceiling = made_up_result(task_id, outcome_score=0.8372)
        short = made_up_result(task_id, outcome_score=0.8)
        gated = made_up_result(task_id, outcome_score=0.8372, security_score=0.0)
        for result, bucket in [
            (at_ceiling, buckets.ORACLE_DEFECT),
            (short, buckets.UNLABELLED),
            (gated, buckets.UNLABELLED),
        ]:
            self.assertEqual(buckets.run_bucket(result, known.checks, known.ceilings), bucket)

    def test_a_failed_run_takes_the_bucket_that_lost_the_most_weight(self) -> None:
        checks = [
            failed_check("ledger_header", "retry ledger header exact", weight=1.0),
            failed_check("ledger_events", "ledger records 429, 503, and cursor recovery", 3.0),
            {"id": "datasets", "label": "datasets exact", "pass": True, "weight": 3.0},
        ]
        self.assertEqual(buckets.run_bucket(made_up_result(checks=checks), set(), {}), buckets.TOOL)

    def test_a_run_whose_last_model_call_got_a_5xx_is_infra(self) -> None:
        proxy_log = self.tmp / "usage-proxy" / "requests.jsonl"
        proxy_log.parent.mkdir(parents=True)
        result = made_up_result() | {"sandbox": str(self.tmp)}
        proxy_log.write_text('{"status": 503}\n{"status": 200}\n', encoding="utf-8")
        self.assertIsNone(buckets.infra_reason(result))
        proxy_log.write_text('{"status": 200}\n{"status": 503}\n', encoding="utf-8")
        self.assertEqual(buckets.infra_reason(result), "the last model call got HTTP 503")

    def test_a_run_passes_on_its_oracle_whatever_the_judge_blends_in(self) -> None:
        judged_down = made_up_result("008-image-recognize", outcome=1.0, outcome_score=0.55)
        self.assertTrue(report.passed(judged_down))
        judged_up = made_up_result("013-image-edit", outcome=0.9, outcome_score=1.0)
        self.assertFalse(report.passed(judged_up))
        only_oracle = made_up_result() | {"oracle_result": {"outcome_score": 1.0}}
        del only_oracle["scoring"]["oracle_outcome_score"]
        self.assertTrue(report.passed(only_oracle))

    def test_the_band_keeps_tasks_passed_30_to_70_percent_of_the_time(self) -> None:
        passes = {"001-file": 0, "002-exec": 1, "007-session-memory": 3}
        runs = [
            {task: made_up_result(task, outcome=float(i < n)) for task, n in passes.items()}
            for i in range(3)
        ]
        self.assertEqual(report.in_band(list(passes), runs, set()), ["002-exec"])
        self.assertEqual(report.in_band(list(passes), runs, {"002-exec"}), [])

    def test_ceiling_normalised_completion_divides_by_the_ledger_ceiling(self) -> None:
        tasks = ["012-doc-synthesis", "001-file", "002-exec"]
        one_run = {
            "012-doc-synthesis": made_up_result("012-doc-synthesis", outcome_score=0.75),
            "001-file": made_up_result("001-file", outcome_score=0.5),
            "002-exec": None,
        }
        summary = report.arm_summary(tasks, [one_run], {"012-doc-synthesis": 0.75})
        self.assertAlmostEqual(summary["completion"], 1.25 / 3)
        self.assertAlmostEqual(summary["completion_of_ceiling"], 1.5 / 3)

    def test_efficiency_counts_calls_cache_hits_and_the_first_requests_static_prompt(self) -> None:
        responses = self.tmp / "usage-proxy" / "responses"
        responses.mkdir(parents=True)
        listing = {"path": "/models", "request_body": ""}
        system = [{"type": "text", "text": "You are Letronna."}, {"type": "text", "text": "Brief."}]
        tools = [{"type": "function", "function": {"name": "read_file"}}]
        messages = [{"role": "system", "content": system}, {"role": "user", "content": "Go."}]
        opening = {"messages": messages, "tools": tools}
        later = {"messages": [{"role": "system", "content": "a much longer prompt"}]}
        for name, record in [
            ("0001.json", listing),
            ("0002.json", {"path": "/chat/completions", "request_body": json.dumps(opening)}),
            ("0003.json", {"path": "/chat/completions", "request_body": json.dumps(later)}),
        ]:
            (responses / name).write_text(json.dumps(record), encoding="utf-8")
        usage = {"available": True, "input_tokens": 300, "cache_read_tokens": 100}
        rounds = [{"metadata": {"model_calls": 3}}, {"metadata": {"model_calls": 2}}]
        kept = made_up_result("001-file") | {
            "sandbox": str(self.tmp),
            "usage_summary": usage,
            "adapter_results": rounds,
        }
        deleted = made_up_result("002-exec") | {
            "sandbox": str(self.tmp / "deleted"),
            "usage_summary": {"available": True, "input_tokens": 100, "cache_read_tokens": 0},
            "adapter_results": [{"metadata": {"model_calls": 1}}],
        }
        one_run = {"001-file": kept, "002-exec": deleted, "003-browser": None}
        spent = report.efficiency(list(one_run), [one_run], len)
        self.assertEqual(spent["calls"], 3.0, "five calls and one, over the runs with a result")
        self.assertAlmostEqual(spent["cache_hit"], 100 / 500)
        schemas = '[{"type":"function","function":{"name":"read_file"}}]'
        self.assertEqual(spent["static_prompt"], len("You are Letronna.\nBrief.") + len(schemas))
        nothing = report.efficiency(["001-file"], [{"001-file": made_up_result()}], len)
        self.assertEqual(nothing, {"calls": None, "cache_hit": None, "static_prompt": None})

    def test_the_runner_refuses_a_fork_on_a_different_commit(self) -> None:
        bench = self.tmp / "bench"
        (bench / "src" / "harnessbench").mkdir(parents=True)
        (bench / "src" / "harnessbench" / "cli.py").write_text("", encoding="utf-8")
        (bench / "tasks" / "001-file").mkdir(parents=True)
        binary = self.tmp / "letronna"
        binary.write_text("#!/bin/sh\n", encoding="utf-8")
        binary.chmod(0o755)
        (self.tmp / "entry.json").write_text("{}", encoding="utf-8")
        (self.tmp / "tasks.txt").write_text("001-file\n", encoding="utf-8")
        argv = [
            "--bench", str(bench),
            "--entry", str(self.tmp / "entry.json"),
            "--baseline", str(binary),
            "--candidate", str(binary),
            "--tasks", str(self.tmp / "tasks.txt"),
            "--models", "some/model",
            "--out", str(self.tmp / "out"),
            "--known-issues", str(write_ledger(self.tmp / "ledger.json", "1" * 40)),
        ]  # fmt: skip
        with mock.patch.dict(os.environ, {"RUBRIC_API_KEY": "made-up"}):
            with self.assertRaisesRegex(SystemExit, "is at no git commit, but"):
                run.main(argv)
            moved = commit_all(bench)
            with self.assertRaisesRegex(SystemExit, f"is at {moved}, but .* Harness-Bench 1111"):
                run.main(argv)
        self.assertFalse((self.tmp / "out").exists(), "nothing was started")

    def test_a_paired_run_goes_through_run_task_for_every_arm_repeat_and_task(self) -> None:
        bench = self.tmp / "bench"
        (bench / "src" / "harnessbench").mkdir(parents=True)
        (bench / "src" / "harnessbench" / "__init__.py").write_text("", encoding="utf-8")
        (bench / "src" / "harnessbench" / "cli.py").write_text(FAKE_CLI, encoding="utf-8")
        for task_id in ("001-file", "002-exec"):
            (bench / "tasks" / task_id).mkdir(parents=True)
        binaries = {}
        for arm in run.ARMS:
            binaries[arm] = self.tmp / arm / "letronna"
            binaries[arm].parent.mkdir()
            binaries[arm].write_text("#!/bin/sh\n", encoding="utf-8")
            binaries[arm].chmod(0o755)
        (self.tmp / "entry.json").write_text('{"adapter": "letronna"}', encoding="utf-8")
        (self.tmp / "tasks.txt").write_text("# two\n001-file\n002-exec\n", encoding="utf-8")
        issue = {"checks": ["summary_exists"]}
        listed = {"ceiling": 1.0, "exclude_from_tuning": True, "issues": [issue]}
        ledger = write_ledger(self.tmp / "ledger.json", commit_all(bench), {"002-exec": listed})
        out, calls = self.tmp / "out", self.tmp / "calls.txt"
        env = {"FAKE_BENCH_CALLS": str(calls), "RUBRIC_API_KEY": "made-up"}
        argv = [
            "--bench", str(bench),
            "--entry", str(self.tmp / "entry.json"),
            "--baseline", str(binaries["baseline"]),
            "--candidate", str(binaries["candidate"]),
            "--tasks", str(self.tmp / "tasks.txt"),
            "--models", "some/model",
            "--k", "2",
            "--out", str(out),
            "--known-issues", str(ledger),
        ]  # fmt: skip
        no_judge = {
            "RUBRIC_API_KEY": "",
            "OPENAI_API_KEY": "",
            "HARNESSBENCH_SKIP_PROCESS_GRADE": "",
        }
        (self.tmp / "repo" / ".git").mkdir(parents=True)
        (self.tmp / "twice.txt").write_text("001-file\n001-file\n", encoding="utf-8")
        with mock.patch.dict(os.environ, env), redirect_stdout(io.StringIO()):
            with (
                mock.patch.dict(os.environ, no_judge),
                self.assertRaisesRegex(SystemExit, "no key"),
            ):
                run.main(argv)
            with (
                mock.patch.dict(os.environ, {"FAKE_BENCH_BROKEN": "1"}),
                self.assertRaisesRegex(SystemExit, "PyYAML is not installed"),
            ):
                run.main(argv)
            with self.assertRaisesRegex(SystemExit, "git checkout"):
                run.main([*argv, "--out", str(self.tmp / "repo" / "runs")])
            with self.assertRaisesRegex(SystemExit, "would share"):
                run.main([*argv, "--models", "a/b,a-b"])
            with self.assertRaisesRegex(SystemExit, "more than once: 001-file"):
                run.main([*argv, "--tasks", str(self.tmp / "twice.txt")])
            self.assertFalse(calls.exists(), "nothing ran before the setup was right")
            run.main(argv)
            run.main(argv)
        lines = calls.read_text(encoding="utf-8").splitlines()
        self.assertEqual(len(lines), 2 * 2 * 2, "each task once per arm and repeat, not again")
        self.assertEqual(lines[0], f"{binaries['baseline']} some/model 001-file {run.JUDGE}")
        self.assertEqual(lines[1], f"{binaries['candidate']} some/model 001-file {run.JUDGE}")
        self.assertEqual(lines[4], f"{binaries['candidate']} some/model 001-file {run.JUDGE}")

        printed = io.StringIO()
        with redirect_stdout(printed):
            report.main([str(out), "--known-issues", str(ledger)])
        text = printed.getvalue()
        self.assertIn("| score, mean of 2 runs | 0.500 | 0.750 |", text)
        self.assertIn("| completion over each task's ceiling | 0.000 | 1.000 |", text)
        self.assertIn("| pass^2 | 0.000 | 1.000 |", text)
        self.assertIn("| prompt cache hit, cached of input tokens | n/a | n/a |", text)
        self.assertIn("time to first token and call latency are not given", text)
        self.assertIn("**win**", text)
        self.assertIn("| artifact commitment | 2 | 0 |", text)
        self.assertIn("| oracle defect | 2 | 0 |", text)
        self.assertIn("| 001-file | summary_exists | artifact commitment | 0 | 2 |", text)
        self.assertIn("| 002-exec | summary_exists | oracle defect | 0 | 2 |", text)
        with redirect_stdout(io.StringIO()), self.assertRaisesRegex(SystemExit, "is pinned to"):
            report.main([str(out)])

        entry = self.tmp / "entry.json"
        with mock.patch.dict(os.environ, env), redirect_stdout(io.StringIO()):
            entry.write_text('{"adapter": "letronna", "max_rounds": 5}', encoding="utf-8")
            with self.assertRaisesRegex(SystemExit, "other settings"):
                run.main(argv)
            entry.write_text('{"adapter": "letronna"}', encoding="utf-8")
            with (
                mock.patch.dict(os.environ, {"HARNESSBENCH_SKIP_PROCESS_GRADE": "1"}),
                self.assertRaisesRegex(SystemExit, "other settings"),
            ):
                run.main(argv)
            binaries["candidate"].write_text("#!/bin/sh\n# rebuilt\n", encoding="utf-8")
            with self.assertRaisesRegex(SystemExit, "other settings"):
                run.main(argv)
            (out / "manifest.json").write_text("{", encoding="utf-8")
            with self.assertRaisesRegex(SystemExit, "cannot be read"):
                run.main(argv)


if __name__ == "__main__":
    unittest.main()
