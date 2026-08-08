# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The proof rail: what actually proved a trial, and what merely claimed it.

The bias here is the same one the module itself is built around: a benchmark
that reports "passed" for an unexamined claim is worse than one that reports
nothing, because the number looks exactly like a real one. So the tests that
matter most are the ones separating a proven pass from a claimed pass, and the
ones asserting that a legacy spelling still parses — an archive that silently
loses its proof reads as an agent that never produced any.

Every fixture is a literal event stream in the shape
``crates/stella-protocol/src/proof.rs`` serializes. No files, no network.
"""

from __future__ import annotations

import json
from pathlib import Path

from arenabench.proof import distill
from arenabench.telemetry import MetricsReader, TranscriptReader, aggregate

# --------------------------------------------------------------------------
# Event-stream fixtures
# --------------------------------------------------------------------------


def _usage(role: str, model: str, **kw) -> dict:
    return {
        "type": "step_usage",
        "role": role,
        "model": model,
        "input_tokens": kw.get("input_tokens", 100),
        "output_tokens": kw.get("output_tokens", 10),
        "cost_usd": kw.get("cost_usd", 0.01),
    }


def _verdict(passed: bool, summary: str, **ladder) -> dict:
    return {
        "type": "verdict",
        "passed": passed,
        "evidence": {
            "summary": summary,
            "deterministic": ladder.pop("deterministic", True),
            "ladder": ladder,
        },
    }


#: The gold standard: an independent model authored a failing test, the agent's
#: first attempt failed it, the second passed. Modelled on a real recorded
#: trial (`nginx-request-logging`, oracle trace ✗ ✓ ✓).
FLIP_STREAM: list[dict] = [
    {"type": "proof", "step": {"kind": "assurance", "witness": True, "verifier": True}},
    _usage("worker", "moonshotai/kimi-k3"),
    {"type": "stage", "name": "witness"},
    _usage("witness_author", "z-ai/glm-5.2"),
    {
        "type": "proof",
        "step": {
            "kind": "witness_authored",
            "path": "witness_check.sh",
            "command": "sh witness_check.sh",
            "fingerprint": "sha256:aa2df0d1",
        },
    },
    {"type": "proof", "step": {"kind": "warrant", "required": True, "diff_lines": 0}},
    {
        "type": "proof",
        "step": {
            "kind": "oracle",
            "command": "sh witness_check.sh",
            "passed": False,
            "tree": "candidate",
        },
    },
    _verdict(
        True,
        "flip oracle: fail→pass of `sh witness_check.sh`; touched tests green",
        rung="submit_fast",
        flip_achieved=True,
        tracked_command="sh witness_check.sh",
        witness_intact=True,
        touched_tests_passed=True,
        diff_lines=112,
        diff_coverage="covered",
        oracle_trace=[
            {"tree": "candidate", "passed": False},
            {"tree": "candidate", "passed": True},
            {"tree": "candidate", "passed": True},
        ],
    ),
]


# --------------------------------------------------------------------------
# The flip — the only outcome that earns the word "done"
# --------------------------------------------------------------------------


def test_flip_is_graded_as_proven_and_counts_the_wrong_guess() -> None:
    proof = distill(FLIP_STREAM)

    assert proof.grade == "flip"
    assert proof.witness_authored
    assert proof.witness_path == "witness_check.sh"
    assert proof.witness_command == "sh witness_check.sh"
    assert proof.flip_achieved is True
    assert proof.witness_intact is True
    assert proof.deterministic
    assert not proof.claimed_without_proof
    # The point of the whole rail: the model thought it was done once, and a
    # test disagreed, before it got there.
    assert proof.failed_attempts == 1
    assert [run.passed for run in proof.oracle_runs] == [False, True, True]


def test_ladder_trace_supersedes_the_proof_steps() -> None:
    """The verdict's own trace is richer than what reached the stream.

    An authored witness's failing baseline is folded into the oracle after
    the fact rather than published as a `proof.oracle` step, so counting
    those steps undercounts every genuine flip. Here one step was emitted
    and three observations exist.
    """
    proof = distill(FLIP_STREAM)

    assert sum(1 for e in FLIP_STREAM if e.get("step", {}).get("kind") == "oracle") == 1
    assert len(proof.oracle_runs) == 3
    # The command survives the substitution — the trace itself has no room
    # for one, and a flip strip with no command names nothing.
    assert all(run.command == "sh witness_check.sh" for run in proof.oracle_runs)


def test_ladder_diff_lines_beats_the_warrants_earlier_count() -> None:
    """Both are real; they measure at different moments.

    The warrant fires as it decides whether to demand proof and can publish a
    0 that the verdict-time count contradicts. A reader asking "how big was
    this change" means the later one.
    """
    proof = distill(FLIP_STREAM)

    assert proof.diff_lines == 112


# --------------------------------------------------------------------------
# The honesty trap — three different things all publish `passed: true`
# --------------------------------------------------------------------------


def test_unverifiable_pass_is_not_a_proven_pass() -> None:
    proof = distill(
        [
            _verdict(
                True,
                "UNVERIFIABLE — every verification channel was blind",
                rung="unverifiable",
                flip_achieved=False,
                deterministic=False,
            )
        ]
    )

    assert proof.verdict_passed is True
    assert proof.claimed_without_proof
    assert not proof.deterministic
    assert proof.honesty == "unverifiable"


def test_waived_pass_is_not_a_proven_pass() -> None:
    """A waiver rides `deterministic: true` on the evidence and is not.

    `LadderRung::is_deterministic` excludes `waived` for exactly this reason,
    and trusting the evidence flag instead would score an unexamined claim as
    an observation.
    """
    proof = distill(
        [
            _verdict(
                True,
                "model review waived by triage; no independent verification",
                rung="waived",
                flip_achieved=False,
                deterministic=True,
            )
        ]
    )

    assert proof.grade == "waived"
    assert proof.claimed_without_proof


def test_model_verdict_with_no_corroboration_is_a_claim() -> None:
    proof = distill(
        [
            _usage("worker", "z-ai/glm-5.2"),
            _usage("verdict", "z-ai/glm-5.2"),
            _verdict(
                True,
                "The diff shows the change being applied, accomplishing the goal.",
                rung="model_verdict",
                flip_achieved=False,
                verifier_independent=False,
                deterministic=False,
            ),
        ]
    )

    assert proof.grade == "model"
    assert proof.claimed_without_proof
    assert proof.honesty == ""  # a plain claim, with no marker at all
    # The run graded its own homework.
    assert proof.verifier_independent is False


def test_a_real_flip_is_never_claimed_without_proof() -> None:
    assert not distill(FLIP_STREAM).claimed_without_proof


# --------------------------------------------------------------------------
# Refusal — the rail catching a premature "done"
# --------------------------------------------------------------------------


def test_oracle_that_never_passes_is_refuted_not_merely_failed() -> None:
    proof = distill(
        [
            {
                "type": "proof",
                "step": {
                    "kind": "witness_authored",
                    "path": "witness_check.sh",
                    "command": "sh witness_check.sh",
                    "fingerprint": "sha256:beef",
                },
            },
            _verdict(
                False,
                "touched tests failed after execution",
                rung="revise",
                flip_achieved=False,
                tracked_command="sh witness_check.sh",
                oracle_trace=[{"tree": "candidate", "passed": False}],
            ),
        ]
    )

    assert proof.grade == "refuted"
    # A run that never recovered still guessed wrong — scoping this to
    # "before the first pass" would report zero for the starkest case.
    assert proof.failed_attempts == 1


def test_witness_unavailable_carries_its_full_reason() -> None:
    reason = (
        "no author independent of the worker "
        "(judge and worker both resolved to `openrouter/z-ai/glm-5.2`)"
    )
    proof = distill(
        [
            {"type": "stage", "name": "witness"},
            {"type": "proof", "step": {"kind": "witness_unavailable", "reason": reason}},
        ]
    )

    assert proof.grade == "unproven"
    assert proof.stage_ran
    assert proof.witness_unavailable == (reason,)


def test_unwarranted_change_is_not_an_unproven_one() -> None:
    """"No proof was demanded" and "proof was demanded and missing" differ.

    Grading a docs-only change as unproven would put a correct outcome in the
    same bucket as a failure to prove.
    """
    proof = distill(
        [
            {
                "type": "proof",
                "step": {
                    "kind": "warrant",
                    "required": False,
                    "reason": "documentation only; prose has no runtime behavior to flip",
                    "diff_lines": 12,
                },
            }
        ]
    )

    assert proof.grade == "unwarranted"
    assert proof.warranted is False
    assert "documentation only" in proof.warrant_reason


# --------------------------------------------------------------------------
# Who ran — the pipeline model roster
# --------------------------------------------------------------------------


def test_roles_census_names_the_witness_author_separately_from_the_worker() -> None:
    proof = distill(FLIP_STREAM)
    roster = {call.role: call.model for call in proof.roles}

    assert roster["worker"] == "moonshotai/kimi-k3"
    assert roster["witness_author"] == "z-ai/glm-5.2"


def test_roles_are_ordered_by_call_count_then_name() -> None:
    """Deterministic, so a golden fixture of this output stays stable."""
    proof = distill(
        [
            _usage("verdict", "b"),
            _usage("worker", "a"),
            _usage("worker", "a"),
            _usage("triage", "c"),
        ]
    )

    assert [(c.role, c.calls) for c in proof.roles] == [
        ("worker", 2),
        ("triage", 1),
        ("verdict", 1),
    ]


def test_role_usage_accumulates_tokens_and_cost() -> None:
    proof = distill(
        [
            _usage("worker", "a", input_tokens=100, output_tokens=10, cost_usd=0.5),
            _usage("worker", "a", input_tokens=200, output_tokens=20, cost_usd=0.25),
        ]
    )

    (call,) = proof.roles
    assert (call.calls, call.tokens_in, call.tokens_out) == (2, 300, 30)
    assert call.cost_usd == 0.75


# --------------------------------------------------------------------------
# Legacy spellings — an archive must not silently lose its proof
# --------------------------------------------------------------------------


def test_judge_verdict_tag_still_parses() -> None:
    """`judge_verdict` is the pre-rename tag the protocol still reads back."""
    proof = distill(
        [
            {
                "type": "judge_verdict",
                "passed": True,
                "evidence": {
                    "summary": "ok",
                    "deterministic": True,
                    "ladder": {"rung": "submit_fast", "flip_achieved": True},
                },
            }
        ]
    )

    assert proof.verdict_passed is True
    assert proof.rung == "submit_fast"
    assert proof.flip_achieved is True


def test_model_judge_rung_normalises_to_model_verdict() -> None:
    proof = distill([_verdict(True, "ok", rung="model_judge", flip_achieved=False)])

    assert proof.rung == "model_verdict"
    assert proof.grade == "model"


def test_judge_alias_on_assurance_and_role() -> None:
    proof = distill(
        [
            {"type": "proof", "step": {"kind": "assurance", "witness": True, "judge": True}},
            _usage("judge", "some/model"),
        ]
    )

    assert proof.assurance_verifier is True
    assert [call.role for call in proof.roles] == ["verdict"]


# --------------------------------------------------------------------------
# Tolerance — a stream is read while it is being written
# --------------------------------------------------------------------------


def test_empty_stream_grades_as_no_rail() -> None:
    proof = distill([])

    assert proof.grade == "none"
    assert not proof.claimed_without_proof
    assert proof.roles == ()


def test_unknown_proof_step_kind_does_not_lose_the_rest() -> None:
    """The protocol's `ProofStep` enum is closed and will grow."""
    proof = distill(
        [
            {"type": "proof", "step": {"kind": "some_future_step", "detail": "x"}},
            {"type": "proof", "step": {"kind": "warrant", "required": True, "diff_lines": 5}},
        ]
    )

    assert proof.warranted is True


