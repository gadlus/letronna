"""The full Terminal-Bench kit: the relay against a stand-in provider, the janitor's
bookkeeping, the spend and the report, with no Docker, Harbor or model."""

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
from datetime import datetime
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

TB2 = Path(__file__).resolve().parent.parent / "tb2"
sys.path.insert(0, str(TB2))

import janitor  # noqa: E402
import relay  # noqa: E402


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Provider:
    """A provider that records each request and answers with a canned stream, or `status`,
    sending `headers` with either."""

    def __init__(self, status=200, cut=False, hang_up=False, headers=None):
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
                    for name, value in (headers or {}).items():
                        self.send_header(name, value)
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
                for name, value in (headers or {}).items():
                    self.send_header(name, value)
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
        self.assertNotIn("cost", row)
        self.assertNotIn("sk-or-secret", self.meter.read_text())

    def test_the_charge_an_upstream_sends_in_a_header_is_metered(self):
        for status in (200, 429):
            with self.subTest(status=status):
                self.meter.unlink(missing_ok=True)
                provider = Provider(status=status, headers={"X-Example-Cost": "0.0123"})
                base = self.relay("--upstream", provider.url, key="sk-other")
                got, _ = self.post(base, {"model": "m", "stream": True, "messages": []})
                self.assertEqual(got, status)
                [row] = self.meter_rows()
                self.assertEqual(row["cost"], 0.0123)

    def test_a_charge_is_read_from_any_header_named_for_one(self):
        self.assertEqual(relay.charged({"x-example-cost": "$0.5 USD"}), 0.5)
        self.assertEqual(relay.charged({"Date": "today", "X-Request-Cost": "2"}), 2.0)
        self.assertIsNone(relay.charged({"X-Example-Cost": "n/a"}))
        self.assertIsNone(relay.charged({"X-Example-Cost": "nan"}))
        self.assertIsNone(relay.charged({"X-Costs": "1"}))

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

    def test_only_the_compose_processes_of_ended_trials_are_to_be_stopped(self):
        self.assertEqual(janitor.project("mteb-leaderboard__pgiHjLk"), "mteb-leaderboard__pgihjlk__env")
        self.assertEqual(janitor.project("_odd.name__x"), "0_odd-name__x__env")
        cli = ["docker", "compose", "--project-name", "a__b__env", "-f", "c.yaml", "up", "-d"]
        plugin = ["/usr/libexec/docker/cli-plugins/docker-compose", "compose", "--project-name=a__b__env",
                  "up", "-d", ""]
        self.assertEqual(janitor.compose_project(cli), "a__b__env")
        self.assertEqual(janitor.compose_project(plugin), "a__b__env")
        self.assertIsNone(janitor.compose_project(["docker", "run", "--project-name", "a__b__env"]))
        self.assertIsNone(janitor.compose_project(["docker", "compose", "ps"]))
        with tempfile.TemporaryDirectory() as temp:
            jobs = Path(temp)
            for trial, done in [
                ("job/alpha__A1", True), ("job/beta__b1", False), ("job-alone/gamma__c1", True),
            ]:
                (jobs / trial).mkdir(parents=True)
                if done:
                    (jobs / trial / "result.json").write_text("{}")
            (jobs / "job" / "result.json").write_text("{}")
            self.assertEqual(
                janitor.ended(jobs), {"alpha__a1__env": "alpha__A1", "gamma__c1__env": "gamma__c1"}
            )

    @unittest.skipUnless(Path("/proc/self/cmdline").exists(), "reads /proc")
    def test_a_compose_left_by_an_ended_trial_is_stopped(self):
        # Names of this process's own, as the janitor looks at every process on the host.
        ended, running = f"alpha{os.getpid()}__a1", f"beta{os.getpid()}__b1"
        with tempfile.TemporaryDirectory() as temp:
            jobs = Path(temp)
            for trial in (ended, running):
                (jobs / "job" / trial).mkdir(parents=True)
            (jobs / "job" / ended / "result.json").write_text("{}")
            sleep = [sys.executable, "-c", "import time; time.sleep(30)", "compose", "--project-name"]
            left = subprocess.Popen([*sleep, f"{ended}__env", "up"])
            teardown = subprocess.Popen([*sleep, f"{ended}__env", "down"])
            started = subprocess.Popen([*sleep, f"{running}__env", "up"])
            for process in (left, teardown, started):
                self.addCleanup(process.wait)
                self.addCleanup(process.kill)
            self.assertEqual(janitor.stop_composes(jobs), [ended])
            self.assertEqual(left.wait(timeout=5), -15)
            self.assertIsNone(teardown.poll())
            self.assertIsNone(started.poll())

    def test_a_tasks_image_comes_from_its_config(self):
        with tempfile.TemporaryDirectory() as temp:
            task = Path(temp)
            (task / "task.toml").write_text('[environment]\ndocker_image = "org/task:1"\n')
            self.assertEqual(janitor.image_of(task), "org/task:1")
            self.assertIsNone(janitor.image_of(task / "missing"))


