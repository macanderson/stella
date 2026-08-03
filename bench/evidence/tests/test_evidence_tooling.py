"""Tests for the development-baseline scoring and manifest tools.

These two scripts decide what a published Terminal-Bench number *is* — which
trials it covers, which configuration produced it, and which caveats travel with
it. Nothing ran them until this file existed, and three defects had accumulated
behind that: a manifest that recorded `"trials": {"count": 0}` for a complete
89-trial run, a disclosure block that asserted an emulated macOS host for a run
on native Linux, and a posture recomputed from whatever checkout the script
happened to execute in rather than the one the trials used.

Each of those is silent. The manifest is well-formed and confident either way,
so the tests here are mostly about the difference between "wrong" and "visibly
wrong".
"""

from __future__ import annotations

import importlib.util
import json
import sys
from pathlib import Path

import pytest

_EVIDENCE = Path(__file__).resolve().parent.parent


def _load(name: str):
    """Import one of the sibling scripts by path — they are files, not a package."""
    spec = importlib.util.spec_from_file_location(name, _EVIDENCE / f"{name}.py")
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


manifest = _load("make_manifest")
scorer = _load("score_dev_baseline")


# --------------------------------------------------------------------------
# Trial discovery
# --------------------------------------------------------------------------


def _write_result(path: Path, task: str) -> None:
    path.mkdir(parents=True, exist_ok=True)
    (path / "result.json").write_text(json.dumps({"task_name": task}))


def test_flat_layout_is_found(tmp_path: Path) -> None:
    """Harbor 0.6.1 — the pinned audited constant — uses the flat layout.

    This is the regression that matters most: reading only the nested layout
    reported a complete run as having no trials, and `count` is the first field
    a reader checks to see whether the manifest covers the whole run.
    """
    job = tmp_path / "job"
    _write_result(job / "alpha__aaa", "alpha")
    _write_result(job / "beta__bbb", "beta")

    assert manifest._trial_dirs(job, tmp_path / "run") == ["alpha__aaa", "beta__bbb"]


def test_nested_layout_is_found(tmp_path: Path) -> None:
    """Later Harbor releases nest trials under `trials/`."""
    job = tmp_path / "job"
    _write_result(job / "trials" / "alpha__aaa", "alpha")
    _write_result(job / "trials" / "beta__bbb", "beta")

    assert manifest._trial_dirs(job, tmp_path / "run") == ["alpha__aaa", "beta__bbb"]


def test_nested_layout_wins_over_a_stray_sibling(tmp_path: Path) -> None:
    """A `trials/` directory is the authority when both shapes are present."""
    job = tmp_path / "job"
    _write_result(job / "trials" / "alpha__aaa", "alpha")
    _write_result(job / "leftover__zzz", "leftover")

    assert manifest._trial_dirs(job, tmp_path / "run") == ["alpha__aaa"]


def test_trials_come_from_the_evidence_when_the_job_tree_is_gone(tmp_path: Path) -> None:
    """The job tree is never committed, so rebuilding from evidence must work.

    `bench/evidence/README.md` promises a reader can reproduce a published
    number from the run directory alone. A manifest rebuilt that way previously
    named no trials at all.
    """
    run = tmp_path / "run"
    run.mkdir()
    (run / "trials.jsonl").write_text(
        json.dumps({"trial_id": "beta__bbb", "task_name": "beta"})
        + "\n"
        + json.dumps({"trial_id": "alpha__aaa", "task_name": "alpha"})
        + "\n"
    )

    assert manifest._trial_dirs(None, run) == ["alpha__aaa", "beta__bbb"]


def test_no_job_tree_and_no_evidence_is_empty_not_an_error(tmp_path: Path) -> None:
    assert manifest._trial_dirs(None, tmp_path) == []


# --------------------------------------------------------------------------
# Recorded identity, and drift against this checkout
# --------------------------------------------------------------------------


def _trials_file(tmp_path: Path, rows: list[dict]) -> Path:
    run = tmp_path / "run"
    run.mkdir(exist_ok=True)
    (run / "trials.jsonl").write_text("".join(json.dumps(r) + "\n" for r in rows))
    return run


def test_uniform_fields_collapse_to_one_value(tmp_path: Path) -> None:
    run = _trials_file(
        tmp_path,
        [
            {"trial_id": "a", "source_commit": "abc", "harbor_version": "0.6.1"},
            {"trial_id": "b", "source_commit": "abc", "harbor_version": "0.6.1"},
        ],
    )
    recorded = manifest._recorded(run)

    assert recorded["trials_read"] == 2
    assert recorded["source_commit"] == "abc"
    assert recorded["harbor_version"] == "0.6.1"


def test_disagreeing_trials_are_surfaced_not_averaged(tmp_path: Path) -> None:
    """A run whose trials executed different binaries is not one number."""
    run = _trials_file(
        tmp_path,
        [
            {"trial_id": "a", "binary_sha256": "aaa"},
            {"trial_id": "b", "binary_sha256": "bbb"},
        ],
    )
    assert manifest._recorded(run)["binary_sha256"] == {"DISAGREEMENT": ["aaa", "bbb"]}


