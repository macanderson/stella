# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The cross-match series rows (#2068): outcome counting, void exclusion,
and the enforced comparability key.

Built on duck-typed stand-ins rather than a full ``Match``, because the
series module deliberately reads only the members every surface already
shares — ``spec``, ``provenance``, ``metrics_for`` — and a test that
constructs the real runner would be testing the runner.
"""

from __future__ import annotations

from types import SimpleNamespace

from arenabench.series import match_row, series_payload
from arenabench.telemetry import TrialMetrics


def _trial(
    task: str,
    *,
    resolved: bool | None,
    failure: str | None = None,
    priced: float | None = 1.0,
    clock: float = 100.0,
    status: str = "done",
) -> TrialMetrics:
    m = TrialMetrics(task=task, trial=f"{task}__x")
    m.status = status
    m.resolved = resolved
    m.failure = failure or ""
    m.priced_cost = priced
    m.clock_time = clock
    return m


def _seat(
    metrics: dict[str, TrialMetrics],
    *,
    name: str = "stella",
    model: str = "anthropic/claude-sonnet-5",
    roles: dict[str, str] | None = None,
) -> SimpleNamespace:
    role_objs = {
        role: SimpleNamespace(model=pinned) for role, pinned in (roles or {}).items()
    }
    return SimpleNamespace(
        id=f"seat-{name}",
        name=name,
        agent="stella",
        engine=SimpleNamespace(model=model, roles=role_objs),
        _metrics=metrics,
    )


def _match(
    seats: list[SimpleNamespace],
    *,
    match_id: str = "m1",
    tasks: list[str] | None = None,
    sut_commit: str = "a" * 40,
    dataset_digest: str = "sha256:" + "b" * 64,
    created_at: float = 1000.0,
) -> SimpleNamespace:
    task_names = tasks if tasks is not None else sorted(seats[0]._metrics)
    spec = SimpleNamespace(
        id=match_id,
        name=match_id,
        dataset="terminal-bench-2-1",
        tasks=task_names,
        contestants=seats,
    )
    prov = SimpleNamespace(
        dataset_digest=dataset_digest,
        sut_commit=sut_commit,
        to_json=lambda: {"sut_commit": sut_commit, "dataset_digest": dataset_digest},
    )
    return SimpleNamespace(
        spec=spec,
        provenance=prov,
        status="finished",
        created_at=created_at,
        finished_at=created_at + 60.0,
        metrics_for=lambda cid, _seats=seats: next(
            s._metrics for s in _seats if s.id == cid
        ),
    )


class TestOutcomeCounting:
    def test_solved_with_incident_counts_in_both_columns_not_one(self) -> None:
        """The #2066 rule carried into the series: a trial can solve AND
        raise, and the two facts stay separate — the solve keeps the rate,
        the incident keeps the exception."""
        seat = _seat(
            {
                "crack-7z": _trial(
                    "crack-7z", resolved=True, failure="AgentTimeoutError"
                ),
                "fix-git": _trial("fix-git", resolved=True),
                "nginx": _trial("nginx", resolved=False, failure="AgentTimeoutError"),
            }
        )
        row = match_row(_match([seat]))
        outcomes = row["seats"][0]["outcomes"]
        assert outcomes["solved"] == 2
        assert outcomes["failed"] == 1
        assert outcomes["incidents"] == 2
        assert outcomes["incidents_on_solved"] == 1
        # The two timeout shapes mean opposite things and are never merged.
        assert outcomes["timeouts_after_solve"] == 1
        assert outcomes["timeouts_before_solve"] == 1

    def test_voids_are_outside_every_rate_and_visible(self) -> None:
        """An infrastructure void (judged, no verdict) is not agent
        performance: excluded from solved/failed, reported as its own
        count so three surviving trials cannot masquerade as five."""
        seat = _seat(
            {
                "a": _trial("a", resolved=True),
                "b": _trial("b", resolved=None, failure="harness setup failed"),
                "c": _trial("c", resolved=False),
            }
        )
        outcomes = match_row(_match([seat]))["seats"][0]["outcomes"]
        assert outcomes["judged"] == 3
        assert outcomes["scored"] == 2
        assert outcomes["voids"] == 1
        assert outcomes["solved"] == 1
        assert outcomes["failed"] == 1
        per_task = match_row(_match([seat]))["seats"][0]["per_task"]
        assert per_task["b"]["outcome"] == "void"

    def test_unpriced_models_poison_the_cost_never_shrink_it(self) -> None:
        seat = _seat(
            {
                "a": _trial("a", resolved=True, priced=2.0),
                "b": _trial("b", resolved=True, priced=None),
            }
        )
        outcomes = match_row(_match([seat]))["seats"][0]["outcomes"]
        assert outcomes["priced_cost"] is None
        assert outcomes["priced_cost_per_solve"] is None

    def test_unjudged_trials_do_not_count(self) -> None:
        seat = _seat(
            {
                "a": _trial("a", resolved=True),
                "b": _trial("b", resolved=None, status="running"),
            }
        )
        outcomes = match_row(_match([seat]))["seats"][0]["outcomes"]
        assert outcomes["judged"] == 1
        assert outcomes["voids"] == 0


class TestComparability:
    def test_only_the_sut_may_differ_within_a_group(self) -> None:
        """The grouping key holds everything BUT the SUT: same measurement
        on two binaries shares a key; a different model is a different
        measurement even on the same binary."""
        metrics = {"a": _trial("a", resolved=True)}
        base = _match([_seat(metrics)], match_id="m1", sut_commit="1" * 40)
        same_but_sut = _match([_seat(metrics)], match_id="m2", sut_commit="2" * 40)
        other_model = _match(
            [_seat(metrics, model="zai/glm-5.2")], match_id="m3", sut_commit="1" * 40
        )
        key = lambda m: match_row(m)["comparability"]["group_key"]  # noqa: E731
        assert key(base) == key(same_but_sut)
        assert key(base) != key(other_model)

    def test_a_pinned_role_changes_the_arm_and_the_group(self) -> None:
        """#2023: an arm with an independently pinned verifier is a
        materially different agent — never one series with the uniform arm."""
        metrics = {"a": _trial("a", resolved=True)}
        control = _match([_seat(metrics)], match_id="m1")
        treatment = _match(
            [_seat(metrics, roles={"verdict": "deepseek/deepseek-v4-pro"})],
            match_id="m2",
        )
        control_row = match_row(control)
        treatment_row = match_row(treatment)
        assert control_row["comparability"]["seats"][0]["arm"] == "control"
        assert treatment_row["comparability"]["seats"][0]["arm"] == "treatment"
        assert (
            control_row["comparability"]["group_key"]
            != treatment_row["comparability"]["group_key"]
        )

    def test_seat_order_does_not_split_a_group(self) -> None:
        """An h2h with its seats swapped is the same measurement — found
        live: the corpus held the same claude-code/stella pairing as two
        groups purely on listing order."""
        metrics = {"a": _trial("a", resolved=True)}
        forward = _match(
            [_seat(metrics, name="stella"), _seat(metrics, name="cc", model="claude-sonnet-5")],
            match_id="m1",
        )
        backward = _match(
            [_seat(metrics, name="cc", model="claude-sonnet-5"), _seat(metrics, name="stella")],
            match_id="m2",
        )
        assert (
            match_row(forward)["comparability"]["group_key"]
            == match_row(backward)["comparability"]["group_key"]
        )

    def test_task_order_does_not_split_a_group(self) -> None:
        metrics = {
            "a": _trial("a", resolved=True),
            "b": _trial("b", resolved=True),
        }
        forward = _match([_seat(metrics)], match_id="m1", tasks=["a", "b"])
        backward = _match([_seat(metrics)], match_id="m2", tasks=["b", "a"])
        assert (
            match_row(forward)["comparability"]["task_digest"]
            == match_row(backward)["comparability"]["task_digest"]
        )


class TestWholeDatasetMatches:
    def test_empty_spec_tasks_derives_the_list_from_observed_trials(self) -> None:
        """``spec.tasks == []`` means "the whole dataset" (MatchSpec's
        contract), so the row reports the tasks that actually ran — the
        union across seats, as the live grid derives it — never a zero
        count, an empty grid, and the digest of the empty list."""
        forward = _seat({"a": _trial("a", resolved=True)}, name="stella")
        backward = _seat(
            {"b": _trial("b", resolved=False)}, name="cc", model="claude-sonnet-5"
        )
        comp = match_row(_match([forward, backward], tasks=[]))["comparability"]
        assert comp["tasks"] == ["a", "b"]
        assert comp["task_count"] == 2
        assert comp["tasks_source"] == "observed"

    def test_declared_tasks_win_over_observed_and_say_so(self) -> None:
        metrics = {"a": _trial("a", resolved=True), "b": _trial("b", resolved=True)}
        comp = match_row(_match([_seat(metrics)], tasks=["a"]))["comparability"]
        assert comp["tasks"] == ["a"]
        assert comp["tasks_source"] == "spec"

    def test_unprovenanced_whole_dataset_matches_split_by_dataset_name(self) -> None:
        """No digest and no declared tasks used to leave nothing but the
        seat signature in the key, so whole-dataset matches on *different*
        datasets collapsed into one series. The registry name stands in
        when provenance never recorded a digest."""
        metrics = {"a": _trial("a", resolved=True)}
        one = _match([_seat(metrics)], match_id="m1", tasks=[])
        other = _match([_seat(metrics)], match_id="m2", tasks=[])
        for m in (one, other):
            m.provenance = None
        other.spec.dataset = "swe-bench-verified"
        key = lambda m: match_row(m)["comparability"]["group_key"]  # noqa: E731
        assert key(one) != key(other)


class TestPayloadOrdering:
    def test_rows_come_back_oldest_first(self) -> None:
        metrics = {"a": _trial("a", resolved=True)}
        newer = _match([_seat(metrics)], match_id="new", created_at=2000.0)
        older = _match([_seat(metrics)], match_id="old", created_at=1000.0)
        payload = series_payload([newer, older])
        assert [row["id"] for row in payload["matches"]] == ["old", "new"]