class SpendTests(unittest.TestCase):
    def spend(self, *args):
        return subprocess.run(
            [sys.executable, str(TB2 / "spend.py"), *map(str, args)],
            capture_output=True, text=True, check=False,
        )

    def test_charges_are_added_up_and_calls_without_one_counted_apart(self):
        with tempfile.TemporaryDirectory() as temp:
            meter = Path(temp) / "meter.jsonl"
            call = {"method": "POST", "path": "/v1/chat/completions", "status": 200}
            rows = [
                {**call, "t": 100, "usage": {"prompt_tokens": 9}, "cost": 0.5},
                {**call, "t": 200, "usage": {"prompt_tokens": 9, "cost": 0.25}},
                {**call, "t": 300, "usage": {"prompt_tokens": 9}},
                {**call, "t": 400, "status": 503, "usage": None, "error": "busy"},
                {"t": 500, "method": "GET", "path": "/v1/models", "status": 200, "usage": None},
            ]
            meter.write_text("".join(json.dumps(r) + "\n" for r in rows) + '{"t": 600, "meth')
            ran = self.spend(meter)
            self.assertEqual(ran.returncode, 0, ran.stderr)
            self.assertEqual(ran.stdout, f"{meter}: $0.75 over 2 calls, 1 call without a charge\n")
            self.assertIn(": $0.25 over 1 call, 1 call", self.spend(meter, "--since", "150").stdout)
            self.assertIn(": $0.25 over 1 call,", self.spend(meter, "--since", "1970-01-01T00:02:30").stdout)
            self.assertEqual(self.spend(meter, "--budget", "0.75").returncode, 3)
            self.assertEqual(self.spend(meter, "--budget", "1").returncode, 0)
            # A budget no total could reach is refused, rather than never stopping anything.
            for nonsense in ("nan", "inf", "abc"):
                refused = self.spend(meter, "--budget", nonsense)
                self.assertEqual(refused.returncode, 2)
                self.assertIn(f"not a number of dollars: '{nonsense}'", refused.stderr)
            # A meter that cannot be read is said to be so.
            unread = self.spend(temp, "--budget", "1")
            self.assertEqual(unread.returncode, 1)
            self.assertIn(f"cannot read the meter {temp}:", unread.stderr)
            # Another key's meter, and one not written yet.
            other = Path(temp) / "other.jsonl"
            other.write_text(meter.read_text())
            three = self.spend(meter, other, Path(temp) / "missing.jsonl", "--budget", "1.5")
            self.assertEqual(three.returncode, 3)
            self.assertIn("missing.jsonl: $0.00 over 0 calls\n", three.stdout)
            self.assertIn("In all: $1.50 over 4 calls, 2 calls without a charge\n", three.stdout)

    def test_a_key_is_out_of_credit_when_the_call_that_began_last_was_refused_with_402(self):
        with tempfile.TemporaryDirectory() as temp:
            meter, other = Path(temp) / "meter.jsonl", Path(temp) / "other.jsonl"
            call = {"method": "POST", "path": "/v1/chat/completions"}
            meter.write_text("".join(json.dumps(r) + "\n" for r in [
                {**call, "t": 100, "s": 1, "status": 402},
                # Began before the key ran dry, and ended after the refusal.
                {**call, "t": 90, "s": 20, "status": 200, "usage": {"prompt_tokens": 9}},
            ]))
            self.assertEqual(self.spend(meter, "--credit").returncode, 4)
            # A later call in another meter went through, then one only listed the models.
            other.write_text(json.dumps({**call, "t": 120, "s": 1, "status": 200}) + "\n"
                             + json.dumps({"t": 130, "s": 1, "method": "GET", "status": 402}) + "\n")
            self.assertEqual(self.spend(meter, other, "--credit").returncode, 0)
            other.write_text(json.dumps({**call, "t": 120, "s": 1, "status": 402}) + "\n")
            ran = self.spend(meter, other, "--credit")
            self.assertEqual(ran.returncode, 4)
            self.assertIn("refused with 402", ran.stdout)
            # Calls from before a run are not its own.
            self.assertEqual(self.spend(meter, other, "--credit", "--since", "125").returncode, 0)
            self.assertEqual(self.spend(meter, other).returncode, 0)


class AsideTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.job = self.root / "run"
        self.meter = self.root / "meter.jsonl"
        self.meter.write_text("")

    def trial(self, name, reward=0.0, error=None, started=True, letronna=None, tests=None,
              ctrf=False, hour=10):
        """A trial folder as Harbor leaves it, its agent done at `hour`, with Letronna's
        result line and the verifier's output."""
        folder = self.job / name
        (folder / "agent").mkdir(parents=True)
        (folder / "verifier").mkdir()
        result = {
            "task_name": name.split("__")[0],
            "verifier_result": None if reward is None else {"rewards": {"reward": reward}},
            "exception_info": {"exception_type": error} if error else None,
            "agent_execution": {"started_at": f"2026-10-17T{hour - 1:02d}:00:00Z",
                                "finished_at": f"2026-10-17T{hour}:00:00.000000Z"}
            if started else None,
        }
        (folder / "result.json").write_text(json.dumps(result))
        if letronna:
            line = json.dumps({"type": "result", **letronna}, separators=(",", ":"))
            (folder / "agent" / "letronna.jsonl").write_text(line + "\n")
        if tests is not None:
            (folder / "verifier" / "test-stdout.txt").write_text(tests)
        if ctrf:
            (folder / "verifier" / "ctrf.json").write_text("{}")

    def call(self, hour, before, status, error="Upstream busy."):
        """A call in the meter that ended `before` seconds before `hour`."""
        ended = datetime.fromisoformat(f"2026-10-17T{hour}:00:00Z").timestamp() - before
        row = {"t": ended - 0.5, "s": 0.5, "method": "POST", "status": status}
        if status >= 400:
            row["error"] = error
        with self.meter.open("a") as f:
            f.write(json.dumps(row) + "\n")

    def aside(self, *args):
        ran = subprocess.run(
            [sys.executable, str(TB2 / "aside.py"), str(self.job), *map(str, args)],
            capture_output=True, text=True, check=True,
        )
        return dict(line.split("\t") for line in ran.stdout.splitlines()), ran.stderr

    def test_the_trials_the_infrastructure_cost_are_set_aside_and_no_others(self):
        failed = {"status": "failed", "error": "The provider could not finish this response."}
        self.trial("env__1", reward=None, error="RuntimeError", started=False)
        self.trial("stopped__1", reward=None, error="CancelledError")
        self.trial("fetch__1", tests="  x Failed to download `pyyaml==6.0.3`\n  network timeout\n")
        self.trial("hung__1", reward=None, error="VerifierTimeoutError", tests="Resolved 12 packages\n")
        self.trial("ran__1", tests="network timeout in the agent's server\n", ctrf=True)
        self.trial("slow__1", reward=None, error="VerifierTimeoutError",
                   tests="===== test session starts =====\n")
        # Agents done at 10:00, just after a 402.
        self.trial("keyout__1", letronna=failed)
        self.trial("finished__1", letronna={"status": "done"})
        self.trial("passed__1", reward=1.0, letronna=failed)
        self.trial("own__1", letronna={"status": "failed", "error": "Blocked by the content filter."})
        self.call(10, 5, 402)
        # At 11:00, after a refusal that names a filter.
        self.trial("filtered__1", hour=11, letronna=failed)
        self.call(11, 3, 503, '{"error": {"code": "content_filter"}}')
        # At 12:00, after answers, and a 503 too long before.
        self.trial("quiet__1", hour=12, letronna=failed)
        self.call(12, 40, 503)
        self.call(12, 5, 200)
        # At 13:00, after a 400 and a 403, which may be the request's fault.
        self.trial("bad__1", hour=13, letronna=failed)
        self.call(13, 4, 400)
        self.call(13, 2, 403)
        self.assertEqual(
            self.aside("--meter", self.meter)[0],
            {"run/env__1": "environment", "run/stopped__1": "cancelled", "run/fetch__1": "verifier",
             "run/hung__1": "verifier", "run/keyout__1": "gateway"},
        )
        # With no meter, a failed run is the model's.
        self.assertEqual(
            set(self.aside()[0]), {"run/env__1", "run/stopped__1", "run/fetch__1", "run/hung__1"}
        )
        self.assertTrue((self.job / "env__1").is_dir())

    def test_a_trial_set_aside_moves_and_a_task_is_run_again_at_most_max_times(self):
        to = self.root / "aside"
        for done in ("env__0", "env__00", "stopped__0", "stopped__00"):
            (to / "run" / done).mkdir(parents=True)
        (to / "run" / "environment__0").mkdir()
        # Cancelled by a stop, which says nothing of the task.
        for stopped in ("environment__00", "environment__000"):
            (to / "run" / stopped).mkdir()
            (to / "run" / stopped / "result.json").write_text(
                json.dumps({"exception_info": {"exception_type": "CancelledError"}})
            )
        self.trial("env__1", reward=None, error="RuntimeError", started=False)
        self.trial("environment__1", reward=None, error="RuntimeError", started=False)
        self.trial("stopped__1", reward=None, error="CancelledError")
        picked, said = self.aside("--to", to, "--max", "2")
        self.assertEqual(picked, {"run/environment__1": "environment", "run/stopped__1": "cancelled"})
        self.assertTrue((to / "run" / "environment__1" / "result.json").exists())
        self.assertFalse((self.job / "environment__1").exists())
        self.assertTrue((self.job / "env__1").is_dir())
        self.assertIn("run/env__1\tenvironment, kept: env has been run again 2 times", said)


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
