# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The arena's HTTP API and live streams.

Standard library only — no framework, no build step, no dependency to audit.
That is a deliberate constraint for a benchmark tool: the thing measuring your
agent should be small enough to read in an afternoon, and installing it should
never be the reason a comparison did not happen.

Two live channels, both Server-Sent Events over a threading HTTP server:

``/api/matches/<id>/stream``
    The scoreboard. One JSON snapshot per tick — totals, per-task grid,
    leaders. Cheap because :class:`~.telemetry.MetricsReader` re-parses a trial
    only when its file size changed.

``/api/matches/<id>/transcript/<contestant>/<task>``
    One trial's transcript, streamed as it is written. Incremental: the reader
    keeps a byte offset, so a client that connects to a match already an hour
    deep gets the backlog once and then only deltas.

**Binding.** Loopback by default and it should stay that way. Operators paste
provider credentials into this UI; those never leave the process (the API
returns key *names* only), but an arena bound to a public interface would let
anyone on the network launch runs that spend your money.
"""

from __future__ import annotations

import json
import logging
import mimetypes
import os
import random
import threading
import time
import tomllib
import uuid
from dataclasses import replace
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, Callable, Iterator
from urllib.parse import parse_qs, unquote, urlparse

from .agents import AGENTS
from .config import MatchTemplateError, dump_match, match_from_toml, required_env
from .model import EFFORTS, ROLES, MatchSpec
from .recorder import preflight as recorder_preflight
from .registry import DEFAULT_REGISTRY, Registry, sample_tasks
from .runner import MatchRunner
from .telemetry import TranscriptReader

__all__ = ["ArenaServer", "serve"]

log = logging.getLogger("arenabench.server")

WEB_ROOT = Path(__file__).resolve().parent / "web"

#: How often the scoreboard stream re-reads the run tree.
SNAPSHOT_INTERVAL_S = 1.0
#: How often a transcript stream checks for new bytes. Faster than the
#: scoreboard because this is the channel a human watches word by word.
TRANSCRIPT_INTERVAL_S = 0.25
#: Keep-alive comment cadence, so proxies and browsers do not reap an idle
#: stream during a long model call that emits nothing.
HEARTBEAT_S = 15.0


class ArenaServer:
    """Owns the registry, the runner, and the streams over both."""

    def __init__(self, workspace: Path, registry: Registry | None = None) -> None:
        self.workspace = workspace
        self.workspace.mkdir(parents=True, exist_ok=True)
        self.registry = registry or DEFAULT_REGISTRY
        self.runner = MatchRunner(self.registry, workspace)
        self.recorder_status: tuple[bool, str] = (False, "not checked")

    # -- API handlers -----------------------------------------------------

    def datasets(self) -> Any:
        return {"datasets": self.registry.to_json()}

    def tasks(self, key: str) -> Any:
        dataset = self.registry.get(key)
        if dataset is None:
            raise KeyError(key)
        tasks = self.registry.tasks(key)
        return {
            "dataset": dataset.to_json(task_count=len(tasks)),
            "tasks": [task.to_json() for task in tasks],
            "hint": (
                ""
                if tasks
                else (
                    "No tasks found on disk. Fetch the dataset first: "
                    f"`harbor download {dataset.harbor_id}`"
                )
            ),
        }

    def task_sample(
        self,
        key: str,
        count: int,
        seed: int | None,
        exclude_heavy: bool,
        max_memory_mb: int = 0,
    ) -> Any:
        """Draw a reproducible random slice of a dataset.

        Sampling lives on the server so the picker and the CLI draw with the
        *same* generator from the same pool. A browser-side shuffle would be a
        second definition of "random ten", and two definitions is one more
        than a number anybody quotes can survive.
        """
        dataset = self.registry.get(key)
        if dataset is None:
            raise KeyError(key)
        if seed is None:
            seed = random.randrange(1, 2**31)
        chosen = sample_tasks(
            self.registry.tasks(key),
            count,
            seed,
            exclude_heavy=exclude_heavy,
            max_memory_mb=max_memory_mb,
        )
        return {
            "seed": seed,
            "requested": count,
            "exclude_heavy": exclude_heavy,
            "max_memory_mb": max_memory_mb,
            "names": [task.name for task in chosen],
        }

    def agents(self) -> Any:
        return {
            "agents": [spec.to_json() for spec in AGENTS.values()],
            "efforts": list(EFFORTS),
            "roles": list(ROLES),
        }

    def create_match(self, payload: dict[str, Any]) -> Any:
        payload = dict(payload)
        payload.setdefault("id", uuid.uuid4().hex[:12])
        spec = MatchSpec.from_json(payload)
        match = self.runner.create(spec)
        if spec.record_video:
            ok, reason = recorder_preflight()
            self.recorder_status = (ok, reason)
            if not ok:
                # Never fail a match over the camera. Say so and run anyway.
                match.note = f"recording unavailable: {reason}"
                match.spec = replace(match.spec, record_video=False)
        self.runner.start(match)
        return match.snapshot()

    def list_matches(self) -> Any:
        return {
            "matches": [
                {
                    "id": match.spec.id,
                    "name": match.spec.name,
                    "status": match.status,
                    "dataset": match.spec.dataset,
                    "tasks": len(match.spec.tasks),
                    "contestants": [c.name for c in match.spec.contestants],
                    "created_at": match.created_at,
                }
                for match in sorted(
                    self.runner.matches.values(),
                    key=lambda m: m.created_at,
                    reverse=True,
                )
            ]
        }

    def match(self, match_id: str) -> Any:
        match = self.runner.matches.get(match_id)
        if match is None:
            raise KeyError(match_id)
        return match.snapshot()

    def template_for(self, match_id: str) -> str:
        """A running match rendered back out as a committable template."""
        match = self.runner.matches.get(match_id)
        if match is None:
            raise KeyError(match_id)
        return dump_match(match.spec)

    def render_template(self, payload: dict[str, Any]) -> str:
        """Render an in-progress wizard configuration as TOML.

        Lets an operator design a match in the UI and take the file away
        without running it — which is how a template gets into a repository
        in the first place.
        """
        return dump_match(MatchSpec.from_json(payload))

    def parse_template(self, text: str) -> Any:
        """Validate an uploaded template and hand the wizard back a spec.

        Returns the same JSON shape the wizard already speaks, plus the
        credential names each seat needs, so the UI can prompt for exactly
        those and nothing else.
        """
        if not text.strip():
            raise ValueError("no TOML supplied")
        try:
            data = tomllib.loads(text)
        except tomllib.TOMLDecodeError as exc:
            raise ValueError(f"not valid TOML: {exc}") from exc
        try:
            spec = match_from_toml(data)
        except MatchTemplateError as exc:
            return {"ok": False, "problems": exc.problems}
        return {
            "ok": True,
            "match": spec.to_json(),
            "required_env": required_env(spec),
        }

    def cancel(self, match_id: str) -> Any:
        match = self.runner.matches.get(match_id)
        if match is None:
            raise KeyError(match_id)
        self.runner.cancel(match)
        return {"ok": True, "status": match.status}

    # -- streams ----------------------------------------------------------

    def stream_snapshots(self, match_id: str) -> Iterator[tuple[str, Any]]:
        match = self.runner.matches.get(match_id)
        if match is None:
            raise KeyError(match_id)
        last_beat = time.time()
        while True:
            yield "snapshot", match.snapshot()
            if match.status in ("finished", "cancelled", "failed"):
                # One final snapshot after the terminal status, so a client
                # that connects late still sees the completed board, then stop.
                yield "end", {"status": match.status}
                return
            time.sleep(SNAPSHOT_INTERVAL_S)
            if time.time() - last_beat > HEARTBEAT_S:
                last_beat = time.time()
                yield "ping", {"t": last_beat}

    def stream_transcript(
        self, match_id: str, contestant_id: str, task: str
    ) -> Iterator[tuple[str, Any]]:
        match = self.runner.matches.get(match_id)
        if match is None:
            raise KeyError(match_id)
        reader = TranscriptReader()
        announced = False
        last_beat = time.time()
        while True:
            path = match.events_path(contestant_id, task)
            if path is None or not path.exists():
                if not announced:
                    yield "waiting", {"task": task, "contestant": contestant_id}
                    announced = True
            else:
                if not announced:
                    yield "waiting", {"task": task, "contestant": contestant_id}
                    announced = True
                entries = reader.read(path)
                if entries:
                    yield "entries", {"entries": entries}

            metrics = match.metrics_for(contestant_id).get(task)
            if metrics is not None and metrics.status == "done":
                # Drain whatever landed between the last read and the verdict.
                if path is not None and path.exists():
                    tail = reader.read(path)
                    if tail:
                        yield "entries", {"entries": tail}
                yield "end", metrics.to_json()
                return
            if match.status in ("finished", "cancelled", "failed"):
                yield "end", metrics.to_json() if metrics else {"status": match.status}
                return

            time.sleep(TRANSCRIPT_INTERVAL_S)
            if time.time() - last_beat > HEARTBEAT_S:
                last_beat = time.time()
                yield "ping", {"t": last_beat}

    def video(self, match_id: str, contestant_id: str, task: str) -> Path | None:
        match = self.runner.matches.get(match_id)
        if match is None:
            return None
        return match.video_path(contestant_id, task)


# --------------------------------------------------------------------------
# HTTP plumbing
# --------------------------------------------------------------------------


def _handler_factory(arena: ArenaServer) -> type[BaseHTTPRequestHandler]:
    class Handler(BaseHTTPRequestHandler):
        server_version = "ArenaBench"
        protocol_version = "HTTP/1.1"

        # -- helpers ------------------------------------------------------

        def _json(self, payload: Any, status: int = 200) -> None:
            body = json.dumps(payload).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(body)

        def _toml(self, text: str, filename: str) -> None:
            body = text.encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "application/toml; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            # `attachment` so a click downloads the file rather than rendering
            # it — this is a document you commit, not a page you read.
            self.send_header(
                "Content-Disposition", f'attachment; filename="{filename}"'
            )
            self.end_headers()
            self.wfile.write(body)

        def _error(self, status: int, message: str) -> None:
            self._json({"error": message}, status=status)

        def _sse(self, events: Callable[[], Iterator[tuple[str, Any]]]) -> None:
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream; charset=utf-8")
            self.send_header("Cache-Control", "no-cache, no-transform")
            self.send_header("Connection", "keep-alive")
            # This server is same-origin only; the header keeps a proxy from
            # buffering the stream into uselessness.
            self.send_header("X-Accel-Buffering", "no")
            self.end_headers()
            try:
                for name, payload in events():
                    chunk = f"event: {name}\ndata: {json.dumps(payload)}\n\n"
                    self.wfile.write(chunk.encode("utf-8"))
                    self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                pass  # the browser navigated away; entirely normal
            except Exception:
                log.exception("SSE stream failed")

        def _static(self, rel: str) -> None:
            if rel in ("", "/"):
                rel = "index.html"
            target = (WEB_ROOT / rel.lstrip("/")).resolve()
            try:
                target.relative_to(WEB_ROOT)
            except ValueError:
                self._error(HTTPStatus.FORBIDDEN, "path escapes the web root")
                return
            if not target.is_file():
                self._error(HTTPStatus.NOT_FOUND, f"no such asset: {rel}")
                return
            body = target.read_bytes()
            ctype = mimetypes.guess_type(target.name)[0] or "application/octet-stream"
            self.send_response(200)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-cache")
            self.end_headers()
            self.wfile.write(body)

        def _video(self, path: Path) -> None:
            """Serve an MP4 with Range support so the player can seek.

            A browser will not scrub a video the server sends whole; it needs
            206 responses. Recordings are also written *while* a match runs, so
            the size is read per request rather than cached.
            """
            size = path.stat().st_size
            start, end = 0, size - 1
            header = self.headers.get("Range", "")
            partial = header.startswith("bytes=")
            if partial:
                spec = header[len("bytes=") :].split(",")[0].strip()
                first, _, last = spec.partition("-")
                try:
                    if first:
                        start = int(first)
                        end = int(last) if last else size - 1
                    elif last:  # suffix range: last N bytes
                        start = max(0, size - int(last))
                except ValueError:
                    partial = False
            start = max(0, min(start, max(0, size - 1)))
            end = max(start, min(end, size - 1))
            length = end - start + 1

            self.send_response(
                HTTPStatus.PARTIAL_CONTENT if partial else HTTPStatus.OK
            )
            self.send_header("Content-Type", "video/mp4")
            self.send_header("Accept-Ranges", "bytes")
            self.send_header("Content-Length", str(length))
            if partial:
                self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
            self.end_headers()
            with path.open("rb") as handle:
                handle.seek(start)
                remaining = length
                while remaining > 0:
                    chunk = handle.read(min(262144, remaining))
                    if not chunk:
                        break
                    self.wfile.write(chunk)
                    remaining -= len(chunk)

        # -- routing ------------------------------------------------------

        def do_GET(self) -> None:  # noqa: N802 - http.server contract
            parsed = urlparse(self.path)
            parts = [unquote(p) for p in parsed.path.strip("/").split("/") if p]
            try:
                if not parts or parts[0] != "api":
                    self._static(parsed.path)
                    return
                self._route_api_get(parts[1:])
            except KeyError as exc:
                self._error(HTTPStatus.NOT_FOUND, f"not found: {exc}")
            except BrokenPipeError:
                pass
            except Exception as exc:
                log.exception("GET %s failed", self.path)
                self._error(HTTPStatus.INTERNAL_SERVER_ERROR, str(exc))

        def _route_api_get(self, rest: list[str]) -> None:
            match rest:
                case ["health"]:
                    self._json({"ok": True, "workspace": str(arena.workspace)})
                case ["datasets"]:
                    self._json(arena.datasets())
                case ["datasets", key, "tasks"]:
                    self._json(arena.tasks(key))
                case ["datasets", key, "sample"]:
                    query = parse_qs(urlparse(self.path).query)

                    def _int(name: str) -> int | None:
                        raw = (query.get(name) or [""])[0].strip()
                        try:
                            return int(raw)
                        except ValueError:
                            return None

                    self._json(
                        arena.task_sample(
                            key,
                            count=_int("count") or 10,
                            seed=_int("seed"),
                            exclude_heavy=(query.get("exclude_heavy") or [""])[0]
                            == "1",
                            max_memory_mb=_int("max_memory_mb") or 0,
                        )
                    )
                case ["agents"]:
                    self._json(arena.agents())
                case ["matches"]:
                    self._json(arena.list_matches())
                case ["matches", match_id]:
                    self._json(arena.match(match_id))
                case ["matches", match_id, "stream"]:
                    self._sse(lambda: arena.stream_snapshots(match_id))
                case ["matches", match_id, "transcript", contestant, task]:
                    self._sse(
                        lambda: arena.stream_transcript(match_id, contestant, task)
                    )
                case ["matches", match_id, "template.toml"]:
                    self._toml(arena.template_for(match_id), f"{match_id}.toml")
                case ["matches", match_id, "video", contestant, task]:
                    video = arena.video(match_id, contestant, task)
                    if video is None:
                        self._error(HTTPStatus.NOT_FOUND, "no recording for this trial")
                        return
                    self._video(video)
                case _:
                    self._error(HTTPStatus.NOT_FOUND, "unknown endpoint")

        def do_POST(self) -> None:  # noqa: N802 - http.server contract
            parsed = urlparse(self.path)
            parts = [unquote(p) for p in parsed.path.strip("/").split("/") if p]
            length = int(self.headers.get("Content-Length") or 0)
            raw = self.rfile.read(length) if length else b"{}"
            try:
                payload = json.loads(raw or b"{}")
            except ValueError:
                self._error(HTTPStatus.BAD_REQUEST, "body is not valid JSON")
                return
            if not isinstance(payload, dict):
                self._error(HTTPStatus.BAD_REQUEST, "body must be a JSON object")
                return
            try:
                match parts[1:]:
                    case ["matches"]:
                        self._json(arena.create_match(payload))
                    case ["templates", "parse"]:
                        self._json(arena.parse_template(payload.get("toml") or ""))
                    case ["templates", "render"]:
                        self._json({"toml": arena.render_template(payload)})
                    case ["matches", match_id, "cancel"]:
                        self._json(arena.cancel(match_id))
                    case _:
                        self._error(HTTPStatus.NOT_FOUND, "unknown endpoint")
            except KeyError as exc:
                self._error(HTTPStatus.NOT_FOUND, f"not found: {exc}")
            except ValueError as exc:
                self._error(HTTPStatus.BAD_REQUEST, str(exc))
            except Exception as exc:
                log.exception("POST %s failed", self.path)
                self._error(HTTPStatus.INTERNAL_SERVER_ERROR, str(exc))

        def log_message(self, fmt: str, *args: Any) -> None:
            log.debug("%s - %s", self.address_string(), fmt % args)

    return Handler


def serve(
    workspace: Path,
    host: str = "127.0.0.1",
    port: int = 8900,
    registry: Registry | None = None,
    open_browser: bool = False,
) -> None:
    """Run the arena until interrupted."""
    arena = ArenaServer(workspace, registry)
    handler = _handler_factory(arena)
    httpd = ThreadingHTTPServer((host, port), handler)
    httpd.daemon_threads = True
    url = f"http://{host}:{port}/"
    print(f"\n  arenabench  ->  {url}")
    print(f"  workspace   ->  {workspace}")
    if host not in ("127.0.0.1", "localhost", "::1"):
        print(
            "\n  WARNING: bound to a non-loopback interface. Anyone who can reach\n"
            "  this port can launch runs that spend your provider credits.\n"
        )
    print("  ctrl-c to stop\n")
    if open_browser:
        threading.Timer(
            0.5, lambda: __import__("webbrowser").open(url)
        ).start()
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        print("\n  stopping…")
        for match in arena.runner.matches.values():
            if match.status == "running":
                arena.runner.cancel(match)
        httpd.shutdown()


def default_workspace() -> Path:
    override = os.environ.get("ARENABENCH_HOME")
    if override:
        return Path(override).expanduser()
    return Path.home() / ".arenabench"
