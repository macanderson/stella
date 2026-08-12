"""Carrying learned state between trials, and marking the runs that did.

These tests pin the two halves of the contract in :mod:`stella_harbor.lineage`.
The capability half: learned artifacts are harvested out of a container into a
generation-numbered store, and a later trial's container is seeded from it. The
half that makes the capability publishable: a run with no lineage configured is
untouched by any of it, and a run that *was* seeded says so where nothing can
drop it silently.
"""

from __future__ import annotations

import asyncio
import io
import json
import tarfile
from pathlib import Path
from types import SimpleNamespace

import pytest

pytest.importorskip("harbor", reason="Harbor is required to import the adapter")

from stella_harbor import StellaAgent  # noqa: E402
from stella_harbor.lineage import (  # noqa: E402
    LINEAGE_ENV,
    LineageError,
    LineageStore,
    disclosed_lineage,
    discover_task_id,
    harvest_lineage,
    harvest_script,
    parse_lineage_report,
    parse_lineage_selector,
    seed_lineage,
    seed_script,
    task_slug,
)


class FakeS3:
    """An in-memory S3 with the conditional-put semantics `publish` relies on.

    Hand-written rather than moto, matching ``arenabench.tests.test_cloud``:
    the surface used here is three calls wide and a fake makes the
    precondition path testable, which is the whole point of the retry loop.
    """

    def __init__(self) -> None:
        self.objects: dict[str, bytes] = {}
        self.calls: list[tuple[str, str]] = []

    def put_object(self, **kwargs):
        key = kwargs["Key"]
        self.calls.append(("put", key))
        if kwargs.get("IfNoneMatch") == "*" and key in self.objects:
            raise _precondition_failed()
        self.objects[key] = kwargs["Body"]
        return {}

    def get_object(self, **kwargs):
        key = kwargs["Key"]
        self.calls.append(("get", key))
        if key not in self.objects:
            raise KeyError(key)
        return {"Body": io.BytesIO(self.objects[key])}

    def list_objects_v2(self, **kwargs):
        prefix = kwargs["Prefix"]
        self.calls.append(("list", prefix))
        common = {
            prefix + key[len(prefix) :].split("/", 1)[0] + "/"
            for key in self.objects
            if key.startswith(prefix) and "/" in key[len(prefix) :]
        }
        return {"CommonPrefixes": [{"Prefix": p} for p in sorted(common)]}


def _precondition_failed() -> Exception:
    exc = RuntimeError("precondition failed")
    exc.response = {"Error": {"Code": "PreconditionFailed"}}  # type: ignore[attr-defined]
    return exc


class FakeEnvironment:
    """The container-side surface both operations touch."""

    def __init__(self, workdir: str = "/app") -> None:
        self.task_env_config = SimpleNamespace(workdir=workdir)
        self.uploads: list[tuple[str, str]] = []
        self.downloads: list[str] = []
        self.archive = b""

    async def upload_file(self, source: str, target: str) -> None:
        self.uploads.append((source, target))
        self.archive = Path(source).read_bytes()

    async def download_file(self, source: str, target) -> None:
        self.downloads.append(source)
        Path(target).write_bytes(self.archive)


def _archive(members: dict[str, bytes]) -> bytes:
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as tar:
        for name, payload in members.items():
            info = tarfile.TarInfo(name)
            info.size = len(payload)
            tar.addfile(info, io.BytesIO(payload))
    return buffer.getvalue()


class FakeAgent:
    """A `StellaAgent`-shaped stand-in for the two free functions.

    Built by hand rather than with ``StellaAgent.__new__`` where the exec
    result matters: the operations only read ``logs_dir``,
    ``_configured_value`` and ``exec_as_agent`` off the agent, which is the
    seam `ensure_git`/`run_git_baseline` established.
    """

    def __init__(self, logs_dir: Path, env: dict[str, str], stdout: str = "") -> None:
        self.logs_dir = logs_dir
        self._env = env
        self._stdout = stdout
        self.commands: list[str] = []

    def _configured_value(self, key: str, default: str | None = None) -> str | None:
        return self._env.get(key, default)

    async def exec_as_agent(self, environment, command: str, timeout_sec: int = 0):
        self.commands.append(command)
        return SimpleNamespace(stdout=self._stdout)


