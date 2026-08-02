"""The turn deadline reaches the CLI, and is derived from the real one.

Split from `test_adapter.py`, which is over its recorded size ceiling
(`scripts/check-file-size.sh`); the repository's rule is that separable new
code becomes its own module. The subject is one seam —
`stella_harbor.turn_budget` and the two adapter methods that feed it.
"""

from __future__ import annotations

import pytest

from stella_harbor.turn_budget import (
    TEARDOWN_SEC,
    coerce_timeout_seconds,
    resolve_turn_budget,
)

from .test_adapter import _bare_agent


class TestTurnBudgetReachesTheEngine:
    """The turn deadline must reach the CLI, derived from the real one.

    The engine gained ``turn_budget`` in #1161 and a ``--turn-budget`` flag in
    #1170, and #1173 made declining for time complete the turn instead of
    aborting it. None of that could fire in a benchmark trial, because the
    adapter never passed the flag and Harbor 0.6.1 hands ``agent_timeout_sec``
    to the *oracle* agent only — an installed agent was never told the deadline
    it was running against. The policy shipped inert in the one place it was
    built for.

    Exporting ``STELLA_TURN_BUDGET`` was not a workaround: the fail-closed
    ambient check rejects any unrecognized ``STELLA_*`` name, so it refused the
    run rather than enabling the policy.
    """

    def test_the_flag_is_derived_from_harbors_own_per_trial_deadline(self) -> None:
        agent = _bare_agent()
        agent.model_name = "openrouter/z-ai/glm-5.2"
        agent._agent_timeout_sec = 900.0

        cmd = agent._build_command("Fix the bug")

        assert "--turn-budget" in cmd
        assert cmd[cmd.index("--turn-budget") + 1] == "840"

    def test_a_longer_task_gets_a_longer_budget_not_a_scaled_one(self) -> None:
        """Reserve is subtracted, never multiplied.

        Harbor's agent timeout is per-task — 900s and 1800s are both live in
        this dataset — and the comparator's winning ``regex-chess`` run took
        2,746s of wall clock. A fractional margin would throw away hundreds of
        seconds on exactly the long tasks that need them, which is the same
        defect as the global 840s in a different disguise.
        """
        agent = _bare_agent()
        agent.model_name = "openrouter/z-ai/glm-5.2"
        agent._agent_timeout_sec = 3600.0

        cmd = agent._build_command("Fix the bug")

        assert cmd[cmd.index("--turn-budget") + 1] == "3540"

    def test_no_known_deadline_omits_the_flag_entirely(self) -> None:
        """Absent is not zero, and it is not a guess.

        With no deadline the allowance stays a pure count — the behaviour
        before #1161. Inventing one would decline continuations the turn could
        afford, which is this policy's own failure mode inverted.
        """
        agent = _bare_agent()
        agent.model_name = "openrouter/z-ai/glm-5.2"

        cmd = agent._build_command("Fix the bug")

        assert "--turn-budget" not in cmd
        assert "" not in cmd

    def test_a_deadline_under_the_reserve_is_no_deadline_not_a_zero(self) -> None:
        agent = _bare_agent()
        agent.model_name = "openrouter/z-ai/glm-5.2"
        agent._agent_timeout_sec = 45.0

        cmd = agent._build_command("Fix the bug")

        assert "--turn-budget" not in cmd

    def test_harbors_deadline_outranks_an_operator_set_variable(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.setenv("STELLA_TURN_BUDGET", "120")
        agent = _bare_agent()
        agent.model_name = "openrouter/z-ai/glm-5.2"
        agent._agent_timeout_sec = 900.0

        cmd = agent._build_command("Fix the bug")

        assert cmd[cmd.index("--turn-budget") + 1] == "840"

    def test_the_variable_is_the_fallback_when_harbor_cannot_pass_a_kwarg(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.setenv("STELLA_TURN_BUDGET", "900")
        agent = _bare_agent()
        agent.model_name = "openrouter/z-ai/glm-5.2"

        cmd = agent._build_command("Fix the bug")

        assert cmd[cmd.index("--turn-budget") + 1] == "840"

    def test_the_variable_no_longer_refuses_the_run_and_stays_host_only(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """Registered, and registered in the *host-only* bucket.

        It is read here and expressed as argv, so forwarding it into the
        container would buy nothing and would re-open the empty-string parse
        trap that killed six trials on ``--budget``.
        """
        monkeypatch.setenv("STELLA_TURN_BUDGET", "900")
        agent = _bare_agent()
        agent.model_name = "openrouter/z-ai/glm-5.2"

        forwarded = agent._forwarded_env()  # must not raise

        assert "STELLA_TURN_BUDGET" not in forwarded

    def test_a_junk_deadline_is_ignored_rather_than_forwarded(self) -> None:
        """A malformed value must never become an argv element.

        ``--turn-budget`` is parsed by clap before the turn starts, so a bad
        value exits 2 and Harbor scores it identically to the agent failing
        the task — the #1018 / empty-budget shape, on a third variable.
        """
        for junk in ("", "   ", "abc", "-5", "0", None, True):
            agent = _bare_agent()
            agent.model_name = "openrouter/z-ai/glm-5.2"
            agent._agent_timeout_sec = junk

            cmd = agent._build_command("Fix the bug")

            assert "--turn-budget" not in cmd, junk


class TestResolveTurnBudgetIsPure:
    """The policy, exercised without an agent, a container, or a credential."""

    @pytest.mark.parametrize(
        "raw",
        ["", "   ", "abc", "-5", "0", None, True, False, "nan", "inf", "-inf"],
    )
    def test_a_value_that_is_not_a_positive_deadline_is_no_deadline(
        self, raw: object
    ) -> None:
        assert coerce_timeout_seconds(raw) is None

    @pytest.mark.parametrize(
        ("raw", "expected"),
        [("900", 900.0), (900, 900.0), (1800.5, 1800.5), (" 2746 ", 2746.0)],
    )
    def test_harbor_passes_strings_and_floats_and_both_parse(
        self, raw: object, expected: float
    ) -> None:
        assert coerce_timeout_seconds(raw) == expected

    def test_the_reserve_is_subtracted_so_it_does_not_scale_with_the_task(
        self,
    ) -> None:
        """The 900s and 1800s tasks keep the same reserve, not the same ratio.

        A multiplicative margin would cost the 1800s task 270s and the 2,746s
        one over 400s — precisely the wall clock the comparator spends
        finishing the tasks Stella truncates.
        """
        assert resolve_turn_budget(900) == f"{900 - TEARDOWN_SEC:g}"
        assert resolve_turn_budget(1800) == f"{1800 - TEARDOWN_SEC:g}"
        assert float(resolve_turn_budget(1800)) - float(
            resolve_turn_budget(900)
        ) == pytest.approx(900.0)

    def test_a_deadline_under_the_reserve_yields_none_not_zero(self) -> None:
        assert resolve_turn_budget(TEARDOWN_SEC) is None
        assert resolve_turn_budget(TEARDOWN_SEC - 1) is None
        assert resolve_turn_budget(TEARDOWN_SEC + 1) == "1"

    def test_the_kwarg_outranks_the_variable_and_the_variable_is_the_fallback(
        self,
    ) -> None:
        assert resolve_turn_budget(900, "120") == "840"
        assert resolve_turn_budget(None, "900") == "840"
        assert resolve_turn_budget(None, None) is None
        # A junk kwarg falls through to the variable rather than poisoning it.
        assert resolve_turn_budget("garbage", "900") == "840"
