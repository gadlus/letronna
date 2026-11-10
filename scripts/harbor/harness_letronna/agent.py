"""Letronna as a Harbor installed agent, for Terminal-Bench, SWE-bench and the rest.

`install` puts a Linux `letronna` binary, checked against its SHA-256, and
`drive.sh` in the task container, with curl, which the verifiers need. `run`
sets Letronna up on the Custom gateway at the endpoint of the provider the
model names, then runs one `letronna -p` turn over stream-json, through
`drive.sh`, as a run nobody watches: `--yes` and `LETRONNA_DISPOSABLE=1`
run every call without asking, hard-to-undo ones included, since the
container is thrown away; Letronna still refuses what it never runs. After the
run, the session files are copied into the agent's logs, and
`populate_context_post_run` turns them into `trajectory.json` and the token
totals.

The data folder holds the key, so it lives outside the logs, which are kept
and may be shared; only `sessions/` and `agents/` are copied out.
"""

import hashlib
import json
import shlex
import tempfile
import tomllib
from pathlib import Path, PurePosixPath

from harbor.agents.capabilities import AgentCapabilities
from harbor.agents.installed.base import BaseInstalledAgent, with_prompt_template
from harbor.agents.model_connection import ModelConnectionSpec
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext
from harbor.models.trajectories import Trajectory

from harness_letronna import atif

REMOTE = PurePosixPath("/installed-agent/letronna")
DATA = REMOTE / "data"
DRIVE = Path(__file__).with_name("drive.sh")
# SWE-bench Pro's leaderboard scaffold gives a task 250 turns; `-p`'s own
# 25 tool rounds would cut most Terminal-Bench and SWE tasks short.
ROUNDS = 250
# Seconds Letronna's deadline is set short of Harbor's agent timeout, so it
# stops with its result line and its session kept before Harbor kills it, as
# OpenHuman's rig sets it; never more than half the task's time.
DEADLINE_MARGIN = 120
# Images on Debian releases past their end of life have their packages on
# archive.debian.org only, so apt-get update fails there and curl cannot be
# installed until the sources point at it. The archive keeps no security or
# -updates suites for some releases, and its Release files are past their
# validity, so those lines go and the check is off for this one install.
APT_ARCHIVE = (
    "for f in /etc/apt/sources.list /etc/apt/sources.list.d/*.list; do "
    '[ -f "$f" ] || continue; '
    "sed -i -E -e '/debian-security|security\\.debian\\.org|-updates/d' "
    "-e 's#https?://deb\\.debian\\.org/debian([ /])#http://archive.debian.org/debian\\1#g' "
    '"$f"; done; '
    "apt-get -o Acquire::Check-Valid-Until=false update && apt-get install -y curl"
)
# Only Debian has moved releases there. Elsewhere the rewrite would drop an
# Ubuntu image's -updates lines or fail with an apt error in place of the
# real one.
DEBIAN = "grep -Eq '^ID=\"?debian\"?$' /etc/os-release && command -v apt-get >/dev/null 2>&1"


