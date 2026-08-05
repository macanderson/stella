# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh
"""Host-side artifact writes: non-destructive, and honest about loss.

The events file reaches ``logs_dir`` by two racing paths — Stella's
in-container sink (downloaded by Harbor, often root-owned) and the adapter's
host-side ``_write_log``. The 2026-07-31 run printed ``could not write
stella-events.jsonl: [Errno 13]`` eleven times while losing zero bytes: the
error and actual data loss were *anti-correlated*, which trains a reader to
ignore the one line that would ever matter.

These tests pin the repaired contract for both race orderings:

- container copy lands first → the host write backs off silently;
- host write lands first → it succeeds (and a later identical call is a
  silent no-op, never an overwrite);
- a write that fails while a copy exists (the race lost mid-flight) is
  silent;
- a write that fails with NO copy on disk — the only genuine telemetry loss —
  is loud and carries the greppable ``TELEMETRY-INCOMPLETE`` marker that run
  validation (PROMPT-3 step 5) searches for.
"""

from __future__ import annotations

from pathlib import Path

import pytest

pytest.importorskip("harbor", reason="harbor 0.6.x is required for adapter tests")

from stella_harbor import (  # noqa: E402 - after importorskip by design
    _STREAM_EVENTS_NAME,
    _TELEMETRY_INCOMPLETE,
    StellaAgent,
)


def _agent_with_logs(tmp_path: Path) -> StellaAgent:
    agent = StellaAgent.__new__(StellaAgent)
    agent.logs_dir = tmp_path
    return agent


def test_absent_file_is_written(tmp_path: Path, capsys: pytest.CaptureFixture) -> None:
    agent = _agent_with_logs(tmp_path)
    agent._write_log(_STREAM_EVENTS_NAME, '{"type":"complete"}\n')
    assert (tmp_path / _STREAM_EVENTS_NAME).read_text() == '{"type":"complete"}\n'
    assert capsys.readouterr().err == ""


def test_existing_file_is_never_overwritten_and_never_alarmed(
    tmp_path: Path, capsys: pytest.CaptureFixture
) -> None:
    # Container copy landed first (ordering 1). The host-side reconstruction
    # must neither replace it nor print anything scary about it.
    target = tmp_path / _STREAM_EVENTS_NAME
    target.write_text("container copy — authoritative\n")
    agent = _agent_with_logs(tmp_path)

    agent._write_log(_STREAM_EVENTS_NAME, "host reconstruction — different\n")

    assert target.read_text() == "container copy — authoritative\n"
    assert capsys.readouterr().err == ""


def test_lost_race_is_silent(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture,
) -> None:
    # Ordering 2, lost mid-flight: the existence check sees nothing, the
    # container download lands root-owned, and the host write then fails.
    # The artifact is present — nothing was lost, so nothing is printed.
    target = tmp_path / _STREAM_EVENTS_NAME
    real_write_text = Path.write_text

    def download_wins(self: Path, content: str, **kwargs: object) -> int:
        if self == target:
            real_write_text(self, "container copy landed mid-race\n")
            raise PermissionError(13, "Permission denied", str(self))
        return real_write_text(self, content, **kwargs)

    monkeypatch.setattr(Path, "write_text", download_wins)
    agent = _agent_with_logs(tmp_path)

    agent._write_log(_STREAM_EVENTS_NAME, "host reconstruction\n")

    monkeypatch.undo()
    assert target.read_text() == "container copy landed mid-race\n"
    assert capsys.readouterr().err == ""


def test_genuine_loss_is_loud_and_greppable(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture,
) -> None:
    # The only real telemetry loss: the write fails AND no copy exists.
    target = tmp_path / _STREAM_EVENTS_NAME

    def always_denied(self: Path, content: str, **kwargs: object) -> int:
        raise PermissionError(13, "Permission denied", str(self))

    monkeypatch.setattr(Path, "write_text", always_denied)
    agent = _agent_with_logs(tmp_path)

    agent._write_log(_STREAM_EVENTS_NAME, "events that now exist nowhere\n")

    monkeypatch.undo()
    assert not target.exists()
    err = capsys.readouterr().err
    assert _TELEMETRY_INCOMPLETE in err
    assert _STREAM_EVENTS_NAME in err
    # The old cry-wolf wording must not come back: validation greps must be
    # able to treat any occurrence of the marker as a real loss.
    assert "could not write" not in err


def test_empty_content_writes_nothing(tmp_path: Path) -> None:
    agent = _agent_with_logs(tmp_path)
    agent._write_log(_STREAM_EVENTS_NAME, None)
    agent._write_log(_STREAM_EVENTS_NAME, "")
    assert not (tmp_path / _STREAM_EVENTS_NAME).exists()


def test_run_instruction_binds_after_double_dash(monkeypatch: pytest.MonkeyPatch) -> None:
    # A task instruction that begins with a dash must reach `stella run` as
    # the positional, not parse as a flag: one 2026-07-31 trial
    # (pytorch-model-recovery) exited 2 before the agent started because its
    # instruction opened with "- ".
    monkeypatch.delenv("STELLA_BUDGET", raising=False)
    monkeypatch.delenv("STELLA_MODEL", raising=False)
    monkeypatch.delenv("STELLA_BASE_URL", raising=False)
    agent = StellaAgent.__new__(StellaAgent)
    agent.model_name = "openrouter/z-ai/glm-5.2"

    cmd = agent._build_command("- start with a dash")

    run_at = cmd.index("run")
    tail = ["run", "--output-format", "stream-json", "--", "- start with a dash"]
    assert cmd[run_at:] == tail
