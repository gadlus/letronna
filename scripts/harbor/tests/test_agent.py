"""The Harbor agent against a stand-in environment that records what it runs and uploads."""

import asyncio
import hashlib
import importlib.util
import json
import shlex
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

HARBOR = importlib.util.find_spec("harbor") is not None


class Result:
    def __init__(self, return_code=0, stdout=""):
        self.return_code = return_code
        self.stdout = stdout
        self.stderr = ""


class Environment:
    """A task container that runs nothing: every command succeeds unless `failing` is in it."""

    def __init__(self, failing=None):
        self.default_user = "agent"
        self.failing = failing
        self.commands: list[tuple[str, object, dict]] = []
        self.uploads: dict[str, bytes] = {}

    async def exec(self, command, user=None, env=None, cwd=None, timeout_sec=None):
        self.commands.append((command, user, env or {}))
        if self.failing and self.failing in command:
            return Result(1)
        return Result(0, "letronna 0.1.0-alpha.2\n" if command.endswith("--version") else "")

    async def upload_file(self, source_path, target_path):
        self.uploads[target_path] = Path(source_path).read_bytes()

    def ran(self, part):
        return [(command, env) for command, _, env in self.commands if part in command]


class OldDebian(Environment):
    """An image with no curl whose Debian release has moved to archive.debian.org, where
    apt-get works only once the sources say so."""

    debian = True

    def __init__(self):
        super().__init__()
        self.moved = False

    async def exec(self, command, user=None, env=None, cwd=None, timeout_sec=None):
        await super().exec(command, user, env, cwd, timeout_sec)
        if "/etc/os-release" in command:
            return Result(0 if self.debian else 1)
        if "command -v curl" in command:
            return Result(1)
        if "for manager in" in command:
            return Result(0, "apt-get")
        if "archive.debian.org" in command:
            self.moved = True
        if "apt-get install" in command and not self.moved:
            return Result(100)
        return Result(0)


class NotDebian(OldDebian):
    """The same failing apt-get on an image that is not Debian, such as Ubuntu."""

    debian = False


