# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

"""The seat a plugin's child ran at reaches the ATIF export from the trace.

Every model call the host spends for a plugin books at the one call role
``plugin``, because that vocabulary is closed and a plugin's word is not in
it. ``stella_purpose`` alone therefore says a plugin bought the call and not
which of that plugin's jobs it bought.

The word itself rides on the ``sub_agent`` bracket, as data. ``atif.py`` joins
the two by ``sub_agent_id``, so an analyzer reports the seat it was handed
rather than matching it against a list. That is what let the four-language
role-name contract retire: there is nothing left for a guard to pin.

Its own file rather than a class in ``test_adapter.py``: that file sits at its
recorded size ceiling and is closed to growth.
"""

from __future__ import annotations

import pytest

pytest.importorskip("harbor", reason="Harbor is required to import the adapter")

from harbor.models.trajectories import Trajectory  # noqa: E402

from stella_harbor.atif import envelope_to_trajectory  # noqa: E402

#: A seat name no build in this tree ships. The export must publish it
#: unchanged, which is the property a list of known role words cannot have.
UNFAMILIAR_SEAT = "grader/second-opinion"

_CHILD = "plugin:grader/second-opinion#0"


def _envelope(seat: str | None = UNFAMILIAR_SEAT) -> dict[str, object]:
    """One child's call, then the worker's own.

    ``seat=None`` is the ``delegate`` shape: a bracket with no seat key at
    all, which is also what a journal written before seats existed carries.
    """
    started: dict[str, object] = {
        "phase": "started",
        "agent_id": _CHILD,
        "instruction_preview": "does the diff drop the retry?",
        "write_access": False,
        "depth": 1,
    }
    if seat is not None:
        started["seat"] = seat
    return {
        "status": "completed",
        "cost_usd": 0.03,
        "model": "provider/model",
        "events": [
            {"type": "sub_agent", "phase": started},
            {
                "type": "step_usage",
                "step": 0,
                "role": "plugin",
                "output_text": "the retry survives",
                "model": "provider/model",
                "sub_agent_id": _CHILD,
                "input_tokens": 10,
                "output_tokens": 1,
                "cost_usd": 0.01,
            },
            {
                "type": "step_usage",
                "step": 1,
                "role": "worker",
                "model": "provider/model",
                "input_tokens": 20,
                "output_tokens": 2,
                "cost_usd": 0.02,
            },
            {"type": "text", "delta": "Done."},
        ],
    }


def _agent_steps(envelope: dict[str, object]) -> list:
    trajectory = envelope_to_trajectory(
        envelope,
        instruction="Fix it.",
        session_id="session-seats",
        agent_version="stella 0.9.345",
        default_model=None,
        return_code=0,
    )
    return Trajectory.model_validate(trajectory.to_json_dict()).steps[1:]


def test_an_unfamiliar_seat_is_published_beside_the_purpose() -> None:
    """Fails before the join existed: nothing read the bracket at all."""
    steps = _agent_steps(_envelope())
    assert [step.extra["stella_purpose"] for step in steps] == ["plugin", "execute"]
    assert steps[0].extra["stella_seat"] == UNFAMILIAR_SEAT


def test_a_call_the_engine_made_itself_carries_no_seat() -> None:
    """The worker's call names no child, so it must claim no seat."""
    steps = _agent_steps(_envelope())
    assert "stella_seat" not in steps[1].extra


def test_a_child_that_named_no_seat_publishes_none() -> None:
    """A ``delegate`` names no seat, and the export must invent none."""
    steps = _agent_steps(_envelope(seat=None))
    assert "stella_seat" not in steps[0].extra