def test_malformed_events_are_skipped() -> None:
    proof = distill(
        [
            {"type": "proof"},  # no step
            {"type": "proof", "step": "not-a-mapping"},
            {"type": "verdict", "passed": True},  # no evidence
            {"type": "proof", "step": {"kind": "warrant", "required": True, "diff_lines": 1}},
        ]
    )

    assert proof.warranted is True
    assert proof.verdict_passed is True


def test_to_json_round_trips() -> None:
    payload = json.loads(json.dumps(distill(FLIP_STREAM).to_json()))

    assert payload["grade"] == "flip"
    assert payload["failed_attempts"] == 1
    assert payload["witness_authored"] is True
    assert len(payload["oracle_runs"]) == 3
    assert {row["role"] for row in payload["roles"]} == {"worker", "witness_author"}


# --------------------------------------------------------------------------
# Wiring — the rail has to reach a scoreboard, which is where it was lost
# --------------------------------------------------------------------------


def _trial(tmp_path: Path, name: str, stream: list[dict]) -> Path:
    trial = tmp_path / "job" / name
    (trial / "agent").mkdir(parents=True)
    (trial / "agent" / "stella-events.jsonl").write_text(
        "\n".join(json.dumps(e) for e in stream) + "\n", encoding="utf-8"
    )
    return trial


