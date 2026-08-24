"""The de-duplication decision and the dry-run default.

The requirement this file exists to hold: **the output is never a duplicate
issue.** Everything below is one of the ways that can fail — a fingerprint seen
twice, a ledger that was lost, a closed issue that came back, a search that did
not run — with a fake `gh` so no test reaches the network.
"""

from __future__ import annotations

from typing import Any

import pytest
import triage_bench_traces
from detectors import run_all
from fingerprint import Ledger, fingerprint_of, markers_in, normalize
from fixtures import proof, write_run
from issues import Decision, apply_actions, plan_actions
from run_trace import load_run

REASON = "witness author produced no TEST_COMMAND line"


class FakeGitHub:
    """A `gh` that records rather than calls."""

    def __init__(
        self, hits: list[dict[str, Any]] | None = None, states: dict[int, str] | None = None
    ):
        self.hits = hits or []
        self.states = states or {}
        self.bodies: dict[int, str] = {}
        self.created: list[tuple[str, str]] = []
        self.comments: list[tuple[int, str]] = []
        self.next_number = 5000
        self.search_raises = False

    def search_issues(self, terms):
        if self.search_raises:
            raise RuntimeError("gh search issues failed: rate limited")
        return list(self.hits)

    def view_issue(self, number: int):
        return {
            "number": number,
            "title": f"issue {number}",
            "state": self.states.get(number, "OPEN"),
            "body": self.bodies.get(number, ""),
        }

    def create_issue(self, title: str, body: str) -> int:
        number = self.next_number
        self.next_number += 1
        self.created.append((title, body))
        self.bodies[number] = body
        self.states[number] = "OPEN"
        return number

    def comment_issue(self, number: int, body: str) -> None:
        self.comments.append((number, body))


def _witness_failure_events(reason: str = REASON) -> list:
    """One witness failure, and nothing else a detector would also fire on.

    Three of these four events are required to keep that "nothing else" true, and
    each was added after a detector fired on the fixture rather than on the
    defect it describes:

    * the trailing `verdict` and post-verdict `file_change` — without them the
      trial also trips `no-post-verdict-file-change`;
    * the leading `step_usage` — without it the trial has completed zero model
      calls, and a graded trial that never reached the model trips
      `graded-void-trial`. It also makes the fixture agree with the wire, which
      is the standing rule in `fixtures.py`: a trial cannot emit a `proof` and a
      `verdict` without a model call having completed first, so a fixture with a
      proof and no `step_usage` was describing a trace that cannot exist.

    Its model is the unqualified tail of `write_run`'s default declared worker,
    which is how the telemetry spells it, so the census detector stays quiet.
    """
    return [
        {
            "ts": 1,
            "type": "step_usage",
            "step": 0,
            "role": "worker",
            "model": "z-ai/glm-5.2",
            "complete": True,
        },
        proof("witness_unavailable", reason=reason),
        {"ts": 8, "type": "verdict", "passed": False, "evidence": {}},
        {"ts": 9, "type": "file_change", "path": "out", "kind": "created"},
    ]


def _run_with_witness_failure(tmp_path, name: str, tasks: list[str], run_id: str):
    root = tmp_path / name
    write_run(root, [{"task": t, "reward": 0, "events": _witness_failure_events()} for t in tasks])
    return load_run(root, run_id)


def _ledger(tmp_path) -> Ledger:
    ledger = Ledger(tmp_path / "ledger.json")
    return ledger


# --------------------------------------------------------------------------
# The headline requirement
# --------------------------------------------------------------------------


