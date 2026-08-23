#!/usr/bin/env python3
"""wirelog — capture the raw JSON an agent actually puts on the wire.

Both arms of a head-to-head talk HTTPS to a vendor, and neither writes what it
sent anywhere an operator can read. That gap is why a 20-task panel could
establish *that* Stella emits 1.94x Claude Code's output tokens but not
*whether* the two `low` efforts resolve to the same thinking budget: the
`reasoning` / `thinking` / `max_tokens` fields are only ever visible on the
request, and nothing recorded the request.

This is a transparent forwarding proxy. It does not parse, rewrite, or
validate anything: it copies the request to the upstream byte for byte,
streams the response back as it arrives, and writes both to a JSONL file on
the side. Latency cost is one local hop; it is paid by **every** arm pointed
at it, so a comparison stays like-for-like.

Run one listener per upstream:

    python3 wirelog.py --port 8788 --upstream https://openrouter.ai \\
        --label stella --out ~/.arenabench/wire/stella.jsonl
    python3 wirelog.py --port 8789 --upstream https://api.anthropic.com \\
        --label claude-code --out ~/.arenabench/wire/cc.jsonl

Then point the agent at `http://host.docker.internal:<port>` (from a
container) or `http://127.0.0.1:<port>` (from the host).

**Credentials are forwarded but never logged.** Every header on
`REDACTED_HEADERS` is written as `"<redacted>"`. The bodies are logged in
full, because the request body is the entire point and an Anthropic/OpenAI
request body carries no credential.

One JSON object per exchange, one line each:

    {"ts", "label", "method", "path", "status", "duration_ms", "ttfb_ms",
     "request_headers", "response_headers", "request_body", "response_body",
     "response_sse_events"}

A streaming (SSE) response is captured twice: `response_body` holds the raw
`text/event-stream` bytes, and `response_sse_events` holds the decoded `data:`
payloads in order, which is the form worth reading.

**A stalled upstream fails fast and loudly (#3754).** A single connection
used to carry one 1800s socket timeout for its entire life, so an upstream
that returned a 200 status and then never wrote a body byte held the
exchange — and a trial's whole budget — for up to that long; two such stalls
silently contaminated a paid benchmark match before anyone read the raw
transcript. `--idle-timeout` (default `DEFAULT_IDLE_TIMEOUT_S`) now bounds
*each* read: it re-arms on every byte, so a slow-but-progressing stream is
unaffected and only a genuine gap trips it. `--total-timeout` (default
`DEFAULT_TOTAL_TIMEOUT_S`) is the independent ceiling a trickle of chunks
just under the idle timeout apart would otherwise dodge. `ttfb_ms` records
time-to-response-headers separately from `duration_ms`, and `response_headers`
carries the upstream's own headers, so a stall can be attributed after the
fact: fast headers with a long duration means the body stalled, not the
connection. At exit, `print_stall_summary` writes one line to stderr —
exchange count, how many ran past `--stall-threshold`, how many returned an
HTTP 200 with zero bytes — so an operator learns about contamination before
analysing results, not after.
"""

from __future__ import annotations

import argparse
import atexit
import http.client
import json
import ssl
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit

#: Seconds a single upstream read may go without producing a byte before the
#: exchange is aborted. Short by design: re-arms on every byte received, so
#: only a genuine gap trips it — a stream that keeps sending small chunks,
#: however slowly overall, is unaffected.
DEFAULT_IDLE_TIMEOUT_S = 60.0

#: Ceiling on one exchange's total wall-clock, independent of the idle
#: timeout above: a trickle of chunks each arriving just under the idle
#: timeout apart would otherwise never trip it. Generous, because a
#: genuinely slow-but-progressing stream (long reasoning before the first
#: token) must not be killed by it.
DEFAULT_TOTAL_TIMEOUT_S = 1800.0

#: Duration past which an exchange counts toward the end-of-run stall
#: summary. Reporting only — independent of the two enforcement timeouts
#: above, and matches the threshold #3754's own measurement used to flag the
#: three contaminating calls.
DEFAULT_STALL_THRESHOLD_S = 60.0

#: Headers whose values are credentials. Forwarded upstream, never written.
REDACTED_HEADERS = {
    "authorization",
    "x-api-key",
    "api-key",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "openai-api-key",
    "anthropic-auth-token",
}

#: Hop-by-hop headers a proxy must not relay verbatim.
HOP_BY_HOP = {
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
}

_write_lock = threading.Lock()

_stats_lock = threading.Lock()
#: Process-wide exchange counters feeding `print_stall_summary` (#3754).
#: Mutated only through `_record_stat`, under `_stats_lock`.
_stats = {"total": 0, "stalled": 0, "zero_stream": 0}


