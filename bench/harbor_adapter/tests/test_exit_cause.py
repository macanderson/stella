"""Tests for the SIGKILL (exit 137) post-mortem (#1178).

The pure classification in ``stella_harbor.exit_cause`` needs no Harbor; the
``run()`` integration tests exercise the same ``__new__``-constructed agent
and environment-hook pattern as ``test_adapter``, so a 137 exit is proven to
write ``stella-exit-cause.json`` and name its verdict in the official
``NonZeroAgentExitCodeError`` — the exception class Harbor scores, and the
one place a silent death used to leave nothing at all.
"""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from types import SimpleNamespace

import pytest

pytest.importorskip("harbor", reason="Harbor is required to import the adapter")

from harbor.agents.installed.base import NonZeroAgentExitCodeError  # noqa: E402
from harbor.models.agent.context import AgentContext  # noqa: E402

from stella_harbor import (  # noqa: E402 - after importorskip by design
    _INSTALL_PATH,
    SelfReportedVerificationFailureError,
    StellaAgent,
)
from stella_harbor.exit_cause import (  # noqa: E402 - after importorskip by design
    EXIT_CAUSE_LOG_NAME,
    SIGKILL_EXIT_CODE,
    build_exit_cause_report,
    classify_stream_end,
    parse_memory_events,
    sigkill_verdict,
)


@pytest.fixture(autouse=True)
def _clean_stella_environment(monkeypatch: pytest.MonkeyPatch) -> None:
    """Scrub ambient ``STELLA_*`` knobs so the adapter's fail-closed check
    judges the test's own environment, not whatever shell hosts the run."""
    import os

    for name in [name for name in os.environ if name.startswith("STELLA_")]:
        monkeypatch.delenv(name, raising=False)


class TestStreamEndClassification:
    def test_a_stream_ending_on_step_usage_is_a_step_boundary(self) -> None:
        end = classify_stream_end(
            [
                {"type": "stage", "name": "execute"},
                {"type": "text_delta", "text": "working"},
                {"type": "step_usage", "step": 0, "duration_ms": 1000},
            ]
        )
        assert end["classification"] == "step-boundary"
        assert end["ended_at_step_boundary"] is True
        assert end["last_event_type"] == "step_usage"
        assert end["events_seen"] == 3

    def test_a_stream_ending_mid_generation_is_mid_step(self) -> None:
        # The OOM shape: the kill lands during a step's work, so the last
        # thing the process lived to flush is mid-step activity.
        end = classify_stream_end(
            [
                {"type": "step_usage", "step": 0},
                {"type": "tool_start", "name": "bash"},
            ]
        )
        assert end["classification"] == "mid-step"
        assert end["ended_at_step_boundary"] is False

    def test_an_empty_stream_is_its_own_classification(self) -> None:
        assert classify_stream_end([])["classification"] == "no-events"
        assert classify_stream_end(None)["classification"] == "no-events"


class TestMemoryEvents:
    def test_cgroup_v2_counters_parse(self) -> None:
        parsed = parse_memory_events("low 0\nhigh 12\nmax 4\noom 1\noom_kill 1\n")
        assert parsed == {"low": 0, "high": 12, "max": 4, "oom": 1, "oom_kill": 1}

    def test_garbage_lines_are_skipped_not_fatal(self) -> None:
        # This parses whatever a possibly-dying container handed back.
        parsed = parse_memory_events("oom_kill 2\nnot a counter line\nhigh x\n")
        assert parsed == {"oom_kill": 2}
        assert parse_memory_events(None) == {}


