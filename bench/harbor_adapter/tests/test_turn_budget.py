"""The turn deadline reaches the CLI, and is derived from the real one.

Split from `test_adapter.py`, which is over its recorded size ceiling
(`scripts/check-file-size.sh`); the repository's rule is that separable new
code becomes its own module. The subject is one seam —
`stella_harbor.turn_budget` and the two adapter methods that feed it.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

pytest.importorskip("harbor", reason="Harbor is required to import the adapter")

# The #2135 helpers are reached as `turn_budget.<name>` at each call site
# rather than imported by name, deliberately: a `from … import` of a symbol a
# tree does not have is a COLLECTION error, which fails the whole file at once
# and proves only that a name is missing. Reached this way, each test fails on
# its own — the behavioural witness on its assertion about the argv the adapter
# builds, which is the fact #2135 is about.
from stella_harbor import turn_budget  # noqa: E402 - after importorskip by design
from stella_harbor.turn_budget import (  # noqa: E402 - after importorskip by design
    TEARDOWN_SEC,
    coerce_timeout_seconds,
    resolve_turn_budget,
)

from .test_adapter import _bare_agent  # noqa: E402 - after importorskip by design


def _trial_tree(
    tmp_path: Path,
    *,
    task_timeout_sec: object = 900.0,
    trial_config: dict[str, object] | None = None,
) -> Path:
    """Lay out a Harbor single-step trial and return its agent ``logs_dir``.

    The shape Harbor itself writes (``harbor/models/trial/paths.py``): the
    resolved ``config.json`` at the trial root, the agent's log directory
    immediately beneath it, and the task definition wherever ``task.path``
    says. Built on disk rather than mocked because the thing under test is a
    file layout, and a mock of it would agree with whatever the code does.
    """
    task_dir = tmp_path / "task"
    task_dir.mkdir()
    if task_timeout_sec is not None:
        (task_dir / "task.toml").write_text(
            f'schema_version = "1.1"\n\n[agent]\ntimeout_sec = {task_timeout_sec}\n',
            encoding="utf-8",
        )
    config: dict[str, object] = {
        "task": {"path": str(task_dir)},
        "timeout_multiplier": 1.0,
        "agent": {"override_timeout_sec": None, "max_timeout_sec": None},
    }
    config.update(trial_config or {})
    trial_dir = tmp_path / "trial"
    logs_dir = trial_dir / "agent"
    logs_dir.mkdir(parents=True)
    (trial_dir / "config.json").write_text(json.dumps(config), encoding="utf-8")
    return logs_dir


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


class TestTheDeadlineArmsWithoutAnybodyPassingIt:
    """#2135's witness: no runner ever passed the kwarg, so nothing armed.

    ``BudgetGuard::set_task_deadline`` (#1481), its safe-boundary consumer in
    ``stella-core::driver::settlement`` (#1503) and the CLI's
    ``one_shot_budget_guard`` (#1507) were all built, wired and shipped. They
    were also completely inert on the bench path, because arming them needs a
    ``--turn-budget`` on argv, that flag needs a deadline, and the only two
    channels for one were things a *caller* had to remember:

    * ``--agent-kwarg agent_timeout_sec=…`` — every trial in match
      ``cc00894779ff`` records ``agent.kwargs: {}``, and ArenaBench's
      ``harbor run`` command line passes no ``--agent-kwarg`` at all.
    * ``STELLA_TURN_BUDGET`` — a static value, and wrong by construction for a
      dataset whose tasks carry 900s and 1800s deadlines.

    So ``resolve_turn_budget(None, None)`` returned ``None``, the flag was
    omitted, ``Config::turn_budget`` stayed ``None``, and every timeout in that
    match was Harbor's own SIGKILL rather than an engine-side stop.

    Harbor has always known the number, and writes it — with the inputs that
    produce it — to ``config.json`` at the trial root before the agent starts.
    These tests fail on a tree where the adapter does not read it.
    """

    def test_a_trial_arms_its_deadline_with_no_kwarg_and_no_variable(
        self, tmp_path: Path
    ) -> None:
        """The witness. Exactly the inputs `cobol-modernization__kt3nQsB` had."""
        agent = _bare_agent()
        agent.model_name = "openrouter/anthropic/claude-sonnet-5"
        agent.logs_dir = _trial_tree(tmp_path, task_timeout_sec=900.0)

        cmd = agent._build_command("Fix the bug")

        assert "--turn-budget" in cmd
        assert cmd[cmd.index("--turn-budget") + 1] == "840"

    def test_the_deadline_stays_per_task_across_a_mixed_dataset(
        self, tmp_path: Path
    ) -> None:
        """The reason a static variable could never be the answer.

        Terminal-Bench 2.1 runs 900s and 1800s tasks in one job. Discovery is
        per trial, so each gets its own deadline rather than the tightest one.
        """
        agent = _bare_agent()
        agent.model_name = "openrouter/anthropic/claude-sonnet-5"
        agent.logs_dir = _trial_tree(tmp_path, task_timeout_sec=1800.0)

        cmd = agent._build_command("Fix the bug")

        assert cmd[cmd.index("--turn-budget") + 1] == "1740"

    def test_an_explicit_kwarg_still_outranks_what_was_discovered(
        self, tmp_path: Path
    ) -> None:
        """Stated beats derived: an operator who says it means it."""
        agent = _bare_agent()
        agent.model_name = "openrouter/anthropic/claude-sonnet-5"
        agent.logs_dir = _trial_tree(tmp_path, task_timeout_sec=1800.0)
        agent._agent_timeout_sec = 900.0

        cmd = agent._build_command("Fix the bug")

        assert cmd[cmd.index("--turn-budget") + 1] == "840"

    def test_the_discovered_deadline_outranks_the_static_variable(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """The variable is the fallback for a host that has no trial to read."""
        monkeypatch.setenv("STELLA_TURN_BUDGET", "900")
        agent = _bare_agent()
        agent.model_name = "openrouter/anthropic/claude-sonnet-5"
        agent.logs_dir = _trial_tree(tmp_path, task_timeout_sec=1800.0)

        cmd = agent._build_command("Fix the bug")

        assert cmd[cmd.index("--turn-budget") + 1] == "1740"

    def test_the_armed_value_is_disclosed_in_the_trials_own_metadata(
        self, tmp_path: Path
    ) -> None:
        """A trace field, so triage never has to reconstruct argv again.

        Reading a timed-out trial's artifacts could not distinguish "the
        deadline was armed and the run blew through it" from "no deadline was
        ever armed" — the exact ambiguity that made #2135 a forensics job.
        """
        agent = _bare_agent()
        agent.model_name = "openrouter/anthropic/claude-sonnet-5"
        agent.logs_dir = _trial_tree(tmp_path, task_timeout_sec=900.0)
        agent._metrics = {"status": "completed", "steps": 1}
        agent._return_code = 0

        class _Ctx:
            cost_usd = None
            n_input_tokens = None
            n_output_tokens = None
            n_cache_tokens = None
            metadata = None

        ctx = _Ctx()
        agent.populate_context_post_run(ctx)

        assert ctx.metadata["stella_turn_budget_sec"] == "840"

    def test_an_unarmed_trial_says_so_rather_than_saying_nothing(self) -> None:
        """A word, not an omission — the ``stella_budget_usd`` discipline.

        Harbor's metadata dict drops ``None``, so an absent key would spell
        "no deadline was armed" and "this adapter predates the field" the same
        way, which is how the original question became unanswerable.
        """
        agent = _bare_agent()
        agent.model_name = "openrouter/anthropic/claude-sonnet-5"
        agent._metrics = {"status": "completed", "steps": 1}
        agent._return_code = 0

        class _Ctx:
            cost_usd = None
            n_input_tokens = None
            n_output_tokens = None
            n_cache_tokens = None
            metadata = None

        ctx = _Ctx()
        agent.populate_context_post_run(ctx)

        assert ctx.metadata["stella_turn_budget_sec"] == "unarmed"


class TestDiscoveryFailsOpenNeverGuesses:
    """A recomputation coupled to Harbor's version must degrade, not invent.

    Every one of these returns "no deadline known", which omits the flag and
    restores the pre-#1161 behaviour exactly. The alternative — a fallback
    constant — is the failure this module's header already names: a guessed
    deadline makes the engine decline continuations it could afford.
    """

    def test_no_logs_dir_at_all(self) -> None:
        assert turn_budget.discover_trial_deadline(None) is None
        assert turn_budget.discover_trial_deadline("") is None

    def test_a_trial_directory_with_no_config(self, tmp_path: Path) -> None:
        logs_dir = tmp_path / "trial" / "agent"
        logs_dir.mkdir(parents=True)
        assert turn_budget.discover_trial_deadline(logs_dir) is None

    def test_a_config_that_is_not_json(self, tmp_path: Path) -> None:
        logs_dir = _trial_tree(tmp_path)
        (logs_dir.parent / "config.json").write_text("{not json", encoding="utf-8")
        assert turn_budget.discover_trial_deadline(logs_dir) is None

    def test_a_task_that_is_not_on_this_host(self, tmp_path: Path) -> None:
        """A registry-resolved dataset records ``task.path: null``."""
        logs_dir = _trial_tree(
            tmp_path, trial_config={"task": {"path": None, "source": "registry"}}
        )
        assert turn_budget.discover_trial_deadline(logs_dir) is None

    def test_a_task_directory_with_no_task_toml(self, tmp_path: Path) -> None:
        logs_dir = _trial_tree(tmp_path, task_timeout_sec=None)
        assert turn_budget.discover_trial_deadline(logs_dir) is None

    def test_a_task_toml_that_is_not_toml(self, tmp_path: Path) -> None:
        logs_dir = _trial_tree(tmp_path)
        (tmp_path / "task" / "task.toml").write_text("[[[", encoding="utf-8")
        assert turn_budget.discover_trial_deadline(logs_dir) is None

    def test_a_multi_step_layout_reads_as_unknown_not_as_a_sibling_trial(
        self, tmp_path: Path
    ) -> None:
        """Only the immediate parent is consulted, on purpose.

        Walking further up eventually finds *some* configuration, and arming a
        deadline that belongs to a different run is worse than arming none.
        """
        logs_dir = _trial_tree(tmp_path)
        deeper = logs_dir.parent / "steps" / "step-1" / "agent"
        deeper.mkdir(parents=True)
        assert turn_budget.discover_trial_deadline(deeper) is None


class TestTrialDeadlineArithmeticMirrorsHarbor:
    """`harbor/trial/trial.py`'s own formula, pure and without a trial tree."""

    def test_the_task_timeout_is_the_base(self) -> None:
        assert turn_budget.trial_agent_deadline({}, 900.0) == 900.0

    def test_an_override_replaces_the_task_timeout(self) -> None:
        config = {"agent": {"override_timeout_sec": 1200.0}}
        assert turn_budget.trial_agent_deadline(config, 900.0) == 1200.0

    def test_a_cap_lowers_but_never_raises(self) -> None:
        lowered = {"agent": {"max_timeout_sec": 600.0}}
        raised = {"agent": {"max_timeout_sec": 3600.0}}
        assert turn_budget.trial_agent_deadline(lowered, 900.0) == 600.0
        assert turn_budget.trial_agent_deadline(raised, 900.0) == 900.0

    def test_the_agent_multiplier_wins_over_the_global_one(self) -> None:
        config = {"timeout_multiplier": 3.0, "agent_timeout_multiplier": 2.0}
        assert turn_budget.trial_agent_deadline(config, 900.0) == 1800.0
        global_only = {"timeout_multiplier": 3.0}
        assert turn_budget.trial_agent_deadline(global_only, 900.0) == 2700.0

    def test_no_stated_base_is_no_deadline(self) -> None:
        stated = {"timeout_multiplier": 2.0}
        assert turn_budget.trial_agent_deadline(stated, None) is None

    def test_a_stated_zero_multiplier_is_no_time_not_a_missing_multiplier(
        self,
    ) -> None:
        """Harbor selects the multiplier with ``is not None``.

        A stated ``0`` means the trial is killed on the instant. Reading it as
        "unstated" would arm a full-length deadline against a run that has
        none, so the two parsers are deliberately different.
        """
        instant = {"agent_timeout_multiplier": 0}
        assert turn_budget.trial_agent_deadline(instant, 900.0) == 0.0
        assert resolve_turn_budget(None, None, None) is None
        assert turn_budget.coerce_timeout_multiplier(0) == 0.0
        assert coerce_timeout_seconds(0) is None

    @pytest.mark.parametrize("raw", [None, True, False, "", "abc", "nan", "inf"])
    def test_a_multiplier_that_is_not_a_number_is_unstated(self, raw: object) -> None:
        assert turn_budget.coerce_timeout_multiplier(raw) is None


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