def _record_stat(duration_ms: int, empty_stream: bool, stall_threshold_s: float) -> None:
    """Fold one completed exchange into the process-wide stall counters."""
    with _stats_lock:
        _stats["total"] += 1
        if duration_ms > stall_threshold_s * 1000:
            _stats["stalled"] += 1
        if empty_stream:
            _stats["zero_stream"] += 1


def print_stall_summary(stall_threshold_s: float = DEFAULT_STALL_THRESHOLD_S) -> str | None:
    """Print the end-of-run stall digest to stderr and return the line.

    Registered as an `atexit` hook in `main()` so it fires on a normal exit,
    `KeyboardInterrupt`, or a signal-driven shutdown alike — the operator
    must learn about contamination before analysing results, not after
    (#3754). Returns `None`, printing nothing, if no exchange was proxied.
    """
    with _stats_lock:
        stats = dict(_stats)
    if stats["total"] == 0:
        return None
    line = (
        f"wirelog: {stats['total']} exchanges, {stats['stalled']} over "
        f"{stall_threshold_s:.0f}s, {stats['zero_stream']} returned an "
        f"empty stream (HTTP 200, 0 bytes)"
    )
    print(line, file=sys.stderr)
    return line


def _redact(headers) -> dict[str, str]:
    return {
        k: ("<redacted>" if k.lower() in REDACTED_HEADERS else v)
        for k, v in headers.items()
    }


