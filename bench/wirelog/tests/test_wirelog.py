"""Witness tests for #3754: a stalled upstream must fail fast and loudly.

Two shapes are covered:

- The proxy must abort an exchange whose upstream returns HTTP 200 headers
  and then never writes a body byte — the exact stall the issue's own
  measurement caught, which held a real exchange for up to 3241s under the
  old flat 1800s socket timeout. Here the fake upstream goes silent after
  20s, well past `Handler.idle_timeout`, but the test only ever waits a
  few seconds for the *client* side to see the proxy give up — on the old
  code (no idle enforcement in the streaming loop, no `TimeoutError` catch
  around `upstream.read()`) that wait times out and the test fails, which is
  the fail-on-main half of the witness.
- The transcript must carry `ttfb_ms` and `response_headers`, so a stall can
  be attributed after the fact without re-running the match.

Every fixture here binds to `127.0.0.1` on port 0 (OS-assigned) and every
background thread is a daemon, so a hung old-code run cannot wedge the test
process — the assertions time out on their own short deadlines instead of on
the wirelog defaults.
"""

from __future__ import annotations

import http.client
import json
import socket
import threading
import time
from http.server import ThreadingHTTPServer
from pathlib import Path

import pytest
import wirelog


class _StallingUpstream:
    """A bare TCP server: valid HTTP/1.1 200 headers, then silence.

    This is the "200 with an empty stream" shape #3754 reports from the real
    upstream, reproduced locally so the witness needs no network access and
    no API key.
    """

    def __init__(self, silence_s: float = 20.0) -> None:
        self._sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._sock.bind(("127.0.0.1", 0))
        self._sock.listen(1)
        self.port = self._sock.getsockname()[1]
        self._silence_s = silence_s
        self._thread = threading.Thread(target=self._serve_one, daemon=True)
        self._thread.start()

    def _serve_one(self) -> None:
        try:
            conn, _addr = self._sock.accept()
        except OSError:
            return
        try:
            conn.settimeout(5.0)
            try:
                conn.recv(65536)  # drain the request wirelog forwarded
            except OSError:
                pass
            conn.sendall(
                b"HTTP/1.1 200 OK\r\n"
                b"Content-Type: text/event-stream\r\n"
                b"X-Upstream-Marker: stalling-upstream\r\n"
                b"Transfer-Encoding: chunked\r\n"
                b"\r\n"
            )
            time.sleep(self._silence_s)  # never sends a body chunk
        except OSError:
            pass
        finally:
            conn.close()

    def close(self) -> None:
        self._sock.close()


class _PromptUpstream:
    """A bare TCP server that answers immediately with one JSON chunk."""

    def __init__(self) -> None:
        self._sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._sock.bind(("127.0.0.1", 0))
        self._sock.listen(1)
        self.port = self._sock.getsockname()[1]
        self._thread = threading.Thread(target=self._serve_one, daemon=True)
        self._thread.start()

    def _serve_one(self) -> None:
        try:
            conn, _addr = self._sock.accept()
        except OSError:
            return
        try:
            conn.settimeout(5.0)
            try:
                conn.recv(65536)
            except OSError:
                pass
            payload = b'{"ok": true}'
            conn.sendall(
                b"HTTP/1.1 200 OK\r\n"
                b"Content-Type: application/json\r\n"
                b"X-Upstream-Marker: prompt-upstream\r\n"
                b"Content-Length: " + str(len(payload)).encode() + b"\r\n"
                b"\r\n" + payload
            )
        except OSError:
            pass
        finally:
            conn.close()

    def close(self) -> None:
        self._sock.close()


def _make_proxy_server(handler_cls) -> ThreadingHTTPServer:
    server = ThreadingHTTPServer(("127.0.0.1", 0), handler_cls)
    server.daemon_threads = True
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server


@pytest.fixture(autouse=True)
def _reset_stats():
    """Isolate the module-global stall counters between tests."""
    with wirelog._stats_lock:
        wirelog._stats.update(total=0, stalled=0, zero_stream=0)
    yield
    with wirelog._stats_lock:
        wirelog._stats.update(total=0, stalled=0, zero_stream=0)


def test_idle_timeout_aborts_a_stalled_upstream_fast() -> None:
    """A 200-then-silent upstream must not hold the exchange for long."""
    upstream = _StallingUpstream(silence_s=20.0)

    class _Handler(wirelog.Handler):
        upstream_host = "127.0.0.1"
        upstream_port = upstream.port
        upstream_tls = False
        label = "test"
        out_path = None
        idle_timeout = 0.3
        total_timeout = 5.0
        stall_threshold = 1.0

    server = _make_proxy_server(_Handler)
    try:
        result: dict[str, object] = {}

        def _do_request() -> None:
            conn = http.client.HTTPConnection(
                "127.0.0.1", server.server_address[1], timeout=10
            )
            conn.request("POST", "/v1/messages", body=b"{}")
            resp = conn.getresponse()
            result["status"] = resp.status
            result["body"] = resp.read()

        client_thread = threading.Thread(target=_do_request, daemon=True)
        client_thread.start()
        client_thread.join(timeout=3.0)

        assert not client_thread.is_alive(), (
            "wirelog held the exchange past its idle timeout instead of "
            "failing fast — this is the #3754 stall"
        )
        assert result["status"] in (200, 502, 504)
    finally:
        server.shutdown()
        server.server_close()
        upstream.close()


def test_ttfb_and_response_headers_are_recorded(tmp_path: Path) -> None:
    upstream = _PromptUpstream()
    wire_path = tmp_path / "wire.jsonl"

    class _Handler(wirelog.Handler):
        upstream_host = "127.0.0.1"
        upstream_port = upstream.port
        upstream_tls = False
        label = "test"
        out_path = wire_path
        idle_timeout = 5.0
        total_timeout = 5.0
        stall_threshold = 60.0

    server = _make_proxy_server(_Handler)
    try:
        conn = http.client.HTTPConnection(
            "127.0.0.1", server.server_address[1], timeout=5
        )
        conn.request("POST", "/v1/messages", body=b"{}")
        resp = conn.getresponse()
        resp.read()
        assert resp.status == 200

        deadline = time.time() + 2.0
        while not wire_path.exists() and time.time() < deadline:
            time.sleep(0.02)

        lines = wire_path.read_text().strip().splitlines()
        assert len(lines) == 1
        row = json.loads(lines[0])
        assert isinstance(row["ttfb_ms"], int)
        assert row["ttfb_ms"] >= 0
        assert row["response_headers"]["X-Upstream-Marker"] == "prompt-upstream"
    finally:
        server.shutdown()
        server.server_close()
        upstream.close()


def test_stall_summary_counts_and_reports_empty_streams() -> None:
    assert wirelog.print_stall_summary() is None  # nothing proxied yet

    wirelog._record_stat(duration_ms=500, empty_stream=False, stall_threshold_s=60.0)
    wirelog._record_stat(duration_ms=90_000, empty_stream=True, stall_threshold_s=60.0)
    wirelog._record_stat(duration_ms=95_000, empty_stream=True, stall_threshold_s=60.0)

    line = wirelog.print_stall_summary(stall_threshold_s=60.0)
    assert line is not None
    assert "3 exchanges" in line
    assert "2 over 60s" in line
    assert "2 returned an empty stream" in line