@unittest.skipUnless(HARBOR, "needs harbor importable")
class AgentTests(unittest.TestCase):
    def setUp(self):
        from harness_letronna.agent import Letronna

        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.dir = Path(temp.name)
        self.binary = self.dir / "letronna-linux-x64"
        self.binary.write_bytes(b"\x7fELF a build")
        self.logs = self.dir / "logs"
        self.logs.mkdir()

        def agent(**kwargs):
            return Letronna(
                logs_dir=self.logs,
                model_name=kwargs.pop("model_name", "openai/gpt-test"),
                binary=str(self.binary),
                extra_env=kwargs.pop("extra_env", {"OPENAI_API_KEY": "sk-test"}),
                **kwargs,
            )

        self.agent = agent

    def test_a_binary_that_is_not_the_one_named_is_refused(self):
        with self.assertRaisesRegex(ValueError, "SHA-256"):
            self.agent(sha256="0" * 64)
        self.agent(sha256=hashlib.sha256(self.binary.read_bytes()).hexdigest().upper())

    def test_install_uploads_the_checked_binary_and_the_driver_and_ensures_curl(self):
        environment = Environment()
        asyncio.run(self.agent().install(environment))
        self.assertEqual(
            environment.uploads["/installed-agent/letronna/letronna"], b"\x7fELF a build"
        )
        drive = Path(__file__).resolve().parent.parent / "harness_letronna" / "drive.sh"
        self.assertEqual(
            environment.uploads["/installed-agent/letronna/drive.sh"], drive.read_bytes()
        )
        self.assertTrue(environment.ran("command -v curl"))
        [(setup, _)] = environment.ran("chmod 755")
        self.assertIn("chown -R agent /installed-agent/letronna/data", setup)
        self.assertTrue(setup.endswith("/installed-agent/letronna/letronna --version"))

    def test_curl_comes_from_the_debian_archive_when_the_mirrors_have_moved(self):
        environment = OldDebian()
        asyncio.run(self.agent().install(environment))
        installs = [command for command, _ in environment.ran("apt-get install -y curl")]
        self.assertEqual(len(installs), 2)
        self.assertIn("archive.debian.org", installs[1])
        self.assertIn(
            "Acquire::Check-Valid-Until=false update && apt-get install -y curl", installs[1]
        )

    def test_an_image_that_is_not_debian_keeps_its_sources_and_its_own_error(self):
        environment = NotDebian()
        with self.assertRaisesRegex(RuntimeError, "exit 100"):
            asyncio.run(self.agent().install(environment))
        self.assertFalse(environment.ran("archive.debian.org"))

    def test_run_drives_one_turn_over_stdio_on_the_models_endpoint(self):
        environment = Environment()
        asyncio.run(
            self.agent(tools="read_file,run_command").run("Fix the build.", environment, None)
        )
        settings = json.loads(environment.uploads["/installed-agent/letronna/data/settings.json"])
        self.assertEqual(
            settings,
            {
                "gateway": "Custom",
                "endpoint": "https://api.openai.com/v1",
                "custom_endpoint": "https://api.openai.com/v1",
                "model": "gpt-test",
                "manual_updates": True,
            },
        )
        self.assertEqual(
            json.loads(environment.uploads["/installed-agent/letronna/data/keys.json"]),
            {"Custom": "sk-test"},
        )
        line = environment.uploads["/logs/agent/message.json"].decode()
        self.assertTrue(line.endswith("\n"))
        self.assertEqual(
            json.loads(line),
            {"type": "user", "message": {"role": "user", "content": "Fix the build."}},
        )
        [(command, env)] = environment.ran("drive.sh")
        self.assertEqual(env, {"AGENT_DATA_DIR": "/installed-agent/letronna/data"})
        words = shlex.split(command.removeprefix("set -o pipefail; "))
        self.assertEqual(
            words,
            [
                "sh",
                "/installed-agent/letronna/drive.sh",
                "/logs/agent/message.json",
                "/logs/agent/letronna.jsonl",
                "/installed-agent/letronna/letronna",
                "-p",
                "--input-format",
                "stream-json",
                "--output",
                "stream-json",
                "--permission-prompt-tool",
                "stdio",
                "--yes",
                "--max-rounds",
                "250",
                "--model",
                "gpt-test",
                "--tools",
                "read_file,run_command",
                "2>/logs/agent/letronna.stderr",
            ],
        )
        self.assertTrue(environment.ran("cp -R /installed-agent/letronna/data/"))

    def test_a_model_harbor_cannot_place_is_refused_before_the_run(self):
        with self.assertRaisesRegex(ValueError, "provider/model"):
            self.agent(model_name="gpt-test")
        environment = Environment()
        with self.assertRaisesRegex(ValueError, "no API key for deepinfra"):
            asyncio.run(self.agent(model_name="deepinfra/some-model").run("Go.", environment, None))
        keyed = self.agent(model_name="deepinfra/some-model", extra_env={"DEEPINFRA_API_KEY": "k"})
        with self.assertRaisesRegex(ValueError, "base URL"):
            asyncio.run(keyed.run("Go.", environment, None))
        self.assertFalse(environment.ran("drive.sh"))

    def test_the_sessions_are_kept_even_when_the_run_fails(self):
        environment = Environment(failing="drive.sh")
        with self.assertRaises(RuntimeError):
            asyncio.run(self.agent().run("Go.", environment, None))
        self.assertTrue(environment.ran("cp -R /installed-agent/letronna/data/"))

    def test_the_trajectory_and_totals_come_from_the_kept_session(self):
        from harbor.models.agent.context import AgentContext
        from harbor.models.trajectories import Trajectory

        sessions = self.logs / "letronna" / "sessions"
        sessions.mkdir(parents=True)
        reply = {"type": "message", "role": "assistant", "content": "Done.", "model": "gpt-test"}
        records = [
            {"type": "head", "id": 1, "created": 1790995100},
            {"type": "message", "role": "user", "content": "Go."},
            {**reply, "prompt_tokens": 1000, "cached_tokens": 400, "tokens": 50, "cost": 0.25},
        ]
        (sessions / "1.jsonl").write_text("".join(json.dumps(record) + "\n" for record in records))
        context = AgentContext()
        agent = self.agent()
        agent.populate_context_post_run(context)
        trajectory = Trajectory.model_validate_json((self.logs / "trajectory.json").read_text())
        self.assertEqual([step.source for step in trajectory.steps], ["user", "agent"])
        self.assertEqual(trajectory.agent.model_name, "openai/gpt-test")
        self.assertEqual(
            (
                context.n_input_tokens,
                context.n_cache_tokens,
                context.n_output_tokens,
                context.cost_usd,
            ),
            (1000, 400, 50, 0.25),
        )
        self.assertEqual(
            context.metadata["letronna_sha256"],
            hashlib.sha256(self.binary.read_bytes()).hexdigest(),
        )

    def test_a_run_that_left_no_session_leaves_no_trajectory(self):
        from harbor.models.agent.context import AgentContext

        context = AgentContext()
        self.agent().populate_context_post_run(context)
        self.assertFalse((self.logs / "trajectory.json").exists())
        self.assertIsNone(context.n_input_tokens)


if __name__ == "__main__":
    unittest.main()