def _trial_tree(tmp_path: Path, task_name: str = "terminal-bench/fix-git") -> Path:
    """A Harbor trial directory: ``config.json`` beside the agent's logs dir."""
    logs_dir = tmp_path / "trial" / "agent"
    logs_dir.mkdir(parents=True)
    (tmp_path / "trial" / "config.json").write_text(
        json.dumps({"task": {"task": {"name": task_name}}}), encoding="utf-8"
    )
    return logs_dir


class TestSelector:
    def test_blank_is_no_lineage(self) -> None:
        assert parse_lineage_selector(None) is None
        assert parse_lineage_selector("") is None
        assert parse_lineage_selector("   ") is None

    def test_bare_id_takes_the_newest_generation(self) -> None:
        selector = parse_lineage_selector("self-improve.v1")
        assert selector is not None
        assert (selector.lineage_id, selector.generation) == ("self-improve.v1", None)

    def test_pinned_generation_is_addressable(self) -> None:
        selector = parse_lineage_selector("beta@7")
        assert selector is not None
        assert (selector.lineage_id, selector.generation) == ("beta", 7)
        assert selector.label() == "beta@7"

    def test_a_malformed_value_is_refused_not_ignored(self) -> None:
        """A typo that degraded to "no lineage" would produce a clean run an
        operator believes is a seeded one — the mislabelling this module
        exists to prevent."""
        with pytest.raises(LineageError):
            parse_lineage_selector("has spaces")
        with pytest.raises(LineageError):
            parse_lineage_selector("beta@latest")


class TestTaskIdentity:
    def test_package_name_is_preferred(self, tmp_path: Path) -> None:
        assert discover_task_id(_trial_tree(tmp_path)) == "terminal-bench/fix-git"

    def test_a_task_path_stands_in(self, tmp_path: Path) -> None:
        logs_dir = tmp_path / "trial" / "agent"
        logs_dir.mkdir(parents=True)
        (tmp_path / "trial" / "config.json").write_text(
            json.dumps({"task": {"path": "/data/tasks/hello-world"}}), encoding="utf-8"
        )
        assert discover_task_id(logs_dir) == "hello-world"

    def test_an_unreadable_trial_yields_nothing(self, tmp_path: Path) -> None:
        assert discover_task_id(tmp_path) is None
        assert discover_task_id(None) is None

    def test_slug_is_one_path_segment(self) -> None:
        assert task_slug("terminal-bench/fix-git") == "terminal-bench-fix-git"
        assert task_slug("") == "unknown-task"


class TestScripts:
    def test_harvest_names_every_learned_tier(self) -> None:
        script = harvest_script("/app")
        for name in ("memories", "skills", "tools", "rules", "agents"):
            assert name in script
        assert "governance.toml" in script
        assert "promotions.jsonl" in script
        # Telemetry is exported separately and must not ride this archive.
        assert "store.db" not in script
        assert "reflections.jsonl" not in script

    def test_seed_excludes_stella_before_unpacking_anything(self) -> None:
        """The exclusion has to be written first, or a task that shipped its
        own git repository (where the baseline deliberately writes none) gets
        seeded artifacts in its graded diff."""
        script = seed_script("/app")
        exclude_at = script.index("info/exclude")
        unpack_at = script.index('cp -a "$STAGE/workspace/.')
        assert exclude_at < unpack_at
        # Resolved, never assumed to be `$WD/.git`: a linked worktree has a
        # `.git` *file*, and appending to it would corrupt its gitdir pointer.
        assert "rev-parse --git-dir" in script.replace("\n", "")

    def test_scratch_never_lands_in_the_workspace(self) -> None:
        for script in (harvest_script("/app"), seed_script("/app")):
            assert "/tmp/.stella-lineage" in script

    def test_report_parsing_takes_the_last_marker(self) -> None:
        parsed = parse_lineage_report(
            "noise\nstella-lineage: state=failed detail=x\n"
            "stella-lineage: state=seeded workspace_files=4 user_files=1\n"
        )
        assert parsed == {
            "state": "seeded",
            "workspace_files": "4",
            "user_files": "1",
        }
        assert parse_lineage_report(None) == {"state": "unreported"}