class TestVerdict:
    def test_a_cgroup_oom_kill_is_an_oom_verdict_even_with_the_container_alive(
        self,
    ) -> None:
        # The subtle case #1178 calls out: the kernel kills the biggest
        # process (the agent) and the container keeps running, so
        # State.OOMKilled stays false. The cgroup counter is the evidence.
        verdict, detail = sigkill_verdict(
            container_state={"Running": True, "OOMKilled": False, "Status": "running"},
            memory_events={"oom_kill": 1},
        )
        assert verdict == "oom-kill"
        assert "oom_kill 1" in detail

    def test_a_container_level_oom_kill_is_an_oom_verdict(self) -> None:
        verdict, _ = sigkill_verdict(
            container_state={"Running": False, "OOMKilled": True, "Status": "exited"},
            memory_events={},
        )
        assert verdict == "oom-kill"

    def test_a_stopped_container_without_oom_evidence_is_external_teardown(
        self,
    ) -> None:
        verdict, detail = sigkill_verdict(
            container_state={"Running": False, "OOMKilled": False, "Status": "exited"},
            memory_events={},
        )
        assert verdict == "external-teardown"
        assert "exited" in detail

    def test_a_missing_container_is_external_teardown(self) -> None:
        # "Missing" means the probe looked and found nothing there, which is
        # what `container_absent` says. Absent that observation, a `None`
        # state means only that the probe could not see — see below.
        verdict, _ = sigkill_verdict(
            container_state=None, memory_events={}, container_absent=True
        )
        assert verdict == "external-teardown"

    def test_an_unobservable_container_is_unattributed_not_teardown(self) -> None:
        # A broken probe must never be reported as an affirmative cause: a
        # Docker hiccup during the post-mortem of a real OOM would otherwise
        # erase that OOM from the record and file it as infrastructure.
        verdict, detail = sigkill_verdict(container_state=None, memory_events={})
        assert verdict == "unattributed"
        assert "could not observe" in detail

    def test_oom_evidence_still_wins_over_an_unobservable_container(self) -> None:
        verdict, _ = sigkill_verdict(
            container_state=None, memory_events={"oom_kill": 1}
        )
        assert verdict == "oom-kill"

    def test_a_healthy_container_with_no_oom_events_is_unattributed(self) -> None:
        # "We looked and found nothing" is recorded, never guessed into a
        # more satisfying answer.
        verdict, _ = sigkill_verdict(
            container_state={"Running": True, "OOMKilled": False, "Status": "running"},
            memory_events={"oom_kill": 0},
        )
        assert verdict == "unattributed"


class TestReport:
    def test_the_report_carries_evidence_alongside_the_verdict(self) -> None:
        report = build_exit_cause_report(
            return_code=SIGKILL_EXIT_CODE,
            container_state={"Running": True, "OOMKilled": False, "Status": "running"},
            memory_events_text="oom 1\noom_kill 1\n",
            events=[{"type": "step_usage"}, {"type": "text_delta", "text": "…"}],
            probe_errors=[],
        )
        assert report["return_code"] == 137
        assert report["signal"] == "SIGKILL"
        assert report["verdict"] == "oom-kill"
        assert report["memory_events"] == {"oom": 1, "oom_kill": 1}
        assert report["stream_end"]["classification"] == "mid-step"
        assert report["probe_errors"] == []


def _stream_cut_mid_step() -> str:
    """A durable stream whose tail is mid-step work — no terminal event."""
    events = [
        {"type": "stage", "name": "execute"},
        {
            "type": "step_usage",
            "step": 0,
            "model": "anthropic/claude-sonnet-5",
            "input_tokens": 1000,
            "output_tokens": 200,
            "cost_usd": 0.02,
            "duration_ms": 900,
        },
        {"type": "tool_start", "name": "bash", "call_id": "call-1"},
    ]
    return "".join(json.dumps(event) + "\n" for event in events)


def _sigkilled_agent(tmp_path: Path) -> StellaAgent:
    agent = StellaAgent.__new__(StellaAgent)
    agent.logs_dir = tmp_path
    agent.model_name = "anthropic/claude-sonnet-5"
    agent._extra_env = {}
    agent._version = "stella 0.6.71"
    agent._binary_sha256 = "b" * 64
    agent._binary_sha256_verified = True
    agent._source_commit = "d" * 40
    agent._agent_timeout_sec = None
    return agent