def _decode_sse(raw: bytes) -> list:
    """Decode an `text/event-stream` body into its ordered `data:` payloads."""
    events = []
    for line in raw.split(b"\n"):
        line = line.strip()
        if not line.startswith(b"data:"):
            continue
        payload = line[5:].strip()
        if not payload or payload == b"[DONE]":
            continue
        try:
            events.append(json.loads(payload))
        except Exception:
            events.append({"_unparsed": payload.decode("utf-8", "replace")})
    return events


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    # Set by main().
    upstream_host: str = ""
    upstream_port: int = 443
    upstream_tls: bool = True
    label: str = ""
    out_path: Path | None = None
    idle_timeout: float = DEFAULT_IDLE_TIMEOUT_S
    total_timeout: float = DEFAULT_TOTAL_TIMEOUT_S
    stall_threshold: float = DEFAULT_STALL_THRESHOLD_S

    def log_message(self, fmt, *args):  # noqa: A003 - stdlib hook name
        pass  # the JSONL is the log; stderr noise would drown the run

    def _proxy(self) -> None:
        started = time.time()
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length) if length else b""

        forward = {
            k: v for k, v in self.headers.items() if k.lower() not in HOP_BY_HOP
        }
        forward["Host"] = self.upstream_host
        forward["Accept-Encoding"] = "identity"  # log readable bytes, not gzip

        # `timeout` here is the IDLE timeout (#3754): http.client applies it
        # as the socket timeout, which governs each individual recv — it
        # re-arms on every byte, so it bounds a gap, not the exchange as a
        # whole. The independent, generous `total_timeout` ceiling is
        # enforced by hand in the streaming loop below.
        if self.upstream_tls:
            conn = http.client.HTTPSConnection(
                self.upstream_host,
                self.upstream_port,
                context=ssl.create_default_context(),
                timeout=self.idle_timeout,
            )
        else:
            conn = http.client.HTTPConnection(
                self.upstream_host, self.upstream_port, timeout=self.idle_timeout
            )

        try:
            conn.request(self.command, self.path, body=body, headers=forward)
            upstream = conn.getresponse()
        except TimeoutError as exc:
            conn.close()
            message = (
                f"wirelog idle timeout ({self.idle_timeout:.0f}s) waiting "
                f"for response headers: {exc}"
            )
            self.send_response(504)
            self.send_header("Content-Type", "text/plain")
            self.end_headers()
            self.wfile.write(message.encode())
            self._record(body, b"", 504, started, message)
            return
        except Exception as exc:
            conn.close()
            self.send_response(502)
            self.send_header("Content-Type", "text/plain")
            self.end_headers()
            self.wfile.write(f"wirelog upstream error: {exc}".encode())
            self._record(body, b"", 502, started, str(exc))
            return

        # Headers are in hand: this is the point time-to-first-byte measures
        # from `started`, distinct from the eventual `duration_ms` — a fast
        # ttfb next to a huge duration means the body stalled, not the
        # connection (the exact shape of the two contaminating stalls #3754
        # reports, both HTTP 200 with zero bytes).
        ttfb_ms = round((time.time() - started) * 1000)
        upstream_headers = dict(upstream.getheaders())

        self.send_response(upstream.status)
        for key, value in upstream.getheaders():
            if key.lower() in HOP_BY_HOP or key.lower() == "content-length":
                continue
            self.send_header(key, value)
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()

        # Stream through: the client must see bytes as the upstream produces
        # them, or a streaming agent's own timing is distorted by the proxy.
        collected = bytearray()
        stall_error = None
        try:
            while True:
                if time.time() - started > self.total_timeout:
                    stall_error = (
                        f"wirelog total timeout ({self.total_timeout:.0f}s) "
                        "exceeded — upstream kept sending within the idle "
                        "window but never finished"
                    )
                    break
                try:
                    chunk = upstream.read(8192)
                except TimeoutError as exc:
                    stall_error = (
                        f"wirelog idle timeout ({self.idle_timeout:.0f}s) "
                        f"waiting for a body byte: {exc}"
                    )
                    break
                if not chunk:
                    break
                collected.extend(chunk)
                self.wfile.write(f"{len(chunk):X}\r\n".encode())
                self.wfile.write(chunk)
                self.wfile.write(b"\r\n")
                self.wfile.flush()
            self.wfile.write(b"0\r\n\r\n")
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            conn.close()
            self._record(
                body,
                bytes(collected),
                upstream.status,
                started,
                stall_error,
                ttfb_ms=ttfb_ms,
                response_headers=upstream_headers,
            )

    def _record(
        self,
        req: bytes,
        resp: bytes,
        status: int,
        started,
        error,
        *,
        ttfb_ms: int | None = None,
        response_headers: dict[str, str] | None = None,
    ) -> None:
        duration_ms = round((time.time() - started) * 1000)
        _record_stat(duration_ms, status == 200 and len(resp) == 0, self.stall_threshold)

        if not self.out_path:
            return
        try:
            request_body = json.loads(req) if req else None
        except Exception:
            request_body = {"_unparsed": req.decode("utf-8", "replace")}

        text = resp.decode("utf-8", "replace")
        sse = _decode_sse(resp) if "data:" in text[:4096] else None
        response_body = None
        if sse is None and text:
            try:
                response_body = json.loads(text)
            except Exception:
                response_body = {"_unparsed": text}

        row = {
            "ts": time.time(),
            "label": self.label,
            "method": self.command,
            "path": self.path,
            "status": status,
            "duration_ms": duration_ms,
            "ttfb_ms": ttfb_ms,
            "request_headers": _redact(self.headers),
            "response_headers": _redact(response_headers) if response_headers else None,
            "request_body": request_body,
            "response_body": response_body,
            "response_sse_events": sse,
        }
        if error:
            row["error"] = error
        line = json.dumps(row, ensure_ascii=False)
        with _write_lock:
            with self.out_path.open("a", encoding="utf-8") as handle:
                handle.write(line + "\n")

    do_POST = _proxy
    do_GET = _proxy
    do_PUT = _proxy
    do_DELETE = _proxy
    do_PATCH = _proxy


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--upstream", required=True, help="e.g. https://api.anthropic.com")
    ap.add_argument("--label", required=True, help="which arm this listener serves")
    ap.add_argument("--out", required=True, help="JSONL transcript path")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument(
        "--idle-timeout",
        type=float,
        default=DEFAULT_IDLE_TIMEOUT_S,
        help="seconds a single upstream read may stall before the exchange "
        "is aborted (default: %(default)s)",
    )
    ap.add_argument(
        "--total-timeout",
        type=float,
        default=DEFAULT_TOTAL_TIMEOUT_S,
        help="ceiling on one exchange's total wall-clock, independent of "
        "--idle-timeout (default: %(default)s)",
    )
    ap.add_argument(
        "--stall-threshold",
        type=float,
        default=DEFAULT_STALL_THRESHOLD_S,
        help="duration past which an exchange counts toward the end-of-run "
        "stall summary (default: %(default)s)",
    )
    args = ap.parse_args()

    split = urlsplit(args.upstream)
    if split.scheme not in ("http", "https") or not split.hostname:
        print(f"bad --upstream: {args.upstream}", file=sys.stderr)
        return 2

    out = Path(args.out).expanduser()
    out.parent.mkdir(parents=True, exist_ok=True)

    Handler.upstream_host = split.hostname
    Handler.upstream_tls = split.scheme == "https"
    Handler.upstream_port = split.port or (443 if Handler.upstream_tls else 80)
    Handler.label = args.label
    Handler.out_path = out
    Handler.idle_timeout = args.idle_timeout
    Handler.total_timeout = args.total_timeout
    Handler.stall_threshold = args.stall_threshold

    # Registered before serve_forever so the digest prints on every exit path
    # — normal, Ctrl-C, or a signal-driven shutdown — not just the one this
    # function happens to catch (#3754).
    atexit.register(print_stall_summary, args.stall_threshold)

    server = ThreadingHTTPServer((args.host, args.port), Handler)
    server.daemon_threads = True
    print(
        f"wirelog[{args.label}] {args.host}:{args.port} -> {args.upstream} "
        f"logging to {out} (idle-timeout={args.idle_timeout:.0f}s, "
        f"total-timeout={args.total_timeout:.0f}s)",
        flush=True,
    )
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
