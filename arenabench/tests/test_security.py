# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The local-only trust boundary, tested as a boundary rather than an intention.

`POST /api/matches` starts subprocesses with the operator's credentials and
there is no login, so what stands between a page you are merely visiting and
your provider account is the set of checks below. They are exercised over a real
socket, because a check that is only unit-tested is a check whose real request
never met it.
"""

from __future__ import annotations

from dataclasses import replace
from pathlib import Path

import pytest

from arenabench.agents import AGENTS
from arenabench.model import Contestant, Engine, MatchSpec, screen_env
from arenabench.registry import DEFAULT_REGISTRY
from arenabench.runner import MatchRunner
from arenabench.server import allowed_hosts, serve


class TestSeatEnvironmentIsScreened:
    """A seat's env becomes the environment of a subprocess this host owns.

    On the HTTP path anyone who can POST a match writes that mapping, so it is
    attacker-shaped input arriving at `Popen`. A contestant needs credentials
    and endpoints; `PATH`, `PYTHONPATH` and `STELLA_BINARY` are how you make the
    process run something else entirely, and no contestant needs any of them.
    """

    def test_credentials_and_endpoints_pass(self):
        allowed, rejected = screen_env(
            {
                "OPENROUTER_API_KEY": "sk-1",
                "ANTHROPIC_AUTH_TOKEN": "t",
                "CLAUDE_CODE_OAUTH_TOKEN": "o",
                "ANTHROPIC_BASE_URL": "https://api.z.ai/api/anthropic",
            }
        )
        assert rejected == []
        assert len(allowed) == 4

    @pytest.mark.parametrize(
        "key",
        [
            "PATH",
            "PYTHONPATH",
            "PYTHONSTARTUP",
            "LD_PRELOAD",
            "DYLD_INSERT_LIBRARIES",
            "BASH_ENV",
            "NODE_OPTIONS",
            "DOCKER_HOST",
            "STELLA_BINARY",
            "ARENABENCH_STELLA_ADAPTER",
        ],
    )
    def test_execution_redirects_are_refused(self, key: str):
        allowed, rejected = screen_env({key: "/tmp/evil"})
        assert allowed == {}
        assert rejected == [key]

    def test_every_name_the_registry_can_ask_a_seat_for_is_allowed(self):
        """The screen and the credential registry must not drift apart: a name
        `missing_credentials` tells an operator to paste, and the screen then
        drops, is a seat that cannot authenticate for a reason nothing says."""
        from arenabench.agents import API_CREDENTIALS

        names = {name for names in API_CREDENTIALS.values() for name in names}
        for spec in AGENTS.values():
            names.update(spec.token_env)
            names.update(spec.alt_credential_env)
            if spec.base_url_env:
                names.add(spec.base_url_env)
        _, rejected = screen_env(dict.fromkeys(names, "x"))
        assert rejected == [], "the screen must not drop a name a seat is told to set"

    def test_a_pasted_env_is_screened_at_the_door_and_says_what_it_dropped(self):
        seat = Contestant.from_json(
            {
                "name": "s",
                "agent": "stella",
                "env": "ZAI_API_KEY=zk\nPATH=/evil/bin\nSTELLA_BINARY=/evil/stella",
            }
        )
        assert set(seat.env) == {"ZAI_API_KEY"}
        assert seat.ignored_env == ("PATH", "STELLA_BINARY")
        assert seat.redacted()["ignored_env_keys"] == ["PATH", "STELLA_BINARY"]

    def test_nothing_rejected_reaches_the_subprocess(self, tmp_path: Path, monkeypatch):
        """The screen at the launch line, not only at the parser: a seat built
        by any other path must not be able to hand `Popen` a `PATH`."""
        # Stubbed at the `arenabench.harbor` seam: `_launch` resolves the
        # binary and asks it for a version and a flag list before building
        # argv, and a real lookup here would let the machine's Harbor (or its
        # absence) decide whether this security assertion runs at all.
        monkeypatch.setattr(
            "arenabench.harbor.harbor_bin", lambda dataset_key=None: "/usr/bin/harbor"
        )
        monkeypatch.setattr(
            "arenabench.harbor.harbor_version", lambda binary=None: "0.20.0"
        )
        monkeypatch.setattr(
            "arenabench.harbor.supports_agent_import_path", lambda binary=None: False
        )
        seen: dict = {}

        class _Fake:
            def __init__(self, command, **kwargs):
                seen.update(kwargs)
                self.returncode = None

            def poll(self):
                return None

        monkeypatch.setattr("arenabench.runner.subprocess.Popen", _Fake)
        seat = Contestant(
            id="s",
            name="s",
            agent="stella",
            engine=Engine(api="openrouter", model="x/y"),
            env={"ZAI_API_KEY": "zk", "PATH": "/evil/bin"},
        )
        spec = MatchSpec.from_json(
            {"dataset": "terminal-bench-2.1", "tasks": ["alpha"], "contestants": []}
        )
        runner = MatchRunner(DEFAULT_REGISTRY, tmp_path / "ws")
        match = runner.create(replace(spec, contestants=(seat,)))
        run = runner._launch(match, seat)

        assert seen["env"]["ZAI_API_KEY"] == "zk"
        assert seen["env"].get("PATH") != "/evil/bin"
        assert any("PATH" in warning for warning in run.warnings), (
            "an ignored key must be visible on the seat, not silently dropped"
        )


class TestLoopbackOnly:
    """`POST /api/matches` starts subprocesses with the operator's credentials.

    There is no login and there should not be one — this is a local tool. What
    has to hold instead is that "local" is enforced: the arena refuses a
    non-loopback bind unless asked, answers only to its own address, and refuses
    a request that a page you are merely visiting could have sent.
    """

    def test_a_public_bind_is_refused_rather_than_warned_about(self, tmp_path: Path):
        with pytest.raises(ValueError):
            serve(tmp_path, host="0.0.0.0")

    def test_the_operator_can_still_ask_for_one_explicitly(self, tmp_path: Path):
        """Refusing outright would be a tool that cannot be run on a lab host;
        the point is that it takes a flag rather than a typo."""
        import threading

        stop = threading.Event()

        class _Server:
            def __init__(self, *a, **k):
                self.daemon_threads = False

            def serve_forever(self):
                stop.set()

            def shutdown(self):
                pass

        import arenabench.server as server_module

        original = server_module.ThreadingHTTPServer
        server_module.ThreadingHTTPServer = _Server  # type: ignore[misc]
        try:
            serve(tmp_path, host="0.0.0.0", port=8901, allow_remote=True)
        finally:
            server_module.ThreadingHTTPServer = original  # type: ignore[misc]
        assert stop.is_set()

    def test_only_the_bound_address_is_an_acceptable_host_header(self):
        hosts = allowed_hosts("127.0.0.1", 8900)
        assert "127.0.0.1:8900" in hosts and "localhost:8900" in hosts
        assert "arena.example.com:8900" not in hosts

    # -- live server ------------------------------------------------------

    @pytest.fixture
    def arena_http(self, tmp_path: Path):
        """A real arena on a real socket, so the checks under test are the ones
        an actual request meets."""
        import threading
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

        from arenabench.server import ArenaServer, _handler_factory

        arena = ArenaServer(tmp_path / "ws")
        httpd = ThreadingHTTPServer(("127.0.0.1", 0), BaseHTTPRequestHandler)
        port = httpd.server_address[1]
        httpd.RequestHandlerClass = _handler_factory(
            arena, allowed_hosts("127.0.0.1", port)
        )
        httpd.daemon_threads = True
        thread = threading.Thread(target=httpd.serve_forever, daemon=True)
        thread.start()
        try:
            yield arena, port
        finally:
            httpd.shutdown()
            thread.join(timeout=5)

    @staticmethod
    def _request(port: int, method: str, path: str, headers: dict, body: str = "") -> int:
        from http.client import HTTPConnection

        conn = HTTPConnection("127.0.0.1", port, timeout=5)
        try:
            conn.request(method, path, body=body, headers=headers)
            response = conn.getresponse()
            response.read()
            return response.status
        finally:
            conn.close()

    def test_a_request_to_the_arenas_own_address_is_served(self, arena_http):
        _, port = arena_http
        status = self._request(port, "GET", "/api/health", {"Host": f"127.0.0.1:{port}"})
        assert status == 200

    def test_a_host_header_naming_somewhere_else_is_refused(self, arena_http):
        """DNS rebinding in one line: a name the attacker controls resolves to
        127.0.0.1, and the browser sends *their* name in the Host header."""
        _, port = arena_http
        assert self._request(port, "GET", "/api/health", {"Host": "evil.example"}) == 403

    def test_a_cross_origin_write_never_reaches_the_runner(self, arena_http):
        arena, port = arena_http
        status = self._request(
            port,
            "POST",
            "/api/matches",
            {
                "Host": f"127.0.0.1:{port}",
                "Origin": "http://evil.example",
                "Content-Type": "application/json",
            },
            body="{}",
        )
        assert status == 403
        assert arena.runner.matches == {}

    def test_a_form_post_cannot_reach_a_write_route(self, arena_http):
        """An HTML form can send exactly three content types and none of them
        is JSON, so requiring JSON is what makes a cross-origin form useless
        even when it arrives without an Origin at all."""
        arena, port = arena_http
        status = self._request(
            port,
            "POST",
            "/api/matches",
            {
                "Host": f"127.0.0.1:{port}",
                "Content-Type": "application/x-www-form-urlencoded",
            },
            body="name=x",
        )
        assert status == 415
        assert arena.runner.matches == {}

    def test_the_dev_client_on_its_own_port_still_works(self, arena_http):
        """`npm run dev` serves the UI on :3900 and proxies /api here, passing
        its own Origin along. A rule that only accepted this exact origin would
        make the documented dev loop 403 on every write."""
        _, port = arena_http
        status = self._request(
            port,
            "POST",
            "/api/matches",
            {
                "Host": f"127.0.0.1:{port}",
                "Origin": "http://localhost:3900",
                "Content-Type": "application/json",
            },
            body="{}",
        )
        assert status == 400, "reached the handler; rejected on its contents"

    def test_the_uis_own_write_still_gets_through(self, arena_http):
        """The guard must reject the browser next door, not the operator: a
        same-origin JSON POST reaches the handler and fails on its contents."""
        _, port = arena_http
        status = self._request(
            port,
            "POST",
            "/api/matches",
            {
                "Host": f"127.0.0.1:{port}",
                "Origin": f"http://127.0.0.1:{port}",
                "Content-Type": "application/json",
            },
            body="{}",
        )
        assert status == 400
