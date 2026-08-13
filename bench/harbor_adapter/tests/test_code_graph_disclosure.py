"""What ``stella init`` built in the container is disclosed, or said not to be.

``_build_code_graph`` has always parsed a ``code graph:`` line out of
``stella init``'s stdout into ``self._code_graph_summary``, and **nothing has
ever read it** — one write, zero reads, and no class-level declaration, so it
existed only on instances where that exact line happened to match. Every
container computed the one fact #3087 needs and threw it away.

That is why a trial reporting ``files: 0, symbols: 0, imports: 0`` against an
``/app`` workspace with a committed, non-empty baseline could not be
diagnosed from the recorded evidence: "init never ran", "init failed", "init
ran and printed nothing", and "init ran and found nothing" all left the same
trace, which is none.

In its own file because ``test_adapter.py`` is grandfathered at its
file-size ceiling and closed to growth.
"""

from __future__ import annotations

import pytest

pytest.importorskip("harbor", reason="Harbor is required to import the adapter")

from stella_harbor.atif import envelope_to_trajectory  # noqa: E402

_ENVELOPE = {
    "status": "completed",
    "model": "provider/model",
    "events": [{"type": "text", "text": "done"}],
}


def _agent_extra(**kwargs: object) -> dict:
    trajectory = envelope_to_trajectory(
        _ENVELOPE,
        instruction="do the thing",
        session_id="s1",
        agent_version="0.0.0-test",
        default_model="provider/model",
        return_code=0,
        **kwargs,  # type: ignore[arg-type]
    )
    return trajectory.to_json_dict()["agent"]["extra"]


def test_a_trajectory_always_says_what_the_code_graph_step_did() -> None:
    """Unconditional, like ``workspace_git_baseline`` beside it.

    An omitted key reads as "this adapter is too old to say"; a present key
    saying ``not_attempted`` is an answer. The difference is the whole point
    — a reviewer must not have to infer an in-container action from a
    missing field.
    """
    extra = _agent_extra()
    assert extra["code_graph"] == {"state": "not_attempted"}


@pytest.mark.parametrize(
    "state",
    [
        # init ran and reported — the only case the old code could produce,
        # and even then it produced it into a field nobody read.
        {"state": "reported", "summary": "code graph: files 12, symbols 340"},
        # init raised. Distinct from an empty graph, and the distinction is
        # exactly what #3087 could not make.
        {"state": "unavailable", "detail": "exec failed: no such container"},
        # init ran, exited cleanly, and said nothing about a code graph.
        {"state": "no_summary_line", "detail": "`stella init` exited without a line"},
    ],
)
def test_every_code_graph_state_reaches_the_trajectory_intact(state: dict) -> None:
    assert _agent_extra(code_graph=state)["code_graph"] == state


def test_the_four_outcomes_are_all_distinguishable_from_each_other() -> None:
    """The property the defect turned on.

    Four things can happen to the code-graph step and they need four
    different traces. Before this, all four produced no trace at all, so a
    ``files: 0`` reading was consistent with every one of them — evidence
    that cannot distinguish a claim from its opposite is not evidence.
    """
    outcomes = [
        _agent_extra()["code_graph"],
        _agent_extra(code_graph={"state": "unavailable", "detail": "boom"})["code_graph"],
        _agent_extra(code_graph={"state": "no_summary_line", "detail": "quiet"})[
            "code_graph"
        ],
        _agent_extra(code_graph={"state": "reported", "summary": "code graph: files 0"})[
            "code_graph"
        ],
    ]
    rendered = [repr(sorted(outcome.items())) for outcome in outcomes]
    assert len(set(rendered)) == len(outcomes), (
        f"two outcomes of the code-graph step are indistinguishable: {rendered}"
    )