def test_missing_evidence_records_nothing(tmp_path: Path) -> None:
    assert manifest._recorded(tmp_path) == {}


def test_drift_confirms_agreement() -> None:
    computed = {"posture": {"a": 1}, "normalized_sha256": "digest"}
    out = manifest._drift(computed, "digest")

    assert out["normalized_sha256"] == "digest"
    assert out["posture"] == {"a": 1}
    assert "agrees" in out["source"]


def test_drift_withholds_a_body_this_run_never_used() -> None:
    """The dangerous case: a plausible posture from the wrong checkout.

    Emitting it beside the recorded digest would let a reader take it as this
    run's configuration, so the body is dropped and the disagreement is stated.
    """
    computed = {"posture": {"effort": "low"}, "normalized_sha256": "from-this-tree"}
    out = manifest._drift(computed, "what-actually-ran")

    assert out["normalized_sha256"] == "what-actually-ran"
    assert out["computed_from_this_checkout_sha256"] == "from-this-tree"
    assert "posture" not in out


def test_drift_keeps_the_recorded_digest_when_the_adapter_is_absent() -> None:
    """Off-host, the recorded digest is the only value there is — do not drop it."""
    out = manifest._drift({"error": "adapter not importable: no module"}, "what-ran")

    assert out["normalized_sha256"] == "what-ran"
    assert "adapter not importable" in out["note"]


def test_drift_passes_through_when_the_run_recorded_nothing() -> None:
    computed = {"posture": {"a": 1}, "normalized_sha256": "digest"}
    assert manifest._drift(computed, None) == computed


# --------------------------------------------------------------------------
# Claim eligibility — the disclosure block
# --------------------------------------------------------------------------


def test_native_host_does_not_disclose_emulation_it_does_not_have() -> None:
    reasons = manifest._claim_eligibility({"native_x86_64_linux": True})["why_not"]

    assert not any("emulation" in r or "Rosetta" in r for r in reasons)
    assert any("host attestation" in r for r in reasons)


def test_emulated_host_discloses_emulation_first() -> None:
    reasons = manifest._claim_eligibility({"native_x86_64_linux": False})["why_not"]

    assert "emulation" in reasons[0]
    assert len(reasons) == 6


def test_a_host_that_does_not_say_is_treated_as_emulated() -> None:
    """Absence must fail towards more disclosure, never less."""
    reasons = manifest._claim_eligibility({})["why_not"]

    assert "emulation" in reasons[0]


def test_no_host_is_ever_an_audited_row() -> None:
    for host in ({"native_x86_64_linux": True}, {"native_x86_64_linux": False}):
        assert manifest._claim_eligibility(host)["audited_public_row"] is False


def test_supplied_host_replaces_local_detection(tmp_path: Path) -> None:
    """The local machine is not the run host once the run is over."""
    path = tmp_path / "host.json"
    path.write_text(json.dumps({"machine": "x86_64", "native_x86_64_linux": True}))
    host = manifest._host(path)

    assert host["machine"] == "x86_64"
    assert "collected on the run host" in host["source"]


# --------------------------------------------------------------------------
# Extraction and scoring
# --------------------------------------------------------------------------


def _result(tmp_path: Path, payload: dict) -> Path:
    trial = tmp_path / "task__aaa"
    trial.mkdir(parents=True, exist_ok=True)
    path = trial / "result.json"
    path.write_text(json.dumps(payload))
    return path


def test_extract_carries_the_runs_own_identity(tmp_path: Path) -> None:
    """Without these the manifest can only recompute, which is how it drifted."""
    path = _result(
        tmp_path,
        {
            "task_name": "task",
            "verifier_result": {"rewards": {"reward": 1.0}},
            "agent_result": {
                "metadata": {
                    "stella_engine_posture_sha256": "posture-digest",
                    "stella_assurance_arm": "witness-off",
                    "stella_assurance_tiers_sha256": "tiers-digest",
                    "stella_binary_sha256": "binary-digest",
                    "stella_source_commit": "commit-sha",
                    "stella_harbor_version": "0.6.1",
                }
            },
        },
    )
    row = scorer.extract_trial(path)

    assert row["engine_posture_sha256"] == "posture-digest"
    assert row["assurance_arm"] == "witness-off"
    assert row["assurance_tiers_sha256"] == "tiers-digest"
    assert row["binary_sha256"] == "binary-digest"
    assert row["source_commit"] == "commit-sha"
    assert row["harbor_version"] == "0.6.1"
    assert row["reward"] == 1.0


