"""The full Terminal-Bench kit: the relay against a stand-in provider, the janitor's
bookkeeping and the report, with no Docker, Harbor or model."""

import http.client
import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

TB2 = Path(__file__).resolve().parent.parent / "tb2"
sys.path.insert(0, str(TB2))

import janitor  # noqa: E402


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Provider:
    """A provider that records each request and answers with a canned stream, or `status`."""

    def __init__(self, status=200, cut=False, hang_up=False):
        self.requests = []
        provider = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                provider.requests.append((self.path, dict(self.headers), body))
                if hang_up:
                    self.connection.shutdown(socket.SHUT_RDWR)
                    return
                if status != 200:
                    data = b'{"error":{"message":"Rate limited."}}'
                    self.send_response(status)
                    self.send_header("Content-Length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
                    return
                chunks = [
                    {"choices": [{"index": 0, "delta": {"content": "Hi."}}]},
                    {"choices": [], "usage": {"prompt_tokens": 7, "completion_tokens": 2}},
                ]
                data = b"".join(b"data: " + json.dumps(c).encode() + b"\n\n" for c in chunks)
                data += b"data: [DONE]\n\n"
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                # Cut: the first chunk, then the connection goes.
                self.wfile.write(data[:40] if cut else data)
                if cut:
                    self.wfile.flush()
                    self.connection.shutdown(socket.SHUT_RDWR)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}/api/v1"


class RelayTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.meter = Path(temp.name) / "meter.jsonl"

    def relay(self, *args, key=None):
        port = free_port()
        env = {k: v for k, v in os.environ.items() if "proxy" not in k.lower()}
        if key:
            env["OPENROUTER_API_KEY"] = key
        process = subprocess.Popen(
            [sys.executable, str(TB2 / "relay.py"), "--host", "127.0.0.1", "--port", str(port),
             "--meter", str(self.meter), *args],
            env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        self.addCleanup(process.wait)
        self.addCleanup(process.kill)
        for _ in range(100):
            try:
                socket.create_connection(("127.0.0.1", port), timeout=0.1).close()
                break
            except OSError:
                time.sleep(0.05)
        return f"http://127.0.0.1:{port}"

    def post(self, base, body, path="/v1/chat/completions"):
        request = urllib.request.Request(
            base + path, data=json.dumps(body).encode(), method="POST",
            headers={
                "Content-Type": "application/json",
                "Authorization": "Bearer dummy",
                "User-Agent": "letronna/0.1.0",
            },
        )
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        try:
            with opener.open(request, timeout=10) as response:
                return response.status, response.read()
        except urllib.error.HTTPError as error:
            return error.code, error.read()

    def meter_rows(self, count=1):
        """The meter's rows once it holds `count`: each is written as its reply ends."""
        for _ in range(100):
            if self.meter.exists() and len(self.meter.read_text().splitlines()) >= count:
                break
            time.sleep(0.05)
        return [json.loads(line) for line in self.meter.read_text().splitlines()]

    def test_no_key_and_no_stub_is_refused(self):
        ran = subprocess.run(
            [sys.executable, str(TB2 / "relay.py"), "--meter", str(self.meter)],
            env={k: v for k, v in os.environ.items() if k != "OPENROUTER_API_KEY"},
            capture_output=True, text=True, timeout=10,
        )
        self.assertNotEqual(ran.returncode, 0)
        self.assertIn("OPENROUTER_API_KEY is not set", ran.stderr)

    def test_calls_go_upstream_pinned_with_the_key_and_are_metered(self):
        provider = Provider()
        base = self.relay(
            "--upstream", provider.url, "--style", "openrouter", "--model",
            "deepseek/deepseek-v4.1-flash", "--provider", "DeepSeek", "--reasoning", "high",
            key="sk-or-secret",
        )
        body = {"model": "other", "stream": True, "reasoning_effort": "low", "messages": []}
        status, data = self.post(base + "/t/trial-1", body)
        self.assertEqual(status, 200)
        self.assertIn(b'"Hi."', data)
        self.assertTrue(data.endswith(b"data: [DONE]\n\n"))
        [(path, headers, sent)] = provider.requests
        self.assertEqual(path, "/api/v1/chat/completions")
        self.assertEqual(headers["Authorization"], "Bearer sk-or-secret")
        # The caller's, as some gateways turn away Python's own.
        self.assertEqual(headers["User-Agent"], "letronna/0.1.0")
        self.assertEqual(sent["model"], "deepseek/deepseek-v4.1-flash")
        self.assertEqual(sent["provider"], {"order": ["DeepSeek"], "allow_fallbacks": False})
        self.assertEqual(sent["reasoning"], {"effort": "high"})
        self.assertNotIn("reasoning_effort", sent)
        self.assertEqual(sent["stream_options"], {"include_usage": True})
        [row] = self.meter_rows()
        self.assertEqual((row["tag"], row["status"]), ("trial-1", 200))
        self.assertEqual(row["usage"], {"prompt_tokens": 7, "completion_tokens": 2})
        self.assertNotIn("sk-or-secret", self.meter.read_text())

    def test_another_gateway_is_sent_openais_reasoning_field_and_no_provider(self):
        provider = Provider()
        base = self.relay(
            "--upstream", provider.url, "--provider", "DeepSeek", "--reasoning", "high",
            key="sk-other",
        )
        self.post(base, {"model": "m", "stream": True, "reasoning": {"effort": "low"}, "messages": []})
        [(_, _, sent)] = provider.requests
        self.assertEqual(sent["reasoning_effort"], "high")
        self.assertNotIn("reasoning", sent)
        self.assertNotIn("provider", sent)

    def test_a_stream_the_provider_cuts_ends_for_the_caller_at_once_and_is_metered(self):
        provider = Provider(cut=True)
        base = self.relay("--upstream", provider.url, key="sk-other")
        # Kept alive, as Letronna's connections are: urllib asks for each to
        # be closed after its reply, which would end a cut body on its own.
        connection = http.client.HTTPConnection(base.removeprefix("http://"), timeout=10)
        self.addCleanup(connection.close)
        started = time.monotonic()
        connection.request(
            "POST", "/v1/chat/completions",
            body=json.dumps({"model": "m", "stream": True, "messages": []}),
            headers={"Content-Type": "application/json"},
        )
        response = connection.getresponse()
        self.assertEqual(response.status, 200)
        with self.assertRaises(http.client.IncompleteRead):
            response.read()
        self.assertLess(time.monotonic() - started, 3)
        [row] = self.meter_rows()
        self.assertTrue(row["cut"].startswith("upstream: "), row)
        self.assertIsNone(row["usage"])
        self.assertIn("tail", row)

    def test_a_provider_that_hangs_up_first_is_a_502_and_metered_as_upstream(self):
        provider = Provider(hang_up=True)
        base = self.relay("--upstream", provider.url, key="sk-other")
        status, data = self.post(base, {"model": "m", "messages": []})
        self.assertEqual(status, 502)
        self.assertIn(b"relay:", data)
        [row] = self.meter_rows()
        self.assertTrue(row["cut"].startswith("upstream: RemoteDisconnected"), row)

    def test_a_provider_error_comes_back_as_it_was_and_is_metered(self):
        provider = Provider(status=429)
        base = self.relay("--upstream", provider.url, key="sk-or-secret")
        status, data = self.post(base, {"model": "m", "messages": []})
        self.assertEqual(status, 429)
        self.assertIn(b"Rate limited.", data)
        [row] = self.meter_rows()
        self.assertEqual(row["status"], 429)
        self.assertIn("Rate limited.", row["error"])

    def test_the_stub_calls_run_command_then_closes(self):
        base = self.relay("--stub")
        tools = [{"type": "function", "function": {"name": "run_command"}}]
        first = {"model": "stub-model", "messages": [{"role": "user", "content": "Go."}],
                 "tools": tools, "stream": True}
        status, data = self.post(base, first)
        self.assertEqual(status, 200)
        self.assertIn(b"run_command", data)
        self.assertIn(b"LETRONNA_", data)
        then = {**first, "messages": first["messages"] + [{"role": "tool", "content": "ok"}]}
        status, data = self.post(base, {**then, "stream": False})
        reply = json.loads(data)
        self.assertEqual(reply["choices"][0]["message"]["content"], "Done: the stub ran one command.")
        self.assertEqual(len(self.meter_rows(2)), 2)

    def test_the_stub_can_be_given_its_calls_arguments(self):
        call = '{"command": "python3 -m http.server 8000", "background": true}'
        base = self.relay("--stub", "--stub-call", call)
        tools = [{"type": "function", "function": {"name": "run_command"}}]
        status, data = self.post(base, {"messages": [], "tools": tools})
        [made] = json.loads(data)["choices"][0]["message"]["tool_calls"]
        self.assertEqual(json.loads(made["function"]["arguments"]), json.loads(call))


class JanitorTests(unittest.TestCase):
    def test_a_task_is_finished_once_every_trial_has_its_result(self):
        with tempfile.TemporaryDirectory() as temp:
            jobs = Path(temp)
            for trial, done in [
                ("job/alpha__a1", True), ("job/alpha__a2", False), ("job/beta__b1", True),
                ("job-alone/gamma__c1", False),
            ]:
                (jobs / trial).mkdir(parents=True)
                if done:
                    (jobs / trial / "result.json").write_text("{}")
            self.assertEqual(
                janitor.finished(jobs), {"alpha": False, "beta": True, "gamma": False}
            )

    def test_a_tasks_image_comes_from_its_config(self):
        with tempfile.TemporaryDirectory() as temp:
            task = Path(temp)
            (task / "task.toml").write_text('[environment]\ndocker_image = "org/task:1"\n')
            self.assertEqual(janitor.image_of(task), "org/task:1")
            self.assertIsNone(janitor.image_of(task / "missing"))


class ReportTests(unittest.TestCase):
    def test_tasks_are_scored_and_those_the_oracle_failed_are_set_apart(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)

            def trial(job, name, reward, error=None):
                folder = root / job / name
                folder.mkdir(parents=True)
                result = {
                    "task_name": name.split("__")[0],
                    "verifier_result": None if reward is None else {"rewards": {"reward": reward}},
                    "exception_info": {"exception_type": error} if error else None,
                    "agent_result": {"n_input_tokens": 1000, "n_output_tokens": 100},
                    "agent_execution": {
                        "started_at": "2026-10-15T10:00:00+00:00",
                        "finished_at": "2026-10-15T10:06:00+00:00",
                    },
                }
                (folder / "result.json").write_text(json.dumps(result))

            trial("run", "alpha__1", 1.0)
            trial("run", "beta__1", 0.0)
            trial("run", "gamma__1", None, "AgentTimeoutError")
            trial("oracle", "alpha__1", 1.0)
            trial("oracle", "beta__1", 0.0)
            trial("oracle", "gamma__1", 1.0)
            out = subprocess.run(
                [sys.executable, str(TB2 / "report.py"), str(root / "run"),
                 "--oracle", str(root / "oracle"), "--csv", str(root / "out.csv")],
                capture_output=True, text=True, check=True,
            ).stdout
            self.assertIn("**3 tasks, 3 trials.** Mean reward 0.333; 1 passed every run.", out)
            self.assertIn("The oracle failed 1 of these here (beta); on the other 2, mean reward 0.500.", out)
            self.assertIn("Errors: AgentTimeoutError 1.", out)
            self.assertIn("| gamma | 1 | 0.00 | 1 | AgentTimeoutError | 1K | 6.0 |", out)
            self.assertEqual(len((root / "out.csv").read_text().splitlines()), 4)


if __name__ == "__main__":
    unittest.main()