def test_metrics_reader_attaches_the_rail(tmp_path: Path) -> None:
    metrics = MetricsReader().read(_trial(tmp_path, "t1", FLIP_STREAM), "nginx")

    assert metrics.proof is not None
    assert metrics.proof.grade == "flip"
    assert metrics.to_json()["proof"]["failed_attempts"] == 1


def test_a_trial_with_no_event_stream_has_no_rail(tmp_path: Path) -> None:
    """A Claude Code arm publishes no proof rail.

    ``None`` rather than an empty rail: it is the absence of the machinery,
    not a failure of it, and a zero would read as "proved nothing" in a
    column where that is an accusation.
    """
    empty = tmp_path / "job" / "t2"
    empty.mkdir(parents=True)

    assert MetricsReader().read(empty, "nginx").proof is None


def test_aggregate_rolls_the_rail_up(tmp_path: Path) -> None:
    reader = MetricsReader()
    refuted = [
        {
            "type": "proof",
            "step": {
                "kind": "witness_authored",
                "path": "w.sh",
                "command": "sh w.sh",
                "fingerprint": "sha256:1",
            },
        },
        _verdict(
            False,
            "touched tests failed",
            rung="revise",
            flip_achieved=False,
            oracle_trace=[{"tree": "candidate", "passed": False}],
        ),
    ]
    claim = [_verdict(True, "looks right to me", rung="model_verdict", flip_achieved=False)]
    trials = [
        reader.read(_trial(tmp_path, "t1", FLIP_STREAM), "a"),
        reader.read(_trial(tmp_path, "t2", refuted), "b"),
        reader.read(_trial(tmp_path, "t3", claim), "c"),
    ]

    totals = aggregate(trials)

    assert totals["rail_trials"] == 3
    assert totals["proven"] == 1
    assert totals["witnessed"] == 2
    assert totals["refuted"] == 1
    assert totals["claimed_without_proof"] == 1
    # One wrong guess that recovered, one that did not.
    assert totals["wrong_guesses"] == 2


