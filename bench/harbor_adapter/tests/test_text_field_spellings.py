"""Both generations of the ``text`` / ``text_delta`` wire spellings parse.

Streams recorded before stella #1886 spell the two fields crossed — the
fragment rides in ``text_delta``'s ``text`` and the consolidated body in
``text``'s ``delta``. Current streams use the self-describing names
(``text_delta.delta``, ``text.text``). The adapter's readers are bilingual;
``test_adapter.py`` covers the legacy spelling throughout, and this module
witnesses the current one. In its own file because ``test_adapter.py`` is
grandfathered at its file-size ceiling and closed to growth.
"""

from __future__ import annotations

import json

import pytest

pytest.importorskip("harbor", reason="Harbor is required to import the adapter")

from harbor.models.trajectories import Trajectory  # noqa: E402

from stella_harbor import _stream_to_envelope  # noqa: E402
from stella_harbor.atif import envelope_to_trajectory  # noqa: E402


def test_stream_parser_reads_current_self_describing_text_fields() -> None:
    events = [
        {
            "type": "step_usage",
            "model": "provider/model",
            "input_tokens": 12,
            "output_tokens": 3,
            "cached_input_tokens": 0,
            "cost_usd": 0.02,
        },
        {"type": "text_delta", "delta": "Inspec"},
        {"type": "text", "text": "Inspecting now."},
        {"type": "complete", "model": "provider/model", "cost_usd": 0.02},
    ]
    envelope = _stream_to_envelope(
        "\n".join(json.dumps(event) for event in events),
        process_returned=True,
    )
    assert envelope is not None
    assert envelope["status"] == "completed"
    assert envelope["text"] == "Inspecting now."


def test_current_self_describing_text_fields_fold_identically() -> None:
    # The same stream as test_adapter.py's missing-usage case, in the
    # post-#1886 spelling. Both generations fold to the same trajectory.
    trajectory = envelope_to_trajectory(
        {
            "status": "completed",
            "model": "provider/model",
            "events": [
                {"type": "text_delta", "delta": "preview"},
                {"type": "text", "text": "authoritative"},
            ],
        },
        instruction="Answer.",
        session_id="session-unmetered",
        agent_version="unknown",
        default_model=None,
        return_code=None,
    )
    assert len(trajectory.steps) == 2
    assert trajectory.steps[1].message == "authoritative"
    Trajectory.model_validate(trajectory.to_json_dict())
