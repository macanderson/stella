#!/usr/bin/env python3
"""Mock LLM provider endpoint for the provider/host-mismatch sandbox.

Zero dependencies (Python 3 stdlib only) so it is "pre-warmed" — no venv, no
pip. It stands in for a real provider host (OpenRouter, Z.ai, …) and does the
one thing that reproduces the bug class: it checks the *exact* bearer token it
receives against the single key it was told to accept, and logs the raw bytes
of the Authorization header it saw.

Why this reproduces the real 401:
  Stella resolves a *provider* (which picks the API key) independently from the
  --base-url it dials. If you send provider A's key to provider B's host, host
  B rejects it with 401 — even though the key is perfectly valid on host A.
  This mock is "host B": it accepts ONLY --accept-key and 401s everything else,
  exactly like Z.ai rejecting a valid OpenRouter key.

Behaviour:
  * Any POST (e.g. /v1/chat/completions) with `Authorization: Bearer <k>`:
      - k == accepted key  -> 200 + a minimal OpenAI-compatible completion
      - otherwise          -> 401 + an OpenRouter-shaped error body
  * GET /whoami -> 200, reports which key this instance accepts (for the runner)
  * Every request logs: method, path, and repr() of the Authorization header so
    an INVISIBLE trailing space / newline / quote in a key is made visible.

Usage:
  python3 mock_provider.py --port 8801 --accept-key sk-or-FAKE-openrouter \
      --provider-label openrouter-host
"""
import argparse
import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ACCEPT_KEY = ""
PROVIDER_LABEL = "mock"


class Handler(BaseHTTPRequestHandler):
    # Quiet the default noisy logging; we do our own structured line.
    def log_message(self, *args):  # noqa: D401
        pass

    def _log(self, note):
        auth = self.headers.get("Authorization")
        # repr() is deliberate: it renders a trailing '\n', ' ', or '"' that
        # would otherwise be invisible and is a classic 401 cause.
        print(
            f"[{PROVIDER_LABEL}] {self.command} {self.path} "
            f"Authorization={auth!r} -> {note}",
            flush=True,
        )

    def _send(self, code, body):
        payload = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def _bearer(self):
        raw = self.headers.get("Authorization", "")
        prefix = "Bearer "
        return raw[len(prefix):] if raw.startswith(prefix) else None

    def do_GET(self):
        if self.path == "/whoami":
            self._log("whoami")
            self._send(200, {"provider_label": PROVIDER_LABEL,
                             "accepts_key_len": len(ACCEPT_KEY)})
            return
        self._log("404")
        self._send(404, {"error": {"message": "not found", "code": 404}})

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0) or 0)
        if length:
            self.rfile.read(length)  # drain body; contents don't matter here
        key = self._bearer()
        if key == ACCEPT_KEY:
            self._log("200 OK (key matches this host)")
            self._send(200, {
                "id": "chatcmpl-mock",
                "object": "chat.completion",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "pong"},
                    "finish_reason": "stop",
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
            })
        else:
            self._log("401 UNAUTHORIZED (key does NOT match this host)")
            # Shape mirrors OpenRouter's 401 body so the adapter maps it the
            # same way it maps the real thing.
            self._send(401, {"error": {
                "message": "No auth credentials found / invalid key",
                "code": 401,
            }})


def main():
    global ACCEPT_KEY, PROVIDER_LABEL
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--accept-key", required=True,
                    help="the ONE bearer token this host accepts")
    ap.add_argument("--provider-label", default="mock")
    args = ap.parse_args()
    ACCEPT_KEY = args.accept_key
    PROVIDER_LABEL = args.provider_label
    srv = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    print(f"[{PROVIDER_LABEL}] listening on http://127.0.0.1:{args.port} "
          f"(accepts a {len(ACCEPT_KEY)}-char key)", flush=True)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        srv.server_close()
        sys.exit(0)


if __name__ == "__main__":
    main()