def test_the_same_fingerprint_twice_opens_one_issue_and_then_comments(tmp_path):
    """Two runs, one defect: issue first, comment second. Never two issues."""
    client = FakeGitHub()
    ledger = _ledger(tmp_path)

    first = _run_with_witness_failure(tmp_path, "r1", ["alpha__AAA"], "run-one")
    actions = plan_actions(first, run_all(first), ledger, client, max_new_issues=5)
    assert [a.decision for a in actions] == [Decision.NEW]
    apply_actions(first, actions, ledger, client)
    assert len(client.created) == 1
    issue_number = 5000

    second = _run_with_witness_failure(tmp_path, "r2", ["beta__BBB"], "run-two")
    actions = plan_actions(second, run_all(second), ledger, client, max_new_issues=5)
    assert [a.decision for a in actions] == [Decision.COMMENT]
    assert actions[0].issue == issue_number
    assert actions[0].matched_by == "ledger"
    apply_actions(second, actions, ledger, client)

    assert len(client.created) == 1, "the second occurrence must not open a second issue"
    assert len(client.comments) == 1
    assert client.comments[0][0] == issue_number


def test_the_fingerprint_survives_a_lost_ledger_via_the_issue_marker(tmp_path):
    """The ledger is a cache of a fact GitHub also holds. Losing it is recoverable."""
    client = FakeGitHub()
    ledger = _ledger(tmp_path)
    first = _run_with_witness_failure(tmp_path, "r1", ["alpha__AAA"], "run-one")
    actions = plan_actions(first, run_all(first), ledger, client, max_new_issues=5)
    apply_actions(first, actions, ledger, client)
    body = client.created[0][1]
    assert markers_in(body), "an opened issue must carry its fingerprint marker"

    # Wipe the ledger; the search now has to carry the de-duplication alone.
    fresh = Ledger(tmp_path / "gone.json")
    client.hits = [{"number": 5000, "title": "completely different words", "state": "OPEN"}]
    second = _run_with_witness_failure(tmp_path, "r2", ["beta__BBB"], "run-two")
    actions = plan_actions(second, run_all(second), fresh, client, max_new_issues=5)
    assert actions[0].decision is Decision.COMMENT
    assert actions[0].matched_by == "fingerprint marker in the issue body"


def test_a_closed_issue_that_recurs_is_a_comment_not_a_new_ticket(tmp_path):
    client = FakeGitHub(states={4242: "CLOSED"})
    ledger = _ledger(tmp_path)
    run = _run_with_witness_failure(tmp_path, "r1", ["alpha__AAA"], "run-one")
    finding = run_all(run)[0]
    ledger.bind(finding.fingerprint, 4242, "older-run", source="seed")

    (action,) = plan_actions(run, [finding], ledger, client, max_new_issues=5)
    assert action.decision is Decision.RECURRENCE
    assert action.issue == 4242
    apply_actions(run, [action], ledger, client)
    assert client.created == [], "a recurring closed issue must never become a new ticket"
    assert client.comments[0][0] == 4242

    body = client.comments[0][1]
    assert "RECURRENCE" in body
    # The comment must not assert a regression it cannot establish from a trace.
    assert "Not decided here" in body
    assert "gh issue reopen" in body


def test_a_search_that_did_not_run_blocks_a_new_issue(tmp_path):
    """A failed search is not an empty search. Fail closed."""
    client = FakeGitHub()
    client.search_raises = True
    ledger = _ledger(tmp_path)
    run = _run_with_witness_failure(tmp_path, "r1", ["alpha__AAA"], "run-one")
    (action,) = plan_actions(run, run_all(run), ledger, client, max_new_issues=5)
    assert action.decision is Decision.SUPPRESSED
    assert "search unavailable" in action.matched_by
    apply_actions(run, [action], ledger, client)
    assert client.created == []


def test_a_repeat_that_adds_nothing_posts_nothing(tmp_path):
    """A comment restating the issue is noise on someone else's ticket."""
    client = FakeGitHub()
    ledger = _ledger(tmp_path)
    run = _run_with_witness_failure(tmp_path, "r1", ["alpha__AAA"], "run-one")
    actions = plan_actions(run, run_all(run), ledger, client, max_new_issues=5)
    apply_actions(run, actions, ledger, client)

    again = plan_actions(run, run_all(run), ledger, client, max_new_issues=5)
    assert again[0].decision is Decision.NO_NEWS
    apply_actions(run, again, ledger, client)
    assert client.comments == []

    forced = plan_actions(run, run_all(run), ledger, client, max_new_issues=5, comment_anyway=True)
    assert forced[0].decision is Decision.COMMENT


