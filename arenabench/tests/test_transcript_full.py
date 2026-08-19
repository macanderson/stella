# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""``GET /api/matches/<id>/transcript-full/<contestant>/<task>`` — the "load
full transcript" button's route.

The live SSE channel (``/transcript/...``) caps a tool call/result body so a
match running for hours stays cheap to stream over; that cap is
unrecoverable client-side, since the elided bytes were never sent. This route
is the other half: a plain request that reads the same file from byte zero
with both caps disabled. Modelled on ``tests/test_file_api.py``'s pattern — a
real socket through ``_handler_factory``, not just the server method — because
that is where a route that forgets to disable the cap would actually be
caught.
"""

from __future__ import annotations

import json
import threading
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest

from arenabench.server import ArenaServer, _handler_factory, allowed_hosts


class _FakeMatch:
    """Just enough `Match` for the transcript routes: one trial's events file."""

    def __init__(self, events: Path) -> None:
        self._events = events

    def events_path(self, contestant_id: str, task: str) -> Path | None:
        if contestant_id == "seat" and task == "demo":
            return self._events
        return None


@pytest.fixture
def served(tmp_path: Path):
    events = tmp_path / "trial" / "agent" / "stella-events.jsonl"
    events.parent.mkdir(parents=True)

    arena = ArenaServer(tmp_path / "ws")
    arena.runner.matches["m1"] = _FakeMatch(events)  # type: ignore[assignment]

    httpd = ThreadingHTTPServer(("127.0.0.1", 0), BaseHTTPRequestHandler)
    port = httpd.server_address[1]
    httpd.RequestHandlerClass = _handler_factory(arena, allowed_hosts("127.0.0.1", port))
    httpd.daemon_threads = True
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    try:
        yield port, events
    finally:
        httpd.shutdown()
        thread.join(timeout=5)


def _get(port: int, path: str) -> tuple[int, bytes]:
    conn = HTTPConnection("127.0.0.1", port, timeout=5)
    try:
        conn.request("GET", path, headers={"Host": f"127.0.0.1:{port}"})
        res = conn.getresponse()
        return res.status, res.read()
    finally:
        conn.close()


def test_the_full_transcript_route_returns_every_entry_uncapped(served):
    port, events = served
    long_output = "line\n" * 5000
    events.write_text(
        "\n".join(
            json.dumps(e)
            for e in [
                {
                    "type": "tool_start",
                    "call": {"call_id": "c1", "name": "bash", "input": {"cmd": "x"}},
                },
                {
                    "type": "tool_result",
                    "call_id": "c1",
                    "output": {"ok": {"content": long_output}},
                    "duration_ms": 5,
                },
            ]
        )
        + "\n",
        encoding="utf-8",
    )
    status, body = _get(port, "/api/matches/m1/transcript-full/seat/demo")
    assert status == 200
    payload = json.loads(body)
    entries = payload["entries"]
    assert len(entries) == 2
    result = entries[-1]
    assert result["body"] == long_output
    assert "truncated" not in result["body"]
    assert result["meta"]["tool_class"] == "execute"


def test_a_streamed_message_is_returned_once_whole_not_once_per_fragment(served):
    """The witness for the "load full transcript" button's second defect.

    The reader coalesces `text_delta`/`reasoning` fragments into ONE growing
    entry that it re-emits under the same `seq` after every fragment. The SSE
    client honours that by keying a map on `seq`, so a re-emission replaces.
    This route returned the reader's raw output and its caller renders a list —
    which for a *finished* trial read in one pass means every prefix of every
    message is in the payload at once.

    Measured on a real archived trial before the fix: 265 entries carrying 72
    distinct `seq`s, one reasoning block repeated 83 times. The reader who paid
    extra bytes for the whole transcript got the same paragraph 83 times.
    """
    port, events = served
    events.write_text(
        "\n".join(
            json.dumps(e)
            for e in [
                {"type": "reasoning", "delta": "I will "},
                {"type": "reasoning", "delta": "read the file "},
                {"type": "reasoning", "delta": "first."},
                {
                    "type": "tool_start",
                    "call": {"call_id": "c1", "name": "bash", "input": {"cmd": "x"}},
                },
                {"type": "text_delta", "delta": "Done"},
                {"type": "text_delta", "delta": " — all tests pass."},
            ]
        )
        + "\n",
        encoding="utf-8",
    )
    status, body = _get(port, "/api/matches/m1/transcript-full/seat/demo")
    assert status == 200
    entries = json.loads(body)["entries"]

    seqs = [entry["seq"] for entry in entries]
    assert len(seqs) == len(set(seqs)), f"a seq is repeated: {seqs}"

    by_kind = {entry["kind"]: entry for entry in entries}
    # The LAST emission, which is the complete one — the accumulator only grows.
    assert by_kind["reasoning"]["body"] == "I will read the file first."
    assert by_kind["text"]["body"] == "Done — all tests pass."
    # Order is the order it happened, not the order a dict rehashed it into.
    assert [entry["kind"] for entry in entries] == ["reasoning", "tool", "text"]


def test_an_unknown_trial_returns_an_empty_transcript_not_an_error(served):
    port, _events = served
    status, body = _get(port, "/api/matches/m1/transcript-full/seat/nope")
    assert status == 200
    assert json.loads(body) == {"entries": []}
