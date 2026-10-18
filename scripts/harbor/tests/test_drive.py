"""drive.sh, run under sh against a stand-in for `letronna` that plays out a scripted run."""

import json
import os
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

DRIVE = Path(__file__).resolve().parent.parent / "harness_letronna" / "drive.sh"

# Writes each line of its script, reading an answer after every request, and
# keeps everything it reads in HEARD. It exits only once stdin closes, as
# Letronna does, so a driver that leaves stdin open hangs the test; a line
# EXIT has it exit at once instead, as a crash would, and a line CLOSE has it
# stop reading.
STAND_IN = textwrap.dedent(
    """
    import json, os, sys

    script = json.loads(os.environ["SCRIPT"])
    heard = open(os.environ["HEARD"], "w")
    closed = False

    def read():
        if closed:
            return ""
        line = sys.stdin.readline()
        heard.write(line)
        heard.flush()
        return line

    read()
    for line in script:
        if line == "EXIT":
            sys.exit(int(os.environ.get("CODE", "0")))
        if line == "CLOSE":
            os.close(0)
            closed = True
            continue
        print(line, flush=True)
        if '"control_request"' in line:
            read()
    while read():
        pass
    heard.write("EOF\\n")
    sys.exit(int(os.environ.get("CODE", "0")))
    """
)

SORTED = (
    '{"request":{"input":{"command":"rm -r build"},"subtype":"can_use_tool",'
    '"tool_name":"run_command",'
    '"tool_use_id":"call_0"},"request_id":"letronna-1","type":"control_request"}'
)
# The order serde_json keeps with preserve_order, and a tool input holding
# what looks like another request's id.
WRITTEN = (
    '{"type":"control_request","request_id":"letronna-2","request":{"subtype":"can_use_tool",'
    '"tool_name":"write_file","input":{"path":"x","content":"\\"request_id\\":\\"letronna-9\\"",'
    '"request_id":"letronna-8"},"tool_use_id":"call_1"}}'
)
PROGRESS = '{"cached":0,"input":10,"output":1,"rounds":1,"type":"progress"}'
# As Letronna writes it, sorted, with "window" after "type".
RESULT = (
    '{"answer":"Done.","cached":0,"files":[],"input":10,"output":1,"rounds":1,"seconds":0,'
    '"status":"done","type":"result","window":{"source":"listed","tokens":128000}}'
)


class DriveTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.dir = Path(temp.name)
        self.message = self.dir / "message.json"
        self.message.write_text('{"type":"user","message":{"role":"user","content":"Go."}}\n')
        self.log = self.dir / "letronna.jsonl"
        self.heard = self.dir / "heard"
        (self.dir / "stand_in.py").write_text(STAND_IN)

    def drive(self, script, code=0):
        env = {
            **os.environ,
            "SCRIPT": json.dumps(script),
            "HEARD": str(self.heard),
            "CODE": str(code),
        }
        command = [
            "sh",
            str(DRIVE),
            str(self.message),
            str(self.log),
            sys.executable,
            str(self.dir / "stand_in.py"),
        ]
        return subprocess.run(command, env=env, capture_output=True, text=True, timeout=30)

    def answers(self):
        lines = self.heard.read_text().splitlines()
        return [
            json.loads(line)["response"]["request_id"]
            for line in lines
            if line.startswith('{"type":"control_response"')
        ]

    def test_every_request_is_allowed_and_stdin_closes_after_the_result(self):
        ran = self.drive([PROGRESS, SORTED, PROGRESS, RESULT])
        self.assertEqual(ran.returncode, 0, ran.stderr)
        heard = self.heard.read_text().splitlines()
        self.assertEqual(json.loads(heard[0])["message"]["content"], "Go.")
        self.assertEqual(
            json.loads(heard[1]),
            {
                "type": "control_response",
                "response": {
                    "request_id": "letronna-1",
                    "subtype": "success",
                    "response": {"behavior": "allow"},
                },
            },
        )
        self.assertEqual(heard[-1], "EOF")
        self.assertEqual(ran.stdout, RESULT + "\n")
        self.assertEqual(self.log.read_text().splitlines(), [PROGRESS, SORTED, PROGRESS, RESULT])

    def test_the_result_is_found_in_any_key_order(self):
        last = '{"answer":"Done.","rounds":1,"status":"done","type":"result"}'
        written = '{"type":"result","status":"done","answer":"Done.","rounds":1}'
        for result in (RESULT, last, written):
            with self.subTest(result=result):
                ran = self.drive([PROGRESS, result])
                self.assertEqual(ran.returncode, 0, ran.stderr)
                self.assertEqual(ran.stdout, result + "\n")
                self.assertEqual(self.heard.read_text().splitlines()[-1], "EOF")

    def test_request_ids_are_read_in_either_key_order(self):
        ran = self.drive([SORTED, WRITTEN, RESULT])
        self.assertEqual(ran.returncode, 0, ran.stderr)
        self.assertEqual(self.answers(), ["letronna-1", "letronna-2"])

    def test_the_exit_code_is_letronnas(self):
        failed = RESULT.replace(
            '"status":"done"', '"error":"The provider is overloaded.","status":"failed"'
        )
        ran = self.drive([failed], code=1)
        self.assertEqual(ran.returncode, 1)
        self.assertEqual(ran.stdout, failed + "\n")

    def test_a_letronna_that_stops_reading_still_has_its_result_kept(self):
        ran = self.drive(["CLOSE", SORTED, RESULT], code=1)
        self.assertEqual(ran.returncode, 1, ran.stderr)
        self.assertEqual(ran.stdout, RESULT + "\n")
        self.assertEqual(self.log.read_text().splitlines(), [SORTED, RESULT])

    def test_a_run_that_ends_without_a_result_still_ends(self):
        ran = self.drive([PROGRESS, "EXIT"], code=3)
        self.assertEqual(ran.returncode, 3)
        self.assertEqual(ran.stdout, "")
        self.assertEqual(self.log.read_text().splitlines(), [PROGRESS])


if __name__ == "__main__":
    unittest.main()