class TestStore:
    def test_generations_are_dense_and_ascending(self) -> None:
        s3 = FakeS3()
        store = LineageStore(bucket="b", clients={"s3": s3})
        for _ in range(3):
            store.publish("lin", "task", b"payload", {"lineage": "lin"})
        assert store.generations("lin", "task") == [0, 1, 2]
        assert (
            store.archive_key("lin", "task", 2)
            == "lineage/lin/task/000002/artifacts.tar.gz"
        )

    def test_fetch_takes_the_newest_by_default(self) -> None:
        s3 = FakeS3()
        store = LineageStore(bucket="b", clients={"s3": s3})
        store.publish("lin", "task", b"gen0", {})
        store.publish("lin", "task", b"gen1", {})
        assert store.fetch("lin", "task", None) == (1, b"gen1")
        assert store.fetch("lin", "task", 0) == (0, b"gen0")

    def test_an_empty_lineage_fetches_nothing(self) -> None:
        store = LineageStore(bucket="b", clients={"s3": FakeS3()})
        assert store.fetch("lin", "task", None) is None

    def test_a_racing_publish_takes_the_next_number(self) -> None:
        """Two attempts of one task must not collide on a generation: the
        loser's conditional put is refused and it appends instead of
        overwriting the winner's artifacts."""
        s3 = FakeS3()
        store = LineageStore(bucket="b", clients={"s3": s3})
        store.publish("lin", "task", b"first", {})

        real_generations = store.generations
        store.generations = lambda *a, **k: []  # type: ignore[method-assign]
        try:
            generation = store.publish("lin", "task", b"second", {})
        finally:
            store.generations = real_generations  # type: ignore[method-assign]
        assert generation == 1
        assert s3.objects["lineage/lin/task/000000/artifacts.tar.gz"] == b"first"
        assert s3.objects["lineage/lin/task/000001/artifacts.tar.gz"] == b"second"

    def test_the_manifest_records_the_generation(self) -> None:
        s3 = FakeS3()
        store = LineageStore(bucket="b", clients={"s3": s3})
        store.publish(
            "lin", "task", b"x", {"lineage": "lin", "parent_generation": None}
        )
        manifest = json.loads(s3.objects["lineage/lin/task/000000/manifest.json"])
        assert manifest["generation"] == 0
        assert manifest["lineage"] == "lin"


class TestNoLineageIsUntouched:
    """The property every existing measurement depends on.

    A run with no lineage id must behave exactly as it did before this module
    existed: no container command, no upload, no download, no S3 client, no
    file on disk, and no metadata key.
    """

    def test_seed_is_inert(self, tmp_path: Path) -> None:
        agent = FakeAgent(_trial_tree(tmp_path), env={})
        environment = FakeEnvironment()
        assert asyncio.run(seed_lineage(agent, environment)) == {"state": "off"}
        assert agent.commands == []
        assert environment.uploads == []
        assert not (agent.logs_dir / "lineage").exists()

    def test_harvest_is_inert(self, tmp_path: Path) -> None:
        agent = FakeAgent(_trial_tree(tmp_path), env={})
        environment = FakeEnvironment()
        assert asyncio.run(harvest_lineage(agent, environment)) == {"state": "off"}
        assert agent.commands == []
        assert environment.downloads == []
        assert not (agent.logs_dir / "lineage").exists()

    def test_an_empty_string_is_the_same_as_unset(self, tmp_path: Path) -> None:
        agent = FakeAgent(_trial_tree(tmp_path), env={LINEAGE_ENV: "  "})
        environment = FakeEnvironment()
        assert asyncio.run(seed_lineage(agent, environment)) == {"state": "off"}
        assert agent.commands == []

    def test_no_lineage_publishes_no_metadata_key(self) -> None:
        assert disclosed_lineage({"state": "off"}) is None
        assert disclosed_lineage(None) is None
        assert disclosed_lineage({"state": "seeded"}) == {"state": "seeded"}


