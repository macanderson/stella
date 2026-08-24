"""The detectors, against a fixture trace. No network, by construction.

What these hold onto is not "the detector fires" — a detector that fires on
everything also passes that — but the three discriminations each one has to
make, because every one of them has been got wrong in this repository before:

* a `proof` read on the top-level `type` instead of `step.kind` sees one
  undifferentiated bucket;
* a reason string cut to fit a column loses the half that names the defect;
* an absent `file_change` read as an unchanged tree is a conclusion the event
  stream cannot support.
"""

from __future__ import annotations

from detectors import run_all
from fixtures import err, ok, proof, tool_pair, write_run
from run_trace import load_run


def _findings(root, code):
    return [f for f in run_all(load_run(root, "testrun")) if f.detector == code]


# --------------------------------------------------------------------------
# void-model-call
# --------------------------------------------------------------------------


def test_void_model_call_fires_on_reasoning_without_completed_steps(tmp_path):
    write_run(
        tmp_path,
        [
            {
                "task": "alpha__AAA",
                "reward": 0,
                "exception_class": "AgentTimeoutError",
                "events": [
                    {"ts": 1, "type": "reasoning", "delta": "thinking"},
                    {"ts": 2, "type": "reasoning", "delta": "more"},
                ],
            }
        ],
    )
    found = _findings(tmp_path, "void-model-call")
    assert len(found) == 1
    assert found[0].count == 1
    assert "2 reasoning deltas, 0 step_usage" in found[0].occurrences[0].location


def test_void_model_call_is_silent_when_a_step_completed(tmp_path):
    """One completed step means the model answered — that is not a void."""
    write_run(
        tmp_path,
        [
            {
                "task": "alpha__AAA",
                "reward": 0,
                "events": [
                    {"ts": 1, "type": "reasoning", "delta": "thinking"},
                    {"ts": 2, "type": "step_usage", "role": "worker", "model": "m"},
                ],
            }
        ],
    )
    assert _findings(tmp_path, "void-model-call") == []


# --------------------------------------------------------------------------
# witness-unavailable
# --------------------------------------------------------------------------


LONG_REASON = (
    "witness author turn aborted: stuck-loop detected (persisted after a steering "
    "warning): the same `create_witness_test` call was issued four times with "
    "byte-identical arguments and the loop did not break"
)


def test_witness_unavailable_reads_step_kind_not_the_top_level_type(tmp_path):
    """The discrimination is on `step.kind`; the top tag is `proof` for all of them."""
    write_run(
        tmp_path,
        [
            {
                "task": "alpha__AAA",
                "reward": 0,
                "events": [
                    proof("assurance", witness=True, verifier=True),
                    proof("warrant", required=True),
                    proof(
                        "witness_unavailable", reason="witness author produced no TEST_COMMAND line"
                    ),
                ],
            }
        ],
    )
    found = _findings(tmp_path, "witness-unavailable")
    assert len(found) == 1, "the assurance and warrant proofs must not be counted"


def test_witness_unavailable_splits_distinct_reasons_into_distinct_defects(tmp_path):
    write_run(
        tmp_path,
        [
            {
                "task": "alpha__AAA",
                "reward": 0,
                "events": [
                    proof(
                        "witness_unavailable", reason="witness author produced no TEST_COMMAND line"
                    )
                ],
            },
            {
                "task": "beta__BBB",
                "reward": 0,
                "events": [
                    proof(
                        "witness_unavailable",
                        reason="witness author emitted a test command but created no file",
                    )
                ],
            },
        ],
    )
    found = _findings(tmp_path, "witness-unavailable")
    assert len(found) == 2
    assert len({f.fingerprint.digest for f in found}) == 2


def test_the_reason_string_reaches_the_issue_whole(tmp_path):
    """Reasons are truncated in every other surface; here they must not be."""
    write_run(
        tmp_path,
        [
            {
                "task": "alpha__AAA",
                "reward": 0,
                "events": [proof("witness_unavailable", reason=LONG_REASON)],
            }
        ],
    )
    (finding,) = _findings(tmp_path, "witness-unavailable")
    assert LONG_REASON in finding.occurrences[0].excerpt