def test_the_same_task_under_a_fresh_trial_suffix_is_not_news(tmp_path):
    """`code-from-image__AAA` and `code-from-image__ZZZ` are one task, two attempts.

    Harbor mints the suffix per trial, so keying novelty on the raw id makes
    every task of a rerun read as newly affected — the loudest possible way to
    report nothing, on someone else's issue.
    """
    client = FakeGitHub()
    ledger = _ledger(tmp_path)
    first = _run_with_witness_failure(tmp_path, "r1", ["code-from-image__AAA"], "run-one")
    apply_actions(first, plan_actions(first, run_all(first), ledger, client, 5), ledger, client)

    second = _run_with_witness_failure(tmp_path, "r2", ["code-from-image__ZZZ"], "run-two")
    (action,) = plan_actions(second, run_all(second), ledger, client, 5)
    assert not any("code-from-image" in item for item in action.novelty)
    # The run itself is still news — that part is real.
    assert any("run-two" in item for item in action.novelty)


def test_a_new_task_is_news_and_says_which(tmp_path):
    client = FakeGitHub()
    ledger = _ledger(tmp_path)
    first = _run_with_witness_failure(tmp_path, "r1", ["alpha__AAA"], "run-one")
    apply_actions(first, plan_actions(first, run_all(first), ledger, client, 5), ledger, client)

    second = _run_with_witness_failure(tmp_path, "r2", ["gamma__GGG"], "run-two")
    (action,) = plan_actions(second, run_all(second), ledger, client, 5)
    assert action.decision is Decision.COMMENT
    assert any("`gamma`" in item for item in action.novelty)
    assert any("run-two" in item for item in action.novelty)


def test_the_new_issue_cap_suppresses_loudly_rather_than_truncating(tmp_path):
    client = FakeGitHub()
    ledger = _ledger(tmp_path)
    root = tmp_path / "many"
    # Distinct WORDS, not distinct digits: the normalizer erases digits on
    # purpose, so `failure 1` and `failure 2` are one defect and would make this
    # test assert the opposite of the design.
    reasons = [
        "witness author produced no TEST_COMMAND line",
        "witness author emitted a test command but created no file",
        "witness author turn aborted: the model returned an empty response",
        "witness author turn aborted: stuck-loop detected after a steering warning",
    ]
    write_run(
        root,
        [
            {"task": f"t{i}__X{i}", "reward": 0, "events": _witness_failure_events(reason)}
            for i, reason in enumerate(reasons)
        ],
    )
    run = load_run(root, "run-many")
    findings = run_all(run)
    assert len(findings) == 4, "four distinct reasons are four distinct defects"

    actions = plan_actions(run, findings, ledger, client, max_new_issues=2)
    assert sum(a.decision is Decision.NEW for a in actions) == 2
    assert sum(a.decision is Decision.SUPPRESSED for a in actions) == 2
    log = apply_actions(run, actions, ledger, client)
    assert len(client.created) == 2
    assert sum("suppressed (cap)" in line for line in log) == 2, "suppression is logged, not silent"


def test_a_human_correction_in_the_ledger_is_never_overwritten(tmp_path):
    ledger = _ledger(tmp_path)
    fingerprint = fingerprint_of("d", "s", "v")
    ledger.bind(fingerprint, 111, "run-one", note="corrected by hand", source="human")
    ledger.bind(fingerprint, 222, "run-two")
    assert ledger.lookup(fingerprint).issue == 111


# --------------------------------------------------------------------------
# Fingerprint stability
# --------------------------------------------------------------------------