class TestSeed:
    def _store(self, monkeypatch, s3: FakeS3) -> None:
        monkeypatch.setattr(
            LineageStore,
            "from_lookup",
            classmethod(lambda cls, lookup: cls(bucket="b", clients={"s3": s3})),
        )

    def test_a_first_generation_is_not_a_failure(self, tmp_path, monkeypatch) -> None:
        self._store(monkeypatch, FakeS3())
        agent = FakeAgent(_trial_tree(tmp_path), env={LINEAGE_ENV: "lin"})
        report = asyncio.run(seed_lineage(agent, FakeEnvironment()))
        assert report["state"] == "first-generation"
        assert report["seeded_generation"] is None
        assert agent.commands == []

    def test_a_prior_generation_is_materialised(self, tmp_path, monkeypatch) -> None:
        s3 = FakeS3()
        self._store(monkeypatch, s3)
        LineageStore(bucket="b", clients={"s3": s3}).publish(
            "lin",
            "terminal-bench-fix-git",
            _archive({"workspace/memories/lesson.md": b"# lesson\n"}),
            {},
        )
        agent = FakeAgent(
            _trial_tree(tmp_path),
            env={LINEAGE_ENV: "lin"},
            stdout="stella-lineage: state=seeded workspace_files=1 user_files=0",
        )
        environment = FakeEnvironment()
        report = asyncio.run(seed_lineage(agent, environment))
        assert report["state"] == "seeded"
        assert report["seeded_generation"] == 0
        assert report["task_id"] == "terminal-bench/fix-git"
        assert environment.uploads and environment.uploads[0][1].startswith("/tmp/")
        assert len(agent.commands) == 1

    def test_a_seed_that_did_not_happen_refuses_the_trial(
        self, tmp_path, monkeypatch
    ) -> None:
        """Not best-effort, deliberately: a trial that ran clean under a
        seeded label is worse than a failed trial."""
        s3 = FakeS3()
        self._store(monkeypatch, s3)
        LineageStore(bucket="b", clients={"s3": s3}).publish(
            "lin", "terminal-bench-fix-git", _archive({"workspace/x": b"y"}), {}
        )
        agent = FakeAgent(
            _trial_tree(tmp_path),
            env={LINEAGE_ENV: "lin"},
            stdout="stella-lineage: state=failed detail=untar-failed",
        )
        with pytest.raises(LineageError):
            asyncio.run(seed_lineage(agent, FakeEnvironment()))

    def test_an_unidentifiable_task_refuses_the_trial(self, tmp_path) -> None:
        agent = FakeAgent(tmp_path, env={LINEAGE_ENV: "lin"})
        with pytest.raises(LineageError, match="task could not be identified"):
            asyncio.run(seed_lineage(agent, FakeEnvironment()))

    def test_a_pinned_generation_that_does_not_exist_refuses(
        self, tmp_path, monkeypatch
    ) -> None:
        self._store(monkeypatch, FakeS3())
        agent = FakeAgent(_trial_tree(tmp_path), env={LINEAGE_ENV: "lin@9"})
        with pytest.raises(LineageError, match="does not exist"):
            asyncio.run(seed_lineage(agent, FakeEnvironment()))

    def test_the_manifest_discloses_the_seed(self, tmp_path, monkeypatch) -> None:
        """Written at seed time, so a trial killed before harvest still says
        it was seeded rather than reading as a clean loss."""
        s3 = FakeS3()
        self._store(monkeypatch, s3)
        LineageStore(bucket="b", clients={"s3": s3}).publish(
            "lin", "terminal-bench-fix-git", _archive({"workspace/x": b"y"}), {}
        )
        logs_dir = _trial_tree(tmp_path)
        agent = FakeAgent(
            logs_dir,
            env={LINEAGE_ENV: "lin"},
            stdout="stella-lineage: state=seeded workspace_files=1 user_files=0",
        )
        asyncio.run(seed_lineage(agent, FakeEnvironment()))
        manifest = json.loads(
            (logs_dir / "lineage" / "manifest.json").read_text(encoding="utf-8")
        )
        assert manifest["lineage"] == "lin"
        assert manifest["seeded_generation"] == 0
        assert manifest["task_id"] == "terminal-bench/fix-git"