# --------------------------------------------------------------------------
# oracle-flip-ungraded
# --------------------------------------------------------------------------


def test_oracle_flip_ungraded_needs_both_trees_and_a_zero_reward(tmp_path):
    write_run(
        tmp_path,
        [
            {
                "task": "flip__AAA",
                "reward": 0,
                "events": [
                    proof("oracle", tree="baseline", passed=False),
                    proof("oracle", tree="candidate", passed=True),
                ],
            },
            {
                # Same flip, but the grader agreed — not a finding.
                "task": "graded__BBB",
                "reward": 1,
                "events": [
                    proof("oracle", tree="baseline", passed=False),
                    proof("oracle", tree="candidate", passed=True),
                ],
            },
            {
                # Failed on both trees: no flip was ever observed.
                "task": "bothfail__CCC",
                "reward": 0,
                "events": [
                    proof("oracle", tree="baseline", passed=False),
                    proof("oracle", tree="candidate", passed=False),
                ],
            },
        ],
    )
    (finding,) = _findings(tmp_path, "oracle-flip-ungraded")
    assert [o.task_id for o in finding.occurrences] == ["flip__AAA"]


# --------------------------------------------------------------------------
# tool-error-envelope
# --------------------------------------------------------------------------


CHAIN_OUTPUT = "total 3960\n" + "\n".join(
    f"-rw-rw-rw-. 1 root root {i} f{i}.jpg" for i in range(12)
)


def test_error_envelope_around_substantial_output_fires(tmp_path):
    write_run(
        tmp_path,
        [
            {
                "task": "alpha__AAA",
                "reward": 0,
                "events": tool_pair(
                    "c1",
                    "bash",
                    {"command": "ls -la docs/; file docs/*"},
                    err(
                        CHAIN_OUTPUT + "\nbash: line 1: file: command not found\n\n[exit code: 127]"
                    ),
                ),
            }
        ],
    )
    (finding,) = _findings(tmp_path, "tool-error-envelope")
    assert CHAIN_OUTPUT in finding.occurrences[0].excerpt


def test_an_environment_classed_envelope_still_fires(tmp_path):
    """A classified `environment` error proceeds to the envelope heuristics."""
    write_run(
        tmp_path,
        [
            {
                "task": "alpha__AAA",
                "reward": 0,
                "events": tool_pair(
                    "c1",
                    "bash",
                    {"command": "ls -la docs/; file docs/*"},
                    err(
                        CHAIN_OUTPUT
                        + "\nbash: line 1: file: command not found\n\n[exit code: 127]",
                        cls="environment",
                    ),
                ),
            }
        ],
    )
    (finding,) = _findings(tmp_path, "tool-error-envelope")
    assert CHAIN_OUTPUT in finding.occurrences[0].excerpt
    assert "error class `environment`" in finding.occurrences[0].location


def test_a_refusal_class_beats_an_envelope_shaped_message(tmp_path):
    """The class is the discriminator, not the prose.

    This payload passes every string heuristic — `[exit code:` trailer,
    substantial body — and the executor classified it `refused_by_policy`, so
    it must not fire. A detector that only reads the message fires here.
    """
    write_run(
        tmp_path,
        [
            {
                "task": "alpha__AAA",
                "reward": 0,
                "events": tool_pair(
                    "c1",
                    "bash",
                    {"command": "ls -la docs/; file docs/*"},
                    err(
                        CHAIN_OUTPUT
                        + "\nbash: line 1: file: command not found\n\n[exit code: 127]",
                        cls="refused_by_policy",
                    ),
                ),
            }
        ],
    )
    assert _findings(tmp_path, "tool-error-envelope") == []


