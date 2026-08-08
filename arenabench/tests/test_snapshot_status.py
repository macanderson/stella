# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""What the supervisor says when capture does not work.

The measurement this module guards is a negative one, which is why it needed
its own file: every assertion here is about a trial that captured *nothing*,
and the failure mode being witnessed is silence. Capture used to report a
commit that failed outright at ``debug`` and an empty ``rev-parse`` at no level
whatsoever, so "capture is broken" and "this host is slow" produced identical
artifacts — an empty ``manifest.jsonl``. Replay reads an empty manifest as
``flip_index=None`` and an operator reads that as "the agent never solved it",
so the silence could convert a solved trial into a failure in the benchmark
record (#2196).

**No Docker.** ``_exec`` and ``_container_running`` are the module's only two
doors to Docker, so stubbing exactly those leaves every line of the
supervisor's own logic under test. Deliberately not the fixed-container-name
Docker suites, which cannot run twice on one host concurrently (#2197).

**No clock.** These tests drive ``_consider`` directly instead of starting the
watcher thread. The thread is only a clock around that method, and what is
under test is what the supervisor *records*, never when — so there is nothing
here to wait on, and nothing to pace (contrast #2114, where the pacing was the
whole subject).
"""

from __future__ import annotations

import json
import logging
from pathlib import Path

from arenabench.snapshot import SnapshotSupervisor

#: Where the capture record lands. Spelled out rather than imported so this
#: file still runs — and still fails on its assertions — against a `snapshot.py`
#: that has no such constant. See `test_a_zero_capture_run_is_machine_detectable`.
CAPTURE_JSON = ("arena", "snapshots", "capture.json")


def _trial(tmp_path: Path, name: str = "demo__RpL1") -> Path:
    """A trial directory shaped the way Harbor leaves one mid-run."""
    trial = tmp_path / "jobs" / "seat" / name
    (trial / "agent").mkdir(parents=True)
    return trial


def _capture_that(monkeypatch, *, commit: tuple[int, str]) -> None:
    """Stub Docker so every capture attempt returns ``commit``.

    The workspace probe and the snapshot-repo init both succeed, so capture
    gets a clean *start* and only the capture itself fails — which is precisely
    the asymmetry that hid the bug: the start side already escalated, the
    capture side did not.
    """
    from arenabench import snapshot

    def fake_exec(container: str, script: str, *, timeout: float = 180.0) -> tuple[int, str]:
        if "rev-parse HEAD" in script:
            return commit
        if "init -q" in script:
            return 0, "ok\n"
        return 0, "/app\n"  # the workspace probe

    monkeypatch.setattr(snapshot, "_exec", fake_exec)
    monkeypatch.setattr(snapshot, "_container_running", lambda name: True)


def _supervisor(tmp_path: Path, **kwargs) -> SnapshotSupervisor:
    return SnapshotSupervisor(
        jobs_root=tmp_path / "jobs",
        jobs=["seat"],
        interval=0.0,  # every sweep is due; the cadence is not what is on trial
        poll_interval=0.0,
        **kwargs,
    )


def _record(trial: Path) -> dict:
    return json.loads(trial.joinpath(*CAPTURE_JSON).read_text(encoding="utf-8"))


# -- a failed capture is named, not whispered -------------------------------


def test_a_failed_capture_is_named_not_whispered(tmp_path, monkeypatch, caplog):
    """A capture that fails outright must reach an operator who did not ask.

    It was logged at `debug`, which in practice means nowhere: the run that
    filed #2196 produced zero snapshots in 180 s and an *empty* captured-log
    section, because nothing capture had to say was above `debug`.
    """
    trial = _trial(tmp_path)
    _capture_that(monkeypatch, commit=(1, "fatal: not a git repository\n"))
    sup = _supervisor(tmp_path)

    with caplog.at_level(logging.WARNING, logger="arenabench.snapshot"):
        sup._consider(trial)

    warnings = [r for r in caplog.records if r.levelno >= logging.WARNING]
    assert warnings, "a capture that failed outright said nothing at WARNING or above"
    said = "\n".join(r.getMessage() for r in warnings)
    assert trial.name in said, f"the warning does not name the trial: {said}"
    assert "not a git repository" in said, f"the warning drops the reason: {said}"

    # And the same fact survives the run, for a reader who has only the tree.
    record = _record(trial)
    assert record["snapshots"] == 0
    assert record["attempts"] >= 1, "an attempt that failed was not counted as an attempt"
    assert "not a git repository" in record["reason"]


def test_an_empty_sha_is_recorded_and_distinguishable(tmp_path, monkeypatch, caplog):
    """The branch that logged at no level at all, and the one it must not blur into.

    `git commit` succeeding while `git rev-parse HEAD` prints nothing is a
    different fault from the commit failing — a snapshot repository that went
    away under a live container, rather than one that never worked. Both
    produced exactly the same artifact before: an empty manifest.
    """
    trial = _trial(tmp_path)
    _capture_that(monkeypatch, commit=(0, ""))  # "succeeded", printed no sha
    sup = _supervisor(tmp_path)

    with caplog.at_level(logging.WARNING, logger="arenabench.snapshot"):
        sup._consider(trial)

    said = "\n".join(r.getMessage() for r in caplog.records if r.levelno >= logging.WARNING)
    assert said, "the empty-sha branch was silent at every level, as it always has been"
    empty_sha_reason = _record(trial)["reason"]
    assert "rev-parse" in empty_sha_reason, (
        f"the record does not say the sha was missing: {empty_sha_reason}"
    )

    # The other fault, recorded for the same shaped trial, must read differently
    # — otherwise the two are still indistinguishable after the fact.
    other = _trial(tmp_path, name="demo__RpL2")
    _capture_that(monkeypatch, commit=(1, "fatal: not a git repository\n"))
    _supervisor(tmp_path)._consider(other)
    assert _record(other)["reason"] != empty_sha_reason


def test_a_capture_that_lands_records_no_failure(tmp_path, monkeypatch):
    """The control: a working host must not be labelled broken.

    Without this, every assertion above is satisfiable by a supervisor that
    calls everything a failure — which would be its own dishonest measurement.
    """
    trial = _trial(tmp_path)
    _capture_that(monkeypatch, commit=(0, "a" * 40 + "\n"))
    sup = _supervisor(tmp_path)

    sup._consider(trial)

    record = _record(trial)
    assert record["snapshots"] == 1
    assert record["failures"] == 0
    assert record["reason"] == ""
    assert sup.unmeasured() == [], "a trial that captured was reported as unmeasured"


# -- a run that captured nothing is loud and machine-detectable -------------


def test_a_zero_capture_run_is_machine_detectable(tmp_path, monkeypatch, caplog):
    """Zero snapshots for a whole trial is a broken measurement, and must say so.

    The consequence this guards is the severe one: with no manifest, replay
    reports `flip_index=None`, which reads as "the agent never solved it".
    A downstream consumer needs to be able to *refuse* that reading, which
    means the fact has to be machine-detectable and not merely logged.

    `load_capture_status` is imported inside the test rather than at module
    scope on purpose: reverting `snapshot.py` to `main` then leaves the
    witnesses above failing on their own assertions instead of on a collection
    error, so the flip is evidence about behaviour rather than about a symbol.
    """
    from arenabench.snapshot import load_capture_status

    trial = _trial(tmp_path)
    _capture_that(monkeypatch, commit=(1, "no such container\n"))
    sup = _supervisor(tmp_path, max_capture_attempts=2)

    with caplog.at_level(logging.ERROR, logger="arenabench.snapshot"):
        sup._consider(trial)  # begins capture; the first snapshot fails
        sup._consider(trial)  # second failure retires the trial
        sup.stop()

    errors = [r for r in caplog.records if r.levelno >= logging.ERROR]
    assert errors, "a trial that captured nothing never escalated above WARNING"
    said = "\n".join(r.getMessage() for r in errors)
    assert trial.name in said
    assert "never measured" in said, f"the error does not name the consequence: {said}"

    # Machine-detectable, twice: from the live supervisor, and from the tree
    # afterwards by anything holding only the trial directory.
    unmeasured = sup.unmeasured()
    assert [s.trial for s in unmeasured] == [trial.name]
    assert unmeasured[0].captured_nothing

    status = load_capture_status(trial)
    assert status is not None and status.captured_nothing
    assert status.state == "failed", f"a broken capture ended in {status.state!r}"
    assert "no such container" in status.reason


def test_the_pacing_deadline_now_names_which_fault_it_hit(tmp_path, monkeypatch):
    """The snapshot tests' own wait can stop hedging (#2114 + #2196).

    `_wait_for_snapshots` was landed deliberately unable to say whether a host
    was slow or capture had stopped working — it could only suggest re-running
    at DEBUG, because that was the only level the supervisor said anything at.
    With the record on disk it reads the answer instead of guessing at it.
    """
    from tests.conftest import _wait_for_snapshots

    trial = _trial(tmp_path)
    _capture_that(monkeypatch, commit=(1, "fatal: not a git repository\n"))
    _supervisor(tmp_path)._consider(trial)

    try:
        _wait_for_snapshots(trial, 2, what="of anything at all", deadline=0.0)
    except AssertionError as exc:
        message = str(exc)
    else:  # pragma: no cover - the manifest is empty, so the wait cannot pass
        raise AssertionError("the wait returned on an empty manifest")

    assert "not a git repository" in message, f"the deadline still hedges: {message}"
    assert "keeping up" not in message, f"a broken capture was blamed on speed: {message}"


def test_a_retired_capture_stops_paying_for_a_dead_container(tmp_path, monkeypatch):
    """Retirement is bounded work, not a silent surrender.

    The capture side used to retry forever, so a container that vanished
    mid-trial still cost a `docker exec` — and its 180 s timeout — every
    interval for the rest of the run, all of it invisible. The start side has
    always given up after `max_start_attempts`; this is the same contract.
    """
    from arenabench import snapshot

    trial = _trial(tmp_path)
    attempts: list[str] = []

    def counting_exec(container: str, script: str, *, timeout: float = 180.0):
        if "rev-parse HEAD" in script:
            attempts.append(script)
            return 1, "no such container\n"
        if "init -q" in script:
            return 0, "ok\n"
        return 0, "/app\n"

    monkeypatch.setattr(snapshot, "_exec", counting_exec)
    monkeypatch.setattr(snapshot, "_container_running", lambda name: True)

    sup = _supervisor(tmp_path, max_capture_attempts=2)
    for _ in range(6):
        sup._consider(trial)

    assert len(attempts) == 2, f"capture kept retrying a dead container: {len(attempts)}"


def test_capture_survives_a_stumble_without_being_retired(tmp_path, monkeypatch):
    """Only *consecutive* failures retire a trial — a blip must not end capture.

    The counter resetting on success is what keeps this fix from costing
    measurement on a merely loaded host, which would be the same sin in the
    opposite direction.
    """
    from arenabench import snapshot

    outcomes = iter([(1, "transient\n"), (0, "b" * 40 + "\n"), (1, "transient\n")])

    def flaky_exec(container: str, script: str, *, timeout: float = 180.0):
        if "rev-parse HEAD" in script:
            return next(outcomes, (0, "c" * 40 + "\n"))
        if "init -q" in script:
            return 0, "ok\n"
        return 0, "/app\n"

    monkeypatch.setattr(snapshot, "_exec", flaky_exec)
    monkeypatch.setattr(snapshot, "_container_running", lambda name: True)

    trial = _trial(tmp_path)
    sup = _supervisor(tmp_path, max_capture_attempts=2)
    for _ in range(4):
        sup._consider(trial)

    record = _record(trial)
    assert record["snapshots"] >= 2, "a recovering host was retired anyway"
    assert record["state"] != "failed"
    assert sup.unmeasured() == []