def test_a_seat_with_no_rail_scores_none_not_zero(tmp_path: Path) -> None:
    """The zero would flatter, and in a benchmark that always favours somebody.

    A seat with no pipeline has not "never claimed a pass without proof" — it
    produced no evidence either way. Crowning it on a measurement it never
    took would make the scoreboard reward the absence of the machinery it
    exists to compare. Same discipline as `wasted_time`.
    """
    empty = tmp_path / "job" / "t1"
    empty.mkdir(parents=True)
    totals = aggregate([MetricsReader().read(empty, "a")])

    assert totals["rail_trials"] == 0
    assert totals["proven"] is None
    assert totals["claimed_without_proof"] is None


def test_transcript_renders_proof_entries(tmp_path: Path) -> None:
    """The regression this whole change exists to fix.

    `proof` events were parsed and then dropped: the reader had no branch for
    them, so 394 of them across the recorded matches reached no surface at
    all — while the web UI shipped a filter chip for a kind nothing produced.
    """
    trial = _trial(tmp_path, "t1", FLIP_STREAM)
    entries = TranscriptReader().read(trial / "agent" / "stella-events.jsonl")

    proofs = [e for e in entries if e["kind"] == "proof"]
    titles = [e["title"] for e in proofs]

    assert "witness authored" in titles
    assert "warrant: proof required" in titles
    assert "oracle: candidate failed" in titles
    authored = next(e for e in proofs if e["title"] == "witness authored")
    assert authored["meta"]["path"] == "witness_check.sh"


def test_transcript_verdict_body_is_the_evidence_summary(tmp_path: Path) -> None:
    """It was read from a top-level `reasoning` key the wire never carried.

    Every verdict in every transcript therefore rendered with an empty body —
    and the summary is precisely where the pipeline says whether a pass was
    proven.
    """
    trial = _trial(tmp_path, "t1", FLIP_STREAM)
    entries = TranscriptReader().read(trial / "agent" / "stella-events.jsonl")

    verdict = next(e for e in entries if e["kind"] == "verdict")

    assert "flip oracle: fail→pass" in verdict["body"]
    assert verdict["meta"]["rung"] == "submit_fast"
    assert verdict["meta"]["flip_achieved"] is True


def test_proof_reasons_are_never_truncated(tmp_path: Path) -> None:
    """Every other long field here is clipped; this one must not be.

    The half a clip removes is reliably the half naming the model, the
    command or the constraint — the part a reader opened the page for.
    """
    reason = "no author independent of the worker: " + "x" * 9000
    trial = _trial(
        tmp_path,
        "t1",
        [{"type": "proof", "step": {"kind": "witness_unavailable", "reason": reason}}],
    )
    entries = TranscriptReader().read(trial / "agent" / "stella-events.jsonl")

    assert next(e for e in entries if e["kind"] == "proof")["body"] == reason