def test_a_guard_refusal_is_not_this_defect(tmp_path):
    """A refusal wraps a refusal, not output, and is a different ticket."""
    refusal = (
        "refused before running: `/app/documents/` is inside the graded tree `/app`, which "
        "this shell may not touch.\nThis shell's workspace is a snapshot of that tree.\n"
        "Use the candidate workspace instead.\nNothing else outside is restricted."
    )
    write_run(
        tmp_path,
        [
            {
                "task": "alpha__AAA",
                "reward": 0,
                "events": tool_pair("c1", "bash", {"command": "ls /app"}, err(refusal)),
            }
        ],
    )
    assert _findings(tmp_path, "tool-error-envelope") == []


def test_one_defect_stays_one_fingerprint_across_different_payloads(tmp_path):
    """The payload is command output — run data. Keying on it shatters one defect.

    Checked ACROSS runs as well as within one, because within a single run a
    per-payload key still yields one `Finding` per group and the count alone
    cannot tell the two designs apart. Two runs whose only difference is the
    bytes the command printed must hash to the same defect, or next week's
    occurrence opens a second ticket for this week's bug.
    """
    first = tmp_path / "r1"
    second = tmp_path / "r2"
    write_run(
        first,
        [
            {
                "task": "alpha__AAA",
                "reward": 0,
                "events": tool_pair(
                    "c1", "bash", {"command": "a"}, err(CHAIN_OUTPUT + "\n\n[exit code: 1]")
                ),
            }
        ],
    )
    write_run(
        second,
        [
            {
                "task": "beta__BBB",
                "reward": 0,
                "events": tool_pair(
                    "c2",
                    "bash",
                    {"command": "b"},
                    err(
                        "wholly different\nlines of\noutput here "
                        + "x" * 300
                        + "\n\n[exit code: 2]"
                    ),
                ),
            }
        ],
    )
    (a,) = _findings(first, "tool-error-envelope")
    (b,) = _findings(second, "tool-error-envelope")
    assert a.fingerprint.digest == b.fingerprint.digest


# --------------------------------------------------------------------------
# repeated-identical-tool-call
# --------------------------------------------------------------------------


def test_repeat_needs_identical_args_and_identical_output(tmp_path):
    same = tool_pair("c1", "bash", {"command": "make"}, ok("built")) + tool_pair(
        "c2", "bash", {"command": "make"}, ok("built")
    )
    polling = tool_pair("d1", "bash", {"command": "curl :8000"}, ok("refused")) + tool_pair(
        "d2", "bash", {"command": "curl :8000"}, ok("hello")
    )
    write_run(
        tmp_path,
        [
            {"task": "repeat__AAA", "reward": 0, "events": same},
            {"task": "poll__BBB", "reward": 0, "events": polling},
        ],
    )
    (finding,) = _findings(tmp_path, "repeated-identical-tool-call")
    assert [o.task_id for o in finding.occurrences] == ["repeat__AAA"]


# --------------------------------------------------------------------------
# role-model-census-mismatch
# --------------------------------------------------------------------------


def test_census_mismatch_ignores_a_provider_prefix_but_catches_a_wrong_model(tmp_path):
    write_run(
        tmp_path,
        [
            {
                "task": "alpha__AAA",
                "reward": 0,
                "events": [
                    # Declared as openrouter/z-ai/glm-4.7-flash — a prefix difference only.
                    {
                        "ts": 1,
                        "type": "step_usage",
                        "role": "triage",
                        "model": "z-ai/glm-4.7-flash",
                    },
                    # Declared as openrouter/deepseek/deepseek-chat — genuinely different.
                    {"ts": 2, "type": "step_usage", "role": "plan", "model": "z-ai/glm-5.2"},
                ],
            }
        ],
    )
    (finding,) = _findings(tmp_path, "role-model-census-mismatch")
    assert finding.count == 1
    assert "role `plan`" in finding.occurrences[0].location


# --------------------------------------------------------------------------
# no-post-verdict-file-change
# --------------------------------------------------------------------------


