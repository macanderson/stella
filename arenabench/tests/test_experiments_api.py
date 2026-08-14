# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The experiment read routes, against a real socket (#3227).

``test_experiments.py`` witnesses the store's helpers directly, always with
an explicit database path. That cannot see the wiring the browser actually
depends on: the ``case`` arms in ``_route_api_get``, the ``KeyError`` -> 404
conversion an unknown id rides on, and the fact that :class:`ArenaServer`
calls the helpers with *no* path — so the served code resolves the store
from ``ARENABENCH_HOME`` on a branch no helper test reaches. Same reasoning
as :mod:`tests.test_file_api`: a route that forgets to call the helper is
exactly the bug worth catching, and a unit test of the helper cannot see it.

Every test here runs against a temporary home. The experiments store is
machine-global rather than per-workspace, so a test that skipped the
isolation would read the developer's real ``~/.arenabench/experiments.db``
— documents locally, none on CI — and would be measuring the machine.
"""

from __future__ import annotations

import json
import threading
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest

from arenabench import experiments
from arenabench.server import ArenaServer, _handler_factory, allowed_hosts

#: One stored document, with a ``trials`` list the listing must not carry.
_DOCUMENT = {
    "schema": "arenabench-experiment-document/1",
    "calculation_version": "exp-demo/1",
    "experiment": {"id": "exp-001", "title": "A vs B", "status": "open"},
    "trials": [{"task": "t1", "outcome": "verified_solve"}],
}


@pytest.fixture
def served(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    """A live server whose experiments store is an empty temporary home.

    Yields the port and the home directory, so a test seeds the store by
    writing to ``home / "experiments.db"`` — the same path the request will
    resolve — before it makes its request.
    """
    home = tmp_path / "home"
    monkeypatch.setenv("ARENABENCH_HOME", str(home))

    arena = ArenaServer(tmp_path / "ws")

    httpd = ThreadingHTTPServer(("127.0.0.1", 0), BaseHTTPRequestHandler)
    port = httpd.server_address[1]
    httpd.RequestHandlerClass = _handler_factory(arena, allowed_hosts("127.0.0.1", port))
    httpd.daemon_threads = True
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    try:
        yield port, home
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


# -- the listing ------------------------------------------------------------


def test_the_listing_summarizes_each_stored_document(served):
    """Exact equality, because what the listing *omits* is the point.

    A gallery card needs the document's header and nothing else; shipping
    whole documents would grow the listing with every trial they record.
    """
    port, home = served
    experiments.store_results(_DOCUMENT, home / "experiments.db")

    status, body = _get(port, "/api/experiments")

    assert status == 200
    assert json.loads(body) == {
        "experiments": [
            {
                "id": "exp-001",
                "title": "A vs B",
                "status": "open",
                "schema": "arenabench-experiment-document/1",
                "calculation_version": "exp-demo/1",
            }
        ]
    }


def test_an_absent_store_is_an_empty_listing_that_creates_nothing(served):
    """Looking must not write.

    The route is polled by every open browser tab, so a read that created
    the database would mean merely opening the UI writes to the user's home
    directory. Witnessed here on the ``ARENABENCH_HOME`` branch the server
    actually runs, which no helper test reaches.
    """
    port, home = served

    status, body = _get(port, "/api/experiments")

    assert status == 200
    assert json.loads(body) == {"experiments": []}
    assert not (home / "experiments.db").exists()
    assert not home.exists()


# -- one document -----------------------------------------------------------


def test_the_document_route_serves_the_whole_document(served):
    port, home = served
    experiments.store_results(_DOCUMENT, home / "experiments.db")

    status, body = _get(port, "/api/experiments/exp-001")

    assert status == 200
    assert json.loads(body) == _DOCUMENT


def test_an_unknown_experiment_is_a_404(served):
    """The ``KeyError`` :meth:`ArenaServer.experiment` raises reaches the wire.

    Asserted on the body, not the status: deleting the ``case`` arm answers
    404 too — from the catch-all's ``unknown endpoint`` — so a test that
    checked only the status would still pass with the route gone, which is
    the failure mode this module exists to close.
    """
    port, home = served
    experiments.store_results(_DOCUMENT, home / "experiments.db")

    status, body = _get(port, "/api/experiments/exp-missing")

    assert status == 404
    assert "exp-missing" in json.loads(body)["error"]
