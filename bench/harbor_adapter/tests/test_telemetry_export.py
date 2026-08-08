"""Exporting the container's ``.stella/private/`` telemetry per trial.

A trial's container is deleted with everything Stella recorded inside it —
the SQLite store ``stella observe``/``stella usage sync`` read and the
reflections skill mining feeds on — so bench runs contributed nothing to
local telemetry. These tests pin the export contract: private files land
under the trial's ``agent/telemetry/``, sidecars ride along, and no failure
in the export can fail a graded trial.
"""

from __future__ import annotations

import asyncio
from pathlib import Path
from types import SimpleNamespace

import pytest

pytest.importorskip("harbor", reason="Harbor is required to import the adapter")

from stella_harbor import StellaAgent  # noqa: E402


class _FakeEnvironment:
    """The two file APIs the export touches, backed by a dict of files."""

    def __init__(self, files: dict[str, bytes], workdir: str = "/app") -> None:
        self._files = files
        self.task_env_config = SimpleNamespace(workdir=workdir)
        self.downloads: list[str] = []

    async def is_file(self, path: str) -> bool:
        return path in self._files

    async def download_file(self, source: str, target: Path | str) -> None:
        self.downloads.append(source)
        Path(target).write_bytes(self._files[source])


def _agent(tmp_path: Path) -> StellaAgent:
    agent = StellaAgent.__new__(StellaAgent)
    agent.logs_dir = tmp_path
    return agent


class TestPrivateTelemetryExport:
    def test_private_files_and_sidecars_land_under_telemetry(
        self, tmp_path: Path
    ) -> None:
        environment = _FakeEnvironment(
            {
                "/app/.stella/private/store.db": b"sqlite",
                "/app/.stella/private/store.db-wal": b"wal",
                "/app/.stella/private/reflections.jsonl": b"{}\n",
            }
        )
        asyncio.run(_agent(tmp_path)._export_private_telemetry(environment))
        telemetry = tmp_path / "telemetry"
        assert (telemetry / "store.db").read_bytes() == b"sqlite"
        assert (telemetry / "store.db-wal").read_bytes() == b"wal"
        assert (telemetry / "reflections.jsonl").read_bytes() == b"{}\n"
        assert not (telemetry / "context.db").exists()

    def test_a_failing_download_never_escapes(self, tmp_path: Path) -> None:
        """The export is best-effort by contract — a graded trial must not
        be failed over telemetry, and one bad file must not stop the rest."""

        environment = _FakeEnvironment(
            {
                "/app/.stella/private/store.db": b"sqlite",
                "/app/.stella/private/reflections.jsonl": b"{}\n",
            }
        )
        original = environment.download_file

        async def flaky(source: str, target: Path | str) -> None:
            if source.endswith("store.db"):
                raise OSError("permission denied")
            await original(source, target)

        environment.download_file = flaky  # type: ignore[method-assign]
        asyncio.run(_agent(tmp_path)._export_private_telemetry(environment))
        assert (tmp_path / "telemetry" / "reflections.jsonl").exists()
        assert not (tmp_path / "telemetry" / "store.db").exists()

    def test_sync_harbor_file_apis_are_absorbed(self, tmp_path: Path) -> None:
        """Harbor environments differ on whether file helpers are async; the
        export must work against a synchronous pair too."""

        class _SyncEnvironment(_FakeEnvironment):
            def is_file(self, path: str) -> bool:  # type: ignore[override]
                return path in self._files

            def download_file(self, source, target):  # type: ignore[override]
                self.downloads.append(source)
                Path(target).write_bytes(self._files[source])

        environment = _SyncEnvironment({"/app/.stella/private/context.db": b"ctx"})
        asyncio.run(_agent(tmp_path)._export_private_telemetry(environment))
        assert (tmp_path / "telemetry" / "context.db").read_bytes() == b"ctx"

    def test_no_logs_dir_is_a_quiet_no_op(self) -> None:
        agent = StellaAgent.__new__(StellaAgent)
        environment = _FakeEnvironment({"/app/.stella/private/store.db": b"x"})
        asyncio.run(agent._export_private_telemetry(environment))
        assert environment.downloads == []