def test_absent_verdict_and_silent_delivery_are_different_defects(tmp_path):
    write_run(
        tmp_path,
        [
            {
                "task": "killed__AAA",
                "reward": 0,
                "events": [{"ts": 9, "type": "tool_result", "call_id": "x", "output": ok("done")}],
            },
            {
                "task": "silent__BBB",
                "reward": 0,
                "events": [
                    {"ts": 5, "type": "file_change", "path": "a", "kind": "created"},
                    {"ts": 9, "type": "verdict", "passed": False, "evidence": {}},
                ],
            },
        ],
    )
    found = _findings(tmp_path, "no-post-verdict-file-change")
    assert len(found) == 2, "collapsing these two would bury the residual half"
    assert len({f.fingerprint.digest for f in found}) == 2


def test_absence_of_file_change_is_never_reported_as_an_unchanged_tree(tmp_path):
    """`bash` names no path, so the event's absence proves nothing about the tree."""
    write_run(
        tmp_path,
        [
            {
                "task": "silent__BBB",
                "reward": 0,
                "events": [{"ts": 9, "type": "verdict", "passed": True, "evidence": {}}],
            }
        ],
    )
    (finding,) = [f for f in _findings(tmp_path, "no-post-verdict-file-change") if f.count]
    blob = " ".join(finding.caveats) + finding.summary
    assert "observability, never evidence" in blob
    assert "ABSENT EVENT" in blob


# --------------------------------------------------------------------------
# agent-timeout
# --------------------------------------------------------------------------


def test_the_pipeline_detectors_are_silent_on_a_bare_loop_arm(tmp_path):
    """A bare arm has no verify stage: its absences are the design, not a defect.

    Without this the bare arm reports 20/20 "no verdict event", which is
    arithmetically true and entirely meaningless — exactly the flattering (here,
    alarming) measurement artifact a bench conclusion must never be drawn from.
    """
    write_run(
        tmp_path,
        [
            {
                "task": f"t{i}__X{i}",
                "reward": 0,
                "events": [{"ts": 1, "type": "text", "text": "hi"}],
            }
            for i in range(4)
        ],
        bare_loop=True,
    )
    for code in ("no-post-verdict-file-change", "witness-unavailable", "oracle-flip-ungraded"):
        assert _findings(tmp_path, code) == [], code


def test_agent_timeout_reads_harbors_own_classification(tmp_path):
    write_run(
        tmp_path,
        [
            {
                "task": "alpha__AAA",
                "reward": 0,
                "exception_class": "AgentTimeoutError",
                "exception": (
                    "harbor.trial.trial.AgentTimeoutError: "
                    "Agent execution timed out after 900.0 seconds"
                ),
                "events": [{"ts": 1, "type": "step_usage", "role": "worker", "model": "m"}],
            },
            {
                "task": "beta__BBB",
                "reward": 0,
                "exception_class": "VerifierTimeoutError",
                "exception": "harbor.trial.trial.VerifierTimeoutError",
                "events": [{"ts": 1, "type": "step_usage", "role": "worker", "model": "m"}],
            },
        ],
    )
    (finding,) = _findings(tmp_path, "agent-timeout")
    assert [o.task_id for o in finding.occurrences] == ["alpha__AAA"]
    assert "timed out after 900.0 seconds" in finding.occurrences[0].excerpt


# --------------------------------------------------------------------------
# Reporting discipline
# --------------------------------------------------------------------------


def test_a_single_occurrence_says_so_instead_of_quoting_a_rate(tmp_path):
    write_run(
        tmp_path,
        [{"task": f"t{i}__X{i}", "reward": 0, "events": []} for i in range(19)]
        + [
            {
                "task": "alpha__AAA",
                "reward": 0,
                "events": [
                    proof(
                        "witness_unavailable", reason="witness author produced no TEST_COMMAND line"
                    )
                ],
            }
        ],
    )
    (finding,) = _findings(tmp_path, "witness-unavailable")
    assert finding.denominator == 20
    assert "Single occurrence" in finding.rate_line()
    assert "5%" not in finding.rate_line()


def test_every_detector_declares_its_contract():
    from detectors import DETECTORS

    assert DETECTORS, "the registry is populated by importing the module"
    for det in DETECTORS:
        assert det.code and det.title, det
        assert det.search_terms, f"{det.code} needs terms for the de-duplication search"
        assert det.code == det.code.lower().replace(" ", "-")