class _SigkilledEnvironment:
    """A trial whose exec dies with 137 and whose probe finds a cgroup OOM."""

    def __init__(self) -> None:
        self.probed = False

    async def _stella_secure_exec_with_stdin(
        self, *, command: list[str], env: dict[str, str], stdin: bytes
    ) -> SimpleNamespace:
        assert command[0] == _INSTALL_PATH
        return SimpleNamespace(
            stdout=_stream_cut_mid_step(),
            stderr=None,
            return_code=SIGKILL_EXIT_CODE,
        )

    async def _stella_exit_cause_probe(self) -> dict[str, object]:
        self.probed = True
        return {
            "container_state": {
                "Running": True,
                "OOMKilled": False,
                "Status": "running",
            },
            "memory_events_text": "low 0\noom 1\noom_kill 1\n",
            "errors": [],
        }


class TestRunIntegration:
    def test_a_137_exit_writes_the_cause_artifact_and_names_the_verdict(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.setenv("ANTHROPIC_API_KEY", "anthropic-test-secret")
        agent = _sigkilled_agent(tmp_path)
        environment = _SigkilledEnvironment()

        with pytest.raises(NonZeroAgentExitCodeError) as raised:
            asyncio.run(
                StellaAgent.run.__wrapped__(
                    agent, "Fix the task.", environment, AgentContext()
                )
            )

        message = str(raised.value)
        assert "exited with code 137" in message
        assert "SIGKILL" in message, "the exception must say what 137 means"
        assert "oom-kill" in message, "the exception must carry the verdict"
        assert EXIT_CAUSE_LOG_NAME in message, "and point at the evidence"
        assert environment.probed, "the probe ran while the container existed"

        report = json.loads((tmp_path / EXIT_CAUSE_LOG_NAME).read_text())
        assert report["verdict"] == "oom-kill"
        assert report["signal"] == "SIGKILL"
        assert report["memory_events"]["oom_kill"] == 1
        assert report["stream_end"]["classification"] == "mid-step"
        assert report["stream_end"]["last_event_type"] == "tool_start"
        # The stream artifact itself is untouched by the diagnosis.
        assert (tmp_path / "stella-events.jsonl").read_text() == _stream_cut_mid_step()

    def test_a_broken_probe_never_displaces_the_scored_error(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.setenv("ANTHROPIC_API_KEY", "anthropic-test-secret")
        agent = _sigkilled_agent(tmp_path)

        class _BrokenProbeEnvironment(_SigkilledEnvironment):
            async def _stella_exit_cause_probe(self) -> dict[str, object]:
                raise RuntimeError("docker is gone")

        with pytest.raises(NonZeroAgentExitCodeError, match="exited with code 137"):
            asyncio.run(
                StellaAgent.run.__wrapped__(
                    agent, "Fix the task.", _BrokenProbeEnvironment(), AgentContext()
                )
            )
        assert not (tmp_path / EXIT_CAUSE_LOG_NAME).exists()

    def test_an_ordinary_nonzero_exit_gets_no_sigkill_diagnosis(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.setenv("ANTHROPIC_API_KEY", "anthropic-test-secret")
        agent = _sigkilled_agent(tmp_path)

        class _PlainFailureEnvironment(_SigkilledEnvironment):
            async def _stella_secure_exec_with_stdin(
                self, *, command: list[str], env: dict[str, str], stdin: bytes
            ) -> SimpleNamespace:
                return SimpleNamespace(
                    stdout=_stream_cut_mid_step(), stderr=None, return_code=7
                )

        environment = _PlainFailureEnvironment()
        with pytest.raises(NonZeroAgentExitCodeError, match="exited with code 7"):
            asyncio.run(
                StellaAgent.run.__wrapped__(
                    agent, "Fix the task.", environment, AgentContext()
                )
            )
        assert not environment.probed
        assert not (tmp_path / EXIT_CAUSE_LOG_NAME).exists()


def _stream_ending_in_a_verdict(*, passed: bool) -> str:
    """A turn that reached the end of its ladder and stated an outcome."""
    events: list[dict[str, object]] = [
        {"type": "stage", "name": "execute"},
        {
            "type": "step_usage",
            "step": 0,
            "model": "anthropic/claude-sonnet-5",
            "input_tokens": 900,
            "output_tokens": 120,
            "cost_usd": 0.01,
            "duration_ms": 700,
        },
        {
            "type": "verdict",
            "passed": passed,
            "summary": "the added test still fails",
            "evidence": {"deterministic": True},
        },
        {"type": "error", "message": "verification failed: the added test still fails"},
    ]
    return "".join(json.dumps(event) + "\n" for event in events)


class _VerificationFailedEnvironment(_SigkilledEnvironment):
    """A trial that ran to a verdict and exited 1 because the verdict was red."""

    def __init__(self, *, passed: bool = False, stream: str | None = None) -> None:
        super().__init__()
        self._stream = (
            stream if stream is not None else _stream_ending_in_a_verdict(passed=passed)
        )

    async def _stella_secure_exec_with_stdin(
        self, *, command: list[str], env: dict[str, str], stdin: bytes
    ) -> SimpleNamespace:
        return SimpleNamespace(stdout=self._stream, stderr=None, return_code=1)


class TestVerificationFailureIsNotACrash:
    """Exit 1 is both "the agent died" and "the agent reported honestly".

    A ``VerificationFailed`` verdict is a completed turn — the work ran, the
    ladder closed, and the run said the result did not verify. Recorded under
    the same exception class as a process that crashed, the two are one number
    in a results table, which is the conflation `portability` ended for loader
    failures.
    """

    def test_a_red_verdict_at_exit_one_gets_its_own_class(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.setenv("ANTHROPIC_API_KEY", "anthropic-test-secret")
        agent = _sigkilled_agent(tmp_path)
        context = AgentContext()

        with pytest.raises(SelfReportedVerificationFailureError) as raised:
            asyncio.run(
                StellaAgent.run.__wrapped__(
                    agent,
                    "Fix the task.",
                    _VerificationFailedEnvironment(passed=False),
                    context,
                )
            )

        # Still the class Harbor scores by: the trial is recorded exactly as
        # before and the benchmark verifier still runs. Only the name changes,
        # and the name is what `result.json` carries into an analysis.
        assert isinstance(raised.value, NonZeroAgentExitCodeError)
        assert "failed verification verdict" in str(raised.value)
        assert context.metadata["stella_self_verdict_state"] == "failed"

    def test_a_crash_at_the_same_exit_code_stays_the_generic_error(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.setenv("ANTHROPIC_API_KEY", "anthropic-test-secret")
        agent = _sigkilled_agent(tmp_path)

        with pytest.raises(NonZeroAgentExitCodeError) as raised:
            asyncio.run(
                StellaAgent.run.__wrapped__(
                    agent,
                    "Fix the task.",
                    _VerificationFailedEnvironment(stream=_stream_cut_mid_step()),
                    AgentContext(),
                )
            )

        # A stream that never reached a verdict made no claim, and "not
        # measured" must not be promoted to "measured and failed".
        assert type(raised.value) is NonZeroAgentExitCodeError

    def test_a_passing_verdict_that_still_exits_nonzero_stays_generic(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.setenv("ANTHROPIC_API_KEY", "anthropic-test-secret")
        agent = _sigkilled_agent(tmp_path)

        with pytest.raises(NonZeroAgentExitCodeError) as raised:
            asyncio.run(
                StellaAgent.run.__wrapped__(
                    agent,
                    "Fix the task.",
                    _VerificationFailedEnvironment(passed=True),
                    AgentContext(),
                )
            )

        # A green verdict that still exits nonzero is an anomaly, not a
        # self-report, and must keep the loud generic class.
        assert type(raised.value) is NonZeroAgentExitCodeError
