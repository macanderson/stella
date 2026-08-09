# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The ``watch`` line ``arenabench run`` prints after starting a match.

Witness for a regression that broke the primary command outright: the line was
inlined in ``_cmd_run``, read a ``Match.id`` that does not exist, and pointed at
a ``/matches/<id>`` path the static client does not route. Because it sat
*after* ``runner.start``, every ``run`` launched its trials and then died — the
containers came up, harbor kept going unsupervised, and no ``--results``
snapshot was ever written.
"""

from __future__ import annotations

from arenabench.cli import _watch_line
from arenabench.runner import Match


def test_watch_url_carries_the_id_as_a_query_parameter() -> None:
    """The client routes a match from ``?match=``, never a path segment.

    The export publishes only ``/``, ``/_not-found`` and ``/transcript``, so a
    ``/matches/<id>`` URL falls through to static file serving and 404s. This
    pins the shape an operator can actually paste into a browser.
    """
    assert _watch_line("940485cdd079", 8900) == (
        "http://127.0.0.1:8900/?match=940485cdd079"
    )


def test_watch_line_without_a_running_arena_still_names_the_match() -> None:
    """No arena is not an error — the id is what the operator needs next."""
    line = _watch_line("940485cdd079", None)
    assert "arenabench serve" in line
    assert "940485cdd079" in line


def test_match_defines_no_id_so_the_line_must_read_spec_id() -> None:
    """The invariant that forced ``match.spec.id`` at the call site.

    ``Match`` holds its :class:`MatchSpec` rather than copying the identity out
    of it, which is why the server's match list emits ``match.spec.id``. Asserted
    on the class so this costs no construction: if someone later adds a
    ``Match.id``, this fails and the two readers get looked at together rather
    than drifting apart again.
    """
    assert not hasattr(Match, "id")
