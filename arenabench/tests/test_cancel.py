# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Cancelling a run — and, above all, cancelling *only* it.

This is the first code in ArenaBench that destroys someone else's work if it
is wrong, so the important test in this file is not the happy path. It is
:meth:`TestOnlyThisRunsJobs.test_a_neighbouring_runs_jobs_are_never_touched`:
a fake Batch holding two runs' jobs, a ``cancel_run`` for one of them, and an
assertion naming *exactly* which ids were signalled. ``scripts/arena-kill.sh``
records what the other shape costs — it selected servers by argv pattern
machine-wide and killed a neighbour's on 2026-08-08 — and the queues here are
worse, because ``arenabench-measure`` is shared by every run and every
operator in the account.

Everything else is hermetic in the ordinary way: the selection is a pure
function tested with dicts, and the AWS half is a recorded-call fake.
"""

from __future__ import annotations

from arenabench.cancel import CancelPlan, abort_record, cancel_run, plan_cancellation


class TestPlanCancellation:
    def test_queued_jobs_are_cancelled_and_running_jobs_terminated(self) -> None:
        """Batch's own split: ``cancel_job`` only works before a job starts,
        so a RUNNING trial needs ``terminate_job`` or it keeps billing."""
        plan = plan_cancellation(
            ["a", "b", "c", "d", "e"],
            {
                "a": "SUBMITTED",
                "b": "RUNNABLE",
                "c": "STARTING",
                "d": "RUNNING",
                "e": "PENDING",
            },
        )
        assert plan.cancel == ("a", "b", "e")
        assert plan.terminate == ("c", "d")
        assert plan.already_settled == ()

    def test_a_settled_job_is_left_alone(self) -> None:
        plan = plan_cancellation(["a", "b"], {"a": "SUCCEEDED", "b": "FAILED"})
        assert plan.already_settled == ("a", "b")
        assert plan.cancel == ()
        assert plan.terminate == ()

    def test_an_id_batch_did_not_describe_is_cancelled_anyway(self) -> None:
        """We own the id — it is in this run's own ``jobs.json`` — and
        ``cancel_job`` against a job that already settled is a documented
        no-op. Declining to act on a job we own is the expensive direction."""
        plan = plan_cancellation(["a"], {})
        assert plan.cancel == ("a",)

    def test_the_plan_preserves_the_submission_order(self) -> None:
        plan = plan_cancellation(
            ["c", "a", "b"], {"c": "RUNNABLE", "a": "RUNNABLE", "b": "RUNNABLE"}
        )
        assert plan.cancel == ("c", "a", "b")


class FakeBatchCanceller:
    """Records every ``cancel_job``/``terminate_job``; can be told to raise."""

    def __init__(self, states: dict[str, str], *, raise_on: set[str] | None = None) -> None:
        self.states = dict(states)
        self.raise_on = set(raise_on or ())
        self.cancelled: list[str] = []
        self.terminated: list[str] = []

    def describe_jobs(self, *, jobs: list[str]) -> dict:
        return {
            "jobs": [
                {"jobId": j, "status": self.states[j]}
                for j in jobs
                if j in self.states
            ]
        }

    def cancel_job(self, *, jobId: str, reason: str) -> dict:  # noqa: N803 - the SDK's spelling
        if jobId in self.raise_on:
            raise RuntimeError(f"ClientError: cannot cancel {jobId}")
        self.cancelled.append(jobId)
        return {}

    def terminate_job(self, *, jobId: str, reason: str) -> dict:  # noqa: N803
        if jobId in self.raise_on:
            raise RuntimeError(f"ClientError: cannot terminate {jobId}")
        self.terminated.append(jobId)
        return {}


class FakeExecutor:
    """``load_jobs``/``job_states``/``_client`` — the three seams cancel uses."""

    def __init__(self, record: dict, batch: FakeBatchCanceller) -> None:
        self.record = record
        self.batch = batch

    def load_jobs(self, run_id: str) -> dict:
        assert run_id == self.record["run_id"]
        return self.record

    def job_states(self, job_ids) -> dict[str, str]:
        ids = list(job_ids)
        return {
            job["jobId"]: job["status"]
            for job in self.batch.describe_jobs(jobs=ids)["jobs"]
        }

    def _client(self, service: str):
        assert service == "batch"
        return self.batch


#: Run A owns these three; run B's are in the same queue and must survive.
RUN_A = {
    "run_id": "r-aaaa",
    "queue": "arenabench-measure",
    "jobs": [
        {"id": "a-001", "label": "000-stella-task-a"},
        {"id": "a-002", "label": "001-cc-task-a"},
        {"id": "a-003", "label": "002-stella-task-b"},
    ],
}
RUN_B_IDS = ["b-001", "b-002"]

ALL_STATES = {
    "a-001": "SUCCEEDED",
    "a-002": "RUNNING",
    "a-003": "RUNNABLE",
    "b-001": "RUNNING",
    "b-002": "RUNNABLE",
}


class TestOnlyThisRunsJobs:
    def test_a_neighbouring_runs_jobs_are_never_touched(self, capsys) -> None:
        """THE test in this file. The selector is this run's own
        ``jobs.json`` — never a queue sweep, never a job-name prefix — because
        ``arenabench-measure`` is shared by every run and every operator in
        the account, and a queue-scoped cancel kills strangers' work
        (``scripts/arena-kill.sh``'s 2026-08-08 incident, one layer up)."""
        batch = FakeBatchCanceller(ALL_STATES)
        executor = FakeExecutor(RUN_A, batch)

        plan = cancel_run(executor, "r-aaaa", reason="banned behavior B-001")

        assert batch.terminated == ["a-002"]
        assert batch.cancelled == ["a-003"]
        assert plan.already_settled == ("a-001",)
        touched = set(batch.cancelled) | set(batch.terminated)
        assert touched.isdisjoint(RUN_B_IDS), (
            f"run B's jobs were signalled: {sorted(touched & set(RUN_B_IDS))}"
        )

    def test_one_failed_call_does_not_abandon_the_rest(self, capsys) -> None:
        """A partial cancel is the worst outcome available: the operator
        believes the run is stopped and it keeps billing."""
        batch = FakeBatchCanceller(
            {"a-001": "RUNNABLE", "a-002": "RUNNABLE", "a-003": "RUNNING"},
            raise_on={"a-001"},
        )
        executor = FakeExecutor(RUN_A, batch)

        cancel_run(executor, "r-aaaa", reason="banned behavior B-001")

        assert batch.cancelled == ["a-002"]
        assert batch.terminated == ["a-003"]
        out = capsys.readouterr().out
        assert "a-001" in out, "the failed call is reported, never swallowed"

    def test_the_reason_reaches_batch(self) -> None:
        seen: list[str] = []

        class Recording(FakeBatchCanceller):
            def cancel_job(self, *, jobId: str, reason: str) -> dict:  # noqa: N803
                seen.append(reason)
                return super().cancel_job(jobId=jobId, reason=reason)

        batch = Recording({"a-001": "RUNNABLE", "a-002": "RUNNABLE", "a-003": "RUNNABLE"})
        cancel_run(FakeExecutor(RUN_A, batch), "r-aaaa", reason="banned: B-001")
        assert seen == ["banned: B-001"] * 3


class TestAbortRecord:
    def test_it_carries_everything_a_dispute_needs(self) -> None:
        """The record is the artifact somebody argues with. If it cannot
        support 'this entry was wrong', it is not evidence — so it names the
        run, the codes, the trial that tripped it, the gate's full report, and
        every job id that was signalled."""
        import json

        plan = CancelPlan(
            cancel=("a-003",), terminate=("a-002",), already_settled=("a-001",)
        )
        record = abort_record(
            run_id="r-aaaa",
            codes=("B-001", "B-014"),
            trial_label="001-cc-task-a",
            report="B-001 at step 4: ...",
            plan=plan,
        )
        assert record["run_id"] == "r-aaaa"
        assert record["codes"] == ["B-001", "B-014"]
        assert record["trial"] == "001-cc-task-a"
        assert record["report"] == "B-001 at step 4: ..."
        assert record["cancelled"] == ["a-003"]
        assert record["terminated"] == ["a-002"]
        assert record["already_settled"] == ["a-001"]
        json.dumps(record)  # must round-trip; it is written to disk

    def test_it_is_stamped_so_two_aborts_can_be_told_apart(self) -> None:
        record = abort_record(
            run_id="r-aaaa",
            codes=(),
            trial_label="000-stella-task-a",
            report="",
            plan=CancelPlan(cancel=(), terminate=(), already_settled=()),
        )
        assert record["aborted_at"]
