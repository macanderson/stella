# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The trial file-browser endpoints, against a real socket.

These are the first routes that hand out file *contents*, so the containment
rules are exercised here through the actual HTTP stack rather than only against
the helper — a route that forgets to call the helper is exactly the bug worth
catching, and a unit test of the helper cannot see it.
"""

from __future__ import annotations

import json
import struct
import threading
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest

from arenabench.server import ArenaServer, _handler_factory, allowed_hosts


def _box(kind: bytes, payload: bytes = b"") -> bytes:
    return struct.pack(">I4s", 8 + len(payload), kind) + payload


class _FakeMatch:
    """Just enough Match for the file routes: a trial directory per task."""

    def __init__(self, trial: Path) -> None:
        self._trial = trial

    def trial_dir_for(self, contestant_id: str, task: str) -> Path | None:
        if contestant_id == "seat" and task == "demo":
            return self._trial
        return None

    def video_path(self, contestant_id: str, task: str) -> Path | None:
        from arenabench.artifacts import mp4_is_finalized

        video = self._trial / "arena" / "recording.mp4"
        return video if mp4_is_finalized(video) else None


@pytest.fixture
def served(tmp_path: Path):
    trial = tmp_path / "jobs" / "m1-seat" / "demo__abc"
    (trial / "agent").mkdir(parents=True)
    (trial / "arena").mkdir(parents=True)
    (trial / "result.json").write_text('{"reward": 1}', encoding="utf-8")
    (trial / "agent" / "events.jsonl").write_text('{"type":"text"}\n', encoding="utf-8")

    secret = tmp_path / "outside-secret.txt"
    secret.write_text("TOP SECRET", encoding="utf-8")
    (trial / "arena" / "escape").symlink_to(secret)

    arena = ArenaServer(tmp_path / "ws")
    arena.runner.matches["m1"] = _FakeMatch(trial)  # type: ignore[assignment]

    httpd = ThreadingHTTPServer(("127.0.0.1", 0), BaseHTTPRequestHandler)
    port = httpd.server_address[1]
    httpd.RequestHandlerClass = _handler_factory(arena, allowed_hosts("127.0.0.1", port))
    httpd.daemon_threads = True
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    try:
        yield port, trial
    finally:
        httpd.shutdown()
        thread.join(timeout=5)


def _get(port: int, path: str) -> tuple[int, bytes, dict[str, str]]:
    conn = HTTPConnection("127.0.0.1", port, timeout=5)
    try:
        conn.request("GET", path, headers={"Host": f"127.0.0.1:{port}"})
        res = conn.getresponse()
        return res.status, res.read(), dict(res.getheaders())
    finally:
        conn.close()


# -- the tree ---------------------------------------------------------------


def test_the_tree_lists_what_the_trial_produced(served):
    port, _ = served
    status, body, _ = _get(port, "/api/matches/m1/files/seat/demo")
    assert status == 200
    node = json.loads(body)
    names = {child["name"] for child in node["children"]}
    assert {"agent", "arena", "result.json"} <= names


def test_an_unknown_trial_is_a_404(served):
    port, _ = served
    status, _, _ = _get(port, "/api/matches/m1/files/seat/nosuchtask")
    assert status == 404


# -- reading a file ---------------------------------------------------------


def test_a_text_artifact_is_served_as_text(served):
    port, _ = served
    status, body, headers = _get(
        port, "/api/matches/m1/file/seat/demo?path=agent/events.jsonl"
    )
    assert status == 200
    assert b'"type":"text"' in body
    assert headers.get("X-Content-Type-Options") == "nosniff"


@pytest.mark.parametrize(
    "attempt",
    [
        "../../../../etc/passwd",
        "..%2F..%2Fetc%2Fpasswd",
        "/etc/passwd",
        "arena/../../../etc/passwd",
    ],
)
def test_traversal_is_refused_over_http(served, attempt):
    port, _ = served
    status, _, _ = _get(port, f"/api/matches/m1/file/seat/demo?path={attempt}")
    assert status == 404


def test_a_symlink_out_of_the_trial_is_refused_over_http(served):
    """Textually contained, and still must not be served once resolved."""
    port, _ = served
    status, body, _ = _get(port, "/api/matches/m1/file/seat/demo?path=arena/escape")
    assert status == 404
    assert b"TOP SECRET" not in body


def test_a_missing_path_parameter_is_refused(served):
    port, _ = served
    status, _, _ = _get(port, "/api/matches/m1/file/seat/demo")
    assert status == 404


# -- the recording gate -----------------------------------------------------


def test_an_unfinalised_recording_is_not_served(served):
    """A live recording is `ftyp | free | mdat(size=0)` — present, unplayable.

    Serving it produces a broken player, which reads to a user as "video is
    broken" rather than "the run has not finished".
    """
    port, trial = served
    (trial / "arena" / "recording.mp4").write_bytes(
        _box(b"ftyp", b"isom") + struct.pack(">I4s", 0, b"mdat") + b"\xde\xad" * 512
    )
    status, _, _ = _get(port, "/api/matches/m1/video/seat/demo")
    assert status == 404


def test_a_finalised_recording_is_served(served):
    port, trial = served
    (trial / "arena" / "recording.mp4").write_bytes(
        _box(b"ftyp", b"isom") + _box(b"moov", b"\x00" * 32) + _box(b"mdat", b"\x01" * 64)
    )
    status, body, _ = _get(port, "/api/matches/m1/video/seat/demo")
    assert status in (200, 206)
    assert body
