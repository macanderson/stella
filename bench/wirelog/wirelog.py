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

    {"ts", "label", "method", "path", "status", "duration_ms",
     "request_headers", "request_body", "response_body", "response_sse_events"}

A streaming (SSE) response is captured twice: `response_body` holds the raw
`text/event-stream` bytes, and `response_sse_events` holds the decoded `data:`
payloads in order, which is the form worth reading.
"""

from __future__ import annotations

import argparse
import http.client
import json
import ssl
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit

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

        if self.upstream_tls:
            conn = http.client.HTTPSConnection(
                self.upstream_host,
                self.upstream_port,
                context=ssl.create_default_context(),
                timeout=1800,
            )
        else:
            conn = http.client.HTTPConnection(
                self.upstream_host, self.upstream_port, timeout=1800
            )

        try:
            conn.request(self.command, self.path, body=body, headers=forward)
            upstream = conn.getresponse()
        except Exception as exc:
            self.send_response(502)
            self.send_header("Content-Type", "text/plain")
            self.end_headers()
            self.wfile.write(f"wirelog upstream error: {exc}".encode())
            self._record(body, b"", 502, started, str(exc))
            return

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
        try:
            while True:
                chunk = upstream.read(8192)
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
            self._record(body, bytes(collected), upstream.status, started, None)

    def _record(self, req: bytes, resp: bytes, status: int, started, error) -> None:
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
            "duration_ms": round((time.time() - started) * 1000),
            "request_headers": _redact(self.headers),
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

    server = ThreadingHTTPServer((args.host, args.port), Handler)
    server.daemon_threads = True
    print(
        f"wirelog[{args.label}] {args.host}:{args.port} -> {args.upstream} "
        f"logging to {out}",
        flush=True,
    )
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