def test_reward_is_read_from_the_nested_rewards_object(tmp_path: Path) -> None:
    """`verifier_result.reward` is always None; the reward lives one level in.

    Reading the outer key scores every trial of every run zero, and does so
    without erroring — the failure mode is a plausible number, not a crash.
    """
    path = _result(
        tmp_path,
        {
            "task_name": "task",
            "verifier_result": {"reward": None, "rewards": {"reward": 1.0}},
            "agent_result": {"metadata": {}},
        },
    )
    assert scorer.extract_trial(path)["accuracy"] == 1.0


def test_an_attempted_trial_with_no_reward_scores_zero(tmp_path: Path) -> None:
    path = _result(
        tmp_path,
        {
            "task_name": "task",
            "finished_at": "2026-07-31T23:00:00Z",
            "verifier_result": {"rewards": {}},
            "agent_result": {"metadata": {}},
        },
    )
    row = scorer.extract_trial(path)

    assert row["reward"] is None
    assert row["accuracy"] == 0.0


def test_a_missing_task_scores_zero_against_the_fixed_denominator(tmp_path: Path) -> None:
    """An aborted run must not be able to flatter itself by reporting fewer tasks."""
    trials = tmp_path / "trials.jsonl"
    trials.write_text(
        "".join(
            json.dumps({"task_name": f"t{i}", "accuracy": 1.0}) + "\n" for i in range(5)
        )
    )

    args = scorer.main.__globals__["argparse"].Namespace(
        trials=str(trials), tasks=10, markdown=None
    )
    assert scorer.cmd_score(args) == 0


def _score(tmp_path: Path, rows: list[dict], tasks: int, capsys) -> dict:
    """Run the `score` command and parse the summary it prints to stdout."""
    trials = tmp_path / "trials.jsonl"
    trials.write_text("".join(json.dumps(r) + "\n" for r in rows))
    argparse = scorer.main.__globals__["argparse"]
    args = argparse.Namespace(trials=str(trials), tasks=tasks, markdown=None)

    assert scorer.cmd_score(args) == 0
    return json.loads(capsys.readouterr().out)


def test_a_partial_cost_sum_carries_its_own_denominator(tmp_path: Path, capsys) -> None:
    """A comparator arm reports cost on some trials; the total must say so.

    Summing the trials that reported and publishing it beside a complete total
    from the other arm is a like-for-like comparison that is not one.
    """
    rows = [{"task_name": f"t{i}", "accuracy": 0.0, "usd": 1.0} for i in range(3)]
    rows += [{"task_name": f"t{i}", "accuracy": 0.0} for i in range(3, 5)]
    descriptive = _score(tmp_path, rows, 5, capsys)["descriptive"]

    assert descriptive["usd_total"] == 3.0
    assert descriptive["usd_reported_by"] == 3
    assert descriptive["trials"] == 5


def test_a_trial_that_reported_no_tool_calls_is_not_one_that_made_none(
    tmp_path: Path, capsys
) -> None:
    """`tool_calls` comes from Stella's accounting block, which only Stella emits.

    Counting absent as zero reported every trial of the Claude Code arm as
    having made zero tool calls.
    """
    rows = [{"task_name": f"t{i}", "accuracy": 0.0} for i in range(4)]
    descriptive = _score(tmp_path, rows, 4, capsys)["descriptive"]

    assert descriptive["tool_calls_reported_by"] == 0
    assert descriptive["trials_with_zero_tool_calls"] == 0


def test_a_genuine_zero_tool_call_trial_still_counts(tmp_path: Path, capsys) -> None:
    """The zero-tool-call signature is a real failure mode — do not lose it."""
    rows = [
        {"task_name": "a", "accuracy": 0.0, "tool_calls": 0},
        {"task_name": "b", "accuracy": 1.0, "tool_calls": 12},
    ]
    descriptive = _score(tmp_path, rows, 2, capsys)["descriptive"]

    assert descriptive["tool_calls_reported_by"] == 2
    assert descriptive["trials_with_zero_tool_calls"] == 1


def test_a_genuine_zero_cost_trial_is_not_treated_as_unreported(
    tmp_path: Path, capsys
) -> None:
    """$0.0000 from a trial that never reached the model is a datum, not a gap."""
    rows = [
        {"task_name": "a", "accuracy": 0.0, "usd": 0.0},
        {"task_name": "b", "accuracy": 0.0, "usd": 2.0},
    ]
    descriptive = _score(tmp_path, rows, 2, capsys)["descriptive"]

    assert descriptive["usd_reported_by"] == 2
    assert descriptive["usd_total"] == 2.0


@pytest.mark.parametrize(
    ("passes", "total"),
    [(0, 89), (89, 89), (58, 89), (44, 89)],
)
def test_clopper_pearson_stays_inside_the_unit_interval(passes: int, total: int) -> None:
    """The continued fraction diverges above the mode without the reflection.

    Near 0 and near 1 is exactly where a wrong interval looks most convincing.
    """
    low, high = scorer.clopper_pearson(passes, total)

    assert 0.0 <= low <= passes / total <= high <= 1.0