class Letronna(BaseInstalledAgent):
    """Letronna's terminal, run once per trial on the model Harbor names."""

    capabilities = AgentCapabilities(atif=True)
    MODEL_CONNECTION = ModelConnectionSpec()

    def __init__(
        self,
        *args,
        binary: str,
        sha256: str | None = None,
        max_rounds: int = ROUNDS,
        tools: str | list[str] | None = None,
        deadline_margin: int = DEADLINE_MARGIN,
        streams: bool | str = False,
        **kwargs,
    ) -> None:
        super().__init__(*args, **kwargs)
        if not self.model_name or "/" not in self.model_name:
            raise ValueError(
                "Name the model as provider/model, such as openai/gpt-5.4: Letronna "
                "is pointed at that provider's OpenAI-compatible endpoint."
            )
        self._binary = Path(binary).expanduser().resolve()
        self._sha256 = hashlib.sha256(self._binary.read_bytes()).hexdigest()
        if sha256 is not None and str(sha256).strip().lower() != self._sha256:
            raise ValueError(
                f"{self._binary} has SHA-256 {self._sha256}, not the {sha256} it was given."
            )
        self._rounds = int(max_rounds)
        self._tools = ",".join(tools) if isinstance(tools, list) else tools
        self._margin = int(deadline_margin)
        # An --ak arrives as the words typed, so "0" and "false" are no.
        self._streams = str(streams).strip().lower() in ("1", "true", "yes", "on")

    @staticmethod
    def name() -> str:
        return "letronna"

    def get_version_command(self) -> str | None:
        return f"{REMOTE}/letronna --version"

    def parse_version(self, stdout: str) -> str:
        words = stdout.split()
        return words[-1] if words else ""

    async def install(self, environment: BaseEnvironment) -> None:
        await self._ensure_curl(environment)
        await self.exec_as_root(environment, command=f"mkdir -p {DATA}")
        # A snapshot checked once and uploaded, so a binary replaced while
        # trials run cannot be half one build and half another.
        data = self._binary.read_bytes()
        if hashlib.sha256(data).hexdigest() != self._sha256:
            raise ValueError(f"{self._binary} changed since the agent was set up.")
        with tempfile.NamedTemporaryFile() as snapshot:
            snapshot.write(data)
            snapshot.flush()
            await environment.upload_file(Path(snapshot.name), str(REMOTE / "letronna"))
        await environment.upload_file(DRIVE, str(REMOTE / "drive.sh"))
        owner = environment.default_user
        chown = f" && chown -R {shlex.quote(str(owner))} {DATA}" if owner is not None else ""
        await self.exec_as_root(
            environment,
            command=(
                f"chmod 755 {REMOTE}/letronna && chmod 644 {REMOTE}/drive.sh{chown} && "
                f"{REMOTE}/letronna --version"
            ),
        )

    async def _ensure_curl(self, environment: BaseEnvironment) -> None:
        """Installs curl when the image has none, since without it a Terminal-Bench verifier
        cannot start and the trial scores zero."""
        try:
            await self.ensure_system_dependencies(environment, ("curl",))
        except Exception as error:
            debian = await environment.exec(command=DEBIAN, user="root")
            if debian.return_code != 0:
                raise
            self.logger.warning(
                "curl could not be installed (%s); trying archive.debian.org", error
            )
            await self.exec_as_root(
                environment, command=APT_ARCHIVE, env={"DEBIAN_FRONTEND": "noninteractive"}
            )

    @with_prompt_template
    async def run(
        self, instruction: str, environment: BaseEnvironment, context: AgentContext
    ) -> None:
        connection = self.model_connection
        if not connection.base_url and connection.api_key is None:
            # Harbor gives a provider's own endpoint only along with its key.
            raise ValueError(
                f"Harbor found no API key for {connection.provider}; set the one it "
                "reads for that provider, such as OPENAI_API_KEY for openai/ models."
            )
        if not connection.base_url:
            raise ValueError(
                f"Harbor knows no endpoint for {connection.provider}; give its "
                "OpenAI-compatible base URL, such as --ae OPENAI_BASE_URL=... "
                "with an openai/ model."
            )
        model = self.model_name.split("/", 1)[1]
        logs = self.environment_logs_dir
        await self.exec_as_agent(environment, command=f"mkdir -p {shlex.quote(str(logs))}")
        settings = {
            "gateway": "Custom",
            "endpoint": connection.base_url,
            "custom_endpoint": connection.base_url,
            "model": model,
            # Nobody is there to restart into an update.
            "manual_updates": True,
        }
        await self._upload_config_text(
            environment,
            content=json.dumps(settings),
            filename="settings.json",
            remote_path=str(DATA / "settings.json"),
        )
        if connection.api_key:
            await self._upload_config_text(
                environment,
                content=json.dumps({"Custom": connection.api_key}),
                filename="keys.json",
                remote_path=str(DATA / "keys.json"),
            )
        message = {"type": "user", "message": {"role": "user", "content": instruction}}
        await self._upload_config_text(
            environment,
            content=json.dumps(message) + "\n",
            filename="message.json",
            remote_path=str(logs / "message.json"),
        )
        flags = ["-p", "--input-format", "stream-json", "--output", "stream-json"]
        # Unwatched, as OpenHuman's rig runs it: answering approvals over
        # stdio would make a program someone who can answer, which turns off
        # what Letronna does for a run nobody watches and offers ask_user to
        # a model nobody will answer. LETRONNA_DISPOSABLE lets --yes run what
        # is hard to take back, as the container is thrown away.
        flags += ["--yes"]
        flags += ["--max-rounds", str(self._rounds), "--model", model]
        if self._tools:
            flags += ["--tools", self._tools]
        drive = [
            "sh",
            str(REMOTE / "drive.sh"),
            str(logs / "message.json"),
            str(logs / "letronna.jsonl"),
            str(REMOTE / "letronna"),
        ]
        command = shlex.join(drive + flags) + f" 2>{shlex.quote(str(logs / 'letronna.stderr'))}"
        # The task image's PATH and variables, not those a login shell's
        # profile would set over them; and a machine thrown away after it,
        # its files too, so a run that has read the task's own documents or
        # a web page may still move, write over and delete them.
        env = {
            "AGENT_DATA_DIR": str(DATA),
            "LETRONNA_NO_LOGIN_ENV": "1",
            "LETRONNA_DISPOSABLE": "1",
            "LETRONNA_DISPOSABLE_FILES": "1",
        }
        deadline = self._deadline()
        if deadline is not None:
            env["LETRONNA_DEADLINE"] = str(deadline)
        if self._streams:
            # Each request and its reply's stream as they came, beside the
            # run's other logs: what settles whether a tool call that
            # arrived wrong was sent so. No header is written, so no key.
            env["LETRONNA_DUMP_STREAMS"] = str(logs / "streams")
        try:
            await self.exec_as_agent(environment, command=command, env=env)
        finally:
            await self._keep_sessions(environment, logs)

    def _deadline(self) -> int | None:
        """Whole seconds for Letronna's deadline: the trial's agent timeout, worked out
        as Harbor works it out, less the margin but at least half of it. None when the
        trial has no timeout or its config cannot be read, as for a trial started some
        other way; Harbor's own timeout still holds."""
        from harbor.models.trial.config import TrialConfig

        try:
            config = TrialConfig.model_validate_json(
                (self.logs_dir.parent / "config.json").read_text()
            )
            toml = config.task.get_local_path() / "task.toml"
            task = tomllib.loads(toml.read_text()).get("agent", {}).get("timeout_sec")
        except Exception as error:
            self.logger.warning("The trial's agent timeout could not be read: %s", error)
            return None
        base = config.agent.override_timeout_sec or task
        if not base:
            return None
        multiplier = config.agent_timeout_multiplier
        if multiplier is None:
            multiplier = config.timeout_multiplier
        budget = int(min(base, config.agent.max_timeout_sec or float("inf")) * multiplier)
        if budget <= 0:
            return None
        return max(budget - self._margin, budget // 2, 1)

    async def _keep_sessions(self, environment: BaseEnvironment, logs: PurePosixPath) -> None:
        """Copies the run's session and helper files into the logs, for
        `populate_context_post_run` to read."""
        keep = shlex.quote(str(logs / "letronna"))
        command = (
            f"mkdir -p {keep} && for part in sessions agents; do "
            f'if [ -d {DATA}/"$part" ]; then cp -R {DATA}/"$part" {keep}/; fi; done'
        )
        try:
            await self.exec_as_agent(environment, command=command)
        except Exception as error:
            # Raised here, it would take the place of the run's own error.
            self.logger.warning("Letronna's session files could not be kept: %s", error)

    def populate_context_post_run(self, context: AgentContext) -> None:
        context.metadata = {**(context.metadata or {}), "letronna_sha256": self._sha256}
        sessions = sorted(
            (self.logs_dir / "letronna" / "sessions").glob("*.jsonl"),
            key=lambda path: int(path.stem) if path.stem.isdigit() else -1,
        )
        # The trajectory is a record, not the score: a session lost with the
        # sandbox or one that will not convert is logged, not raised, so the
        # trial keeps its result.
        if not sessions:
            self.logger.warning("Letronna left no session file, so there is no trajectory.")
            return
        try:
            built = atif.convert(
                sessions[-1], version=self.version() or "unknown", model=self.model_name
            )
            trajectory = Trajectory.model_validate(built) if built else None
        except Exception as error:
            self.logger.warning("Letronna's session could not be read as a trajectory: %s", error)
            return
        if trajectory is None:
            return
        (self.logs_dir / "trajectory.json").write_text(
            json.dumps(trajectory.to_json_dict(), ensure_ascii=False, indent=2) + "\n"
        )
        totals = trajectory.final_metrics
        context.n_input_tokens = totals.total_prompt_tokens
        context.n_cache_tokens = totals.total_cached_tokens
        context.n_output_tokens = totals.total_completion_tokens
        context.cost_usd = totals.total_cost_usd