class TestHarvest:
    def test_a_generation_is_appended(self, tmp_path, monkeypatch) -> None:
        s3 = FakeS3()
        monkeypatch.setattr(
            LineageStore,
            "from_lookup",
            classmethod(lambda cls, lookup: cls(bucket="b", clients={"s3": s3})),
        )
        logs_dir = _trial_tree(tmp_path)
        agent = FakeAgent(
            logs_dir,
            env={LINEAGE_ENV: "lin"},
            stdout="stella-lineage: state=harvested files=3",
        )
        agent._lineage = {
            "lineage": "lin",
            "task_id": "terminal-bench/fix-git",
            "task_slug": "terminal-bench-fix-git",
            "seeded_generation": None,
            "state": "first-generation",
        }
        environment = FakeEnvironment()
        environment.archive = _archive({"workspace/memories/a.md": b"a"})
        report = asyncio.run(harvest_lineage(agent, environment))
        assert report["harvested_generation"] == 0
        assert report["harvest_state"] == "published"
        key = "lineage/lin/terminal-bench-fix-git/000000/artifacts.tar.gz"
        assert key in s3.objects
        manifest = json.loads(
            (logs_dir / "lineage" / "manifest.json").read_text(encoding="utf-8")
        )
        assert manifest["harvested_generation"] == 0

    def test_a_failed_harvest_never_escapes(self, tmp_path, monkeypatch) -> None:
        """The trial has already been graded; a harvest that fails costs the
        next generation, never this one's verdict."""

        def explode(cls, lookup):
            raise RuntimeError("no credentials")

        monkeypatch.setattr(LineageStore, "from_lookup", classmethod(explode))
        logs_dir = _trial_tree(tmp_path)
        agent = FakeAgent(
            logs_dir,
            env={LINEAGE_ENV: "lin"},
            stdout="stella-lineage: state=harvested files=1",
        )
        agent._lineage = {"task_slug": "t", "lineage": "lin"}
        report = asyncio.run(harvest_lineage(agent, FakeEnvironment()))
        assert report["harvest_state"] == "error"
        assert "no credentials" in report["harvest_error"]

    def test_an_empty_container_publishes_nothing(self, tmp_path) -> None:
        agent = FakeAgent(
            _trial_tree(tmp_path),
            env={LINEAGE_ENV: "lin"},
            stdout="stella-lineage: state=empty",
        )
        agent._lineage = {"task_slug": "t", "lineage": "lin"}
        report = asyncio.run(harvest_lineage(agent, FakeEnvironment()))
        assert report["harvest_state"] == "empty"
        assert report.get("harvested_generation") is None


class TestAdapterWiring:
    def test_both_knobs_are_registered_host_only(self) -> None:
        """The adapter's ambient check fails closed, so an unregistered name
        refuses the run rather than enabling the policy."""
        from stella_harbor import _HOST_ONLY_STELLA_ENV

        assert "STELLA_LINEAGE" in _HOST_ONLY_STELLA_ENV
        assert "STELLA_LINEAGE_BUCKET" in _HOST_ONLY_STELLA_ENV

    def test_neither_knob_reaches_the_container(self, monkeypatch) -> None:
        """Host-only means host-only: the CLI inside the container has no use
        for either, and the container environment stays as narrow as it was."""
        monkeypatch.setenv("STELLA_LINEAGE", "lin")
        monkeypatch.setenv("STELLA_LINEAGE_BUCKET", "some-bucket")
        agent = StellaAgent.__new__(StellaAgent)
        forwarded = agent._forwarded_env()
        assert "STELLA_LINEAGE" not in forwarded
        assert "STELLA_LINEAGE_BUCKET" not in forwarded