@pytest.mark.parametrize(
    "left,right",
    [
        # The same defect, different runs: workspace root, digits, quoted nouns.
        (
            "candidate wrote outside `/tmp/stella_candidate_460_0/out.toml` after 12 steps",
            "candidate wrote outside `/tmp/stella_candidate_77_3/other.json` after 4109 steps",
        ),
        # The same defect, different quoted tool name.
        (
            "stuck-loop detected: the same `create_witness_test` call was issued 4 times",
            "stuck-loop detected: the same `apply_edits` call was issued 9 times",
        ),
    ],
)
def test_run_varying_tokens_do_not_change_the_fingerprint(left, right):
    assert normalize(left) == normalize(right)
    assert fingerprint_of("d", "s", left).digest == fingerprint_of("d", "s", right).digest


def test_genuinely_different_reasons_do_not_collide():
    a = "witness author produced no TEST_COMMAND line"
    b = "witness author emitted a test command but created no file"
    assert (
        fingerprint_of("witness-unavailable", "x", a).digest
        != fingerprint_of("witness-unavailable", "x", b).digest
    )


def test_the_site_is_part_of_the_identity():
    assert fingerprint_of("d", "a.rs", "v").digest != fingerprint_of("d", "b.rs", "v").digest


def test_the_ledger_round_trips_through_disk(tmp_path):
    path = tmp_path / "l.json"
    ledger = Ledger(path)
    fingerprint = fingerprint_of("d", "s", "v")
    entry = ledger.bind(fingerprint, 9, "run-one", note="n")
    entry.observe("run-one", ["a__A"], 3, 20)
    ledger.save()

    reloaded = Ledger(path)
    again = reloaded.lookup(fingerprint)
    assert again.issue == 9
    assert again.runs == ["run-one"]
    assert again.peak_count == 3 and again.peak_denominator == 20


# --------------------------------------------------------------------------
# The dry-run default
# --------------------------------------------------------------------------


def test_the_default_invocation_writes_nothing(tmp_path, monkeypatch, capsys):
    """No `--apply`: the plan is printed and neither GitHub nor the ledger moves."""
    root = tmp_path / "runx"
    write_run(root, [{"task": "alpha__AAA", "reward": 0, "events": _witness_failure_events()}])
    ledger_path = tmp_path / "ledger.json"
    client = FakeGitHub()
    monkeypatch.setattr(triage_bench_traces, "GhCli", lambda repo: client)

    code = triage_bench_traces.main(
        ["--run", "runx", "--mirror", str(root), "--ledger", str(ledger_path)]
    )
    out = capsys.readouterr().out

    assert code == 0
    assert client.created == [] and client.comments == []
    assert not ledger_path.exists(), "a dry run must not touch the tracked ledger"
    assert "DRY RUN" in out
    assert "body it would post" in out
    assert REASON in out, "the plan must show the exact text that would be posted"


def test_apply_is_the_only_thing_that_writes(tmp_path, monkeypatch, capsys):
    root = tmp_path / "runx"
    write_run(root, [{"task": "alpha__AAA", "reward": 0, "events": _witness_failure_events()}])
    ledger_path = tmp_path / "ledger.json"
    client = FakeGitHub()
    monkeypatch.setattr(triage_bench_traces, "GhCli", lambda repo: client)

    triage_bench_traces.main(
        ["--run", "runx", "--mirror", str(root), "--ledger", str(ledger_path), "--apply"]
    )
    assert len(client.created) == 1
    assert ledger_path.exists()
    assert "DRY RUN" not in capsys.readouterr().out


def test_offline_cannot_write(tmp_path):
    root = tmp_path / "runx"
    write_run(root, [{"task": "alpha__AAA", "reward": 0, "events": []}])
    with pytest.raises(SystemExit):
        triage_bench_traces.main(["--run", "runx", "--mirror", str(root), "--offline", "--apply"])
