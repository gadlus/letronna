"""The session-to-ATIF converter, on session folders laid out as Letronna writes them."""

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from harness_letronna import atif  # noqa: E402

HARBOR = importlib.util.find_spec("harbor") is not None


def message(role, content="", **fields):
    """One message line, with the fields Letronna always writes."""
    return {
        "type": "message",
        "role": role,
        "content": content,
        "tool_calls": [],
        "tool_call_id": "",
        **fields,
    }


def call(call_id, name, arguments):
    return {
        "id": call_id,
        "name": name,
        "arguments": json.dumps(arguments) if isinstance(arguments, dict) else arguments,
    }


def reply(content="", calls=(), prompt=1000, cached=400, tokens=50, **fields):
    return message(
        "assistant",
        content,
        model="gpt-test",
        tokens=tokens,
        prompt_tokens=prompt,
        cached_tokens=cached,
        at=1790995107,
        tool_calls=list(calls),
        **fields,
    )


def result(call_id, name, content, **fields):
    return message("tool", content, tool_call_id=call_id, name=name, **fields)


class Folder:
    """A data folder holding one session and its helpers."""

    def __init__(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)

    def session(self, records, id=7):
        path = self.root / "sessions" / f"{id}.jsonl"
        write(path, [{"type": "head", "id": id, "created": 1790995100}, *records])
        return path

    def helper(self, n, records, about, id=7):
        folder = self.root / "agents" / str(id)
        write(folder / f"{n}.jsonl", [{"type": "head", "id": id, "created": 1790995100}, *records])
        (folder / f"{n}.json").write_text(json.dumps(about))

    def calls(self, records, id=7):
        write(self.root / "agents" / str(id) / "calls.jsonl", records)

    def close(self):
        self.temp.cleanup()


def write(path, records):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("".join(json.dumps(record) + "\n" for record in records))


class ConvertTests(unittest.TestCase):
    def setUp(self):
        self.folder = Folder()
        self.addCleanup(self.folder.close)

    def convert(self, records):
        return atif.convert(self.folder.session(records), version="0.1.0", model="openai/gpt-test")

    def test_a_session_with_tool_calls_becomes_atif_steps_with_observations(self):
        trajectory = self.convert(
            [
                {"type": "meta", "title": "t", "model": "gpt-test"},
                message("user", "Clean the build folder."),
                reply(
                    "Cleaning up first.",
                    [
                        call("call_0", "run_command", {"command": "rm -r build"}),
                        call("call_1", "list_dir", {"path": "."}),
                    ],
                    reasoning="It is safe to remove.",
                ),
                result("call_0", "run_command", ""),
                result("call_1", "list_dir", "No such folder.", is_error=True),
                reply("Done."),
            ]
        )
        self.assertEqual(trajectory["schema_version"], "ATIF-v1.7")
        self.assertEqual(trajectory["session_id"], "7")
        self.assertEqual(
            trajectory["agent"],
            {"name": "letronna", "version": "0.1.0", "model_name": "openai/gpt-test"},
        )
        steps = trajectory["steps"]
        self.assertEqual(
            [(s["step_id"], s["source"]) for s in steps], [(1, "user"), (2, "agent"), (3, "agent")]
        )
        acted = steps[1]
        self.assertEqual(acted["message"], "Cleaning up first.")
        self.assertEqual(acted["reasoning_content"], "It is safe to remove.")
        self.assertEqual(acted["model_name"], "gpt-test")
        self.assertEqual(acted["timestamp"], "2026-10-03T02:38:27+00:00")
        self.assertEqual(
            acted["tool_calls"],
            [
                {
                    "tool_call_id": "call_0",
                    "function_name": "run_command",
                    "arguments": {"command": "rm -r build"},
                },
                {"tool_call_id": "call_1", "function_name": "list_dir", "arguments": {"path": "."}},
            ],
        )
        self.assertEqual(
            acted["observation"]["results"],
            [
                {"source_call_id": "call_0", "content": ""},
                {
                    "source_call_id": "call_1",
                    "content": "No such folder.",
                    "extra": {"is_error": True},
                },
            ],
        )
        self.assertEqual(
            acted["metrics"], {"prompt_tokens": 1000, "completion_tokens": 50, "cached_tokens": 400}
        )
        self.assertNotIn("observation", steps[2])

    def test_token_totals_match_the_session(self):
        trajectory = self.convert(
            [
                message("user", "Go."),
                reply(
                    "",
                    [call("call_0", "read_file", {"path": "a"})],
                    prompt=1000,
                    cached=400,
                    tokens=50,
                    cost=0.01,
                ),
                result("call_0", "read_file", "a"),
                # A gateway that counts cache hits beside prompt_tokens: the
                # input is both together, as Letronna counts it.
                reply("Done.", prompt=300, cached=500, tokens=20, cost=0.02),
            ]
        )
        self.assertEqual(
            trajectory["final_metrics"],
            {
                "total_prompt_tokens": 1800,
                "total_completion_tokens": 70,
                "total_cached_tokens": 900,
                "total_cost_usd": 0.03,
                "total_steps": 3,
            },
        )
        self.assertEqual(trajectory["steps"][2]["metrics"]["prompt_tokens"], 800)

    def test_cost_is_left_out_unless_every_call_has_one(self):
        trajectory = self.convert(
            [message("user", "Go."), reply("a", cost=0.01), message("user", "More."), reply("b")]
        )
        self.assertNotIn("total_cost_usd", trajectory["final_metrics"])
        estimated = self.convert(
            [message("user", "Go."), reply("a", cost=0.01, cost_estimated=True)]
        )
        self.assertEqual(estimated["final_metrics"]["total_cost_usd"], 0.01)
        self.assertEqual(estimated["final_metrics"]["extra"], {"cost_estimated": True})

    def test_helpers_become_subagent_trajectories_and_count_in_the_totals(self):
        self.folder.helper(
            2,
            [message("user", "Say hi."), reply("hi", prompt=100, cached=0, tokens=5)],
            {
                "name": "explore-2",
                "type": "explore",
                "model": "gpt-small",
                "parent": 7,
                "depth": 1,
                "description": "greeter",
            },
        )
        self.folder.helper(
            1,
            [message("user", "Look."), reply("seen", prompt=200, cached=0, tokens=6)],
            {"name": "explore"},
        )
        # What side calls such as a goal check took, kept with the helpers.
        self.folder.calls([reply(prompt=10, cached=0, tokens=1)])
        trajectory = self.convert(
            [
                message("user", "Go."),
                reply("", [call("call_0", "agent", {"prompt": "Say hi."})]),
                result("call_0", "agent", "explore finished.\nhi"),
                reply("Done.", summary_usage={"input": 3000, "cached": 0, "output": 100}),
            ]
        )
        helpers = trajectory["subagent_trajectories"]
        self.assertEqual([h["trajectory_id"] for h in helpers], ["helper-1", "helper-2"])
        second = helpers[1]
        self.assertEqual(second["session_id"], "7")
        self.assertEqual(
            second["agent"],
            {
                "name": "letronna",
                "version": "0.1.0",
                "model_name": "gpt-small",
                "extra": {
                    "name": "explore-2",
                    "type": "explore",
                    "description": "greeter",
                    "depth": 1,
                },
            },
        )
        self.assertEqual([s["source"] for s in second["steps"]], ["user", "agent"])
        self.assertEqual(second["final_metrics"]["total_prompt_tokens"], 100)
        totals = trajectory["final_metrics"]
        self.assertEqual(totals["total_prompt_tokens"], 1000 * 2 + 3000 + 100 + 200 + 10)
        self.assertEqual(totals["total_completion_tokens"], 50 * 2 + 100 + 5 + 6 + 1)
        self.assertEqual(totals["total_steps"], 3)

    def fork(self):
        """A session whose one reply forked a helper, laid out as Letronna writes a fork: its
        parent's messages, a result for the call still open, its task, then its own."""
        forking = reply(
            "",
            [call("call_0", "agent", {"prompt": "Run the tests.", "fork": True})],
            prompt=1000,
            cached=0,
            tokens=100,
            cost=0.01,
        )
        first = [
            message("user", "Go."),
            forking,
            result("call_0", "agent", "You are this call: the fork made here."),
            message("user", "You are a fork.\nTask:\n  Run the tests.", notice=True),
        ]
        self.folder.helper(
            1,
            [*first, reply("Passed.", prompt=1100, cached=0, tokens=20, cost=0.02)],
            {"name": "fork", "fork": True, "depth": 1, "from": len(first)},
        )
        return self.convert(
            [
                message("user", "Go."),
                forking,
                result("call_0", "agent", "fork finished.\nPassed."),
                reply("Done.", prompt=1200, cached=0, tokens=10, cost=0.03),
            ]
        )

    def test_a_fork_marks_its_parents_messages_as_copied_context(self):
        steps = self.fork()["subagent_trajectories"][0]["steps"]
        self.assertEqual(
            [(s["source"], s.get("is_copied_context", False)) for s in steps],
            [("user", True), ("agent", True), ("system", False), ("agent", False)],
        )
        self.assertNotIn("metrics", steps[1])

    def test_a_fork_counts_only_what_it_paid_for_itself(self):
        trajectory = self.fork()
        helper = trajectory["subagent_trajectories"][0]
        self.assertEqual(helper["final_metrics"]["total_prompt_tokens"], 1100)
        self.assertEqual(helper["final_metrics"]["total_completion_tokens"], 20)
        totals = trajectory["final_metrics"]
        self.assertEqual(totals["total_prompt_tokens"], 1000 + 1100 + 1200)
        self.assertEqual(totals["total_completion_tokens"], 100 + 20 + 10)
        self.assertAlmostEqual(totals["total_cost_usd"], 0.06)
        # Harbor sums the steps' own metrics, the helpers' included, into
        # the trial's model usage, which has to agree.
        steps = trajectory["steps"] + helper["steps"]
        metrics = [step["metrics"] for step in steps if "metrics" in step]
        self.assertEqual(sum(m["prompt_tokens"] for m in metrics), 1000 + 1100 + 1200)
        self.assertEqual(sum(m["completion_tokens"] for m in metrics), 100 + 20 + 10)

    def test_a_rewind_takes_back_messages_but_not_what_they_cost(self):
        taken = reply("Wrong.", prompt=500, cached=0, tokens=40)
        trajectory = self.convert(
            [
                message("user", "Go."),
                taken,
                {"type": "rewind", "to": 1},
                {**taken, "type": "paid"},
                reply("Right.", prompt=600, cached=0, tokens=30),
            ]
        )
        self.assertEqual([s["message"] for s in trajectory["steps"]], ["Go.", "Right."])
        self.assertEqual(trajectory["final_metrics"]["total_prompt_tokens"], 1100)
        self.assertEqual(trajectory["final_metrics"]["total_completion_tokens"], 70)

    def test_summaries_count_wherever_their_calls_were_saved(self):
        taken = reply(
            "Wrong.",
            prompt=500,
            cached=0,
            tokens=40,
            summary_usage={"input": 10000, "cached": 0, "output": 300},
        )
        # A summary the user asked for that a rewind cut, and one still in
        # the file, whose call no reply carries.
        cut = message("summary", summary_usage={"input": 2000, "cached": 0, "output": 100})
        made = {"at": 1790995107, "model": "gpt-test", "usage": {"input": 3000, "output": 200}}
        trajectory = self.convert(
            [
                message("user", "Go."),
                taken,
                {"type": "rewind", "to": 1},
                {**taken, "type": "paid"},
                {**cut, "type": "paid"},
                {"type": "summary", "upto": 1, "text": "Asked to go.", "call": made},
                reply("Right.", prompt=600, cached=0, tokens=30),
            ]
        )
        totals = trajectory["final_metrics"]
        self.assertEqual(totals["total_prompt_tokens"], 500 + 10000 + 2000 + 3000 + 600)
        self.assertEqual(totals["total_completion_tokens"], 40 + 300 + 100 + 200 + 30)

    def test_a_notice_is_a_system_step_not_the_users(self):
        trajectory = self.convert(
            [
                message("user", "Go."),
                reply("Working."),
                message("user", "[Notice] explore finished.", notice=True),
                reply("Done."),
                message("failure", "The provider is overloaded."),
            ]
        )
        self.assertEqual(
            [s["source"] for s in trajectory["steps"]],
            ["user", "agent", "system", "agent", "system"],
        )
        self.assertEqual(trajectory["steps"][4]["extra"], {"role": "failure"})

    def test_a_result_answers_the_latest_call_with_its_id(self):
        # Models that send no ids get call_0 in every reply.
        trajectory = self.convert(
            [
                message("user", "Go."),
                reply("", [call("call_0", "read_file", {"path": "a"})]),
                result("call_0", "read_file", "first"),
                reply("", [call("call_0", "read_file", {"path": "b"})]),
                result("call_0", "read_file", "second"),
                result("call_9", "read_file", "lost"),
            ]
        )
        first, second = trajectory["steps"][1], trajectory["steps"][2]
        self.assertEqual(first["observation"]["results"][0]["content"], "first")
        self.assertEqual(second["observation"]["results"][0]["content"], "second")
        lost = trajectory["steps"][3]
        self.assertEqual(lost["source"], "system")
        self.assertEqual(
            lost["observation"]["results"],
            [{"content": "lost", "extra": {"tool_call_id": "call_9", "name": "read_file"}}],
        )

    def test_arguments_that_are_not_an_object_are_kept_raw(self):
        trajectory = self.convert(
            [message("user", "Go."), reply("", [call("call_0", "run_command", '{"command": "ls"')])]
        )
        self.assertEqual(
            trajectory["steps"][1]["tool_calls"][0],
            {
                "tool_call_id": "call_0",
                "function_name": "run_command",
                "arguments": {},
                "extra": {"raw_arguments": '{"command": "ls"'},
            },
        )

    def test_a_session_with_no_messages_has_no_trajectory(self):
        self.assertIsNone(self.convert([{"type": "meta", "title": ""}]))

    @unittest.skipUnless(HARBOR, "needs harbor importable, to check against its own models")
    def test_the_trajectory_is_valid_atif_for_harbor(self):
        from harbor.models.trajectories import Trajectory

        self.folder.helper(
            1, [message("user", "Look."), reply("seen")], {"name": "explore", "depth": 1}
        )
        trajectory = self.convert(
            [
                message("user", "Go."),
                reply(
                    "",
                    [call("call_0", "run_command", '{"command"')],
                    reasoning="hmm",
                    interrupted=True,
                ),
                result("call_0", "run_command", "bad", is_error=True),
                result("call_9", "read_file", "lost"),
                message("user", "notice", notice=True),
                reply("Done.", cost=0.5, cost_estimated=True, cache_write_tokens=3),
            ]
        )
        Trajectory.model_validate(trajectory)


if __name__ == "__main__":
    unittest.main()
