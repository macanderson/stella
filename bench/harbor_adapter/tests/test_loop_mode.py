"""Which loop an arm ran: the selector, the argv it produces, and the manifest.

Three levels, because the bug this guards against can enter at any of them:
the selector could accept a word spelled to close, the argv could emit a flag
that no longer does anything (#4023: `--no-pipeline` is a deprecated no-op
post-#3865), or the manifest could publish a mode the command never ran. The
last is the one nothing would have caught before #4023 — a manifest is read
long after the process that wrote it is gone, and every published Stella arm
was claiming `staged_pipeline` for a binary with no staged pipeline to run.

Lives beside ``test_adapter.py`` rather than inside it for the reason
:mod:`stella_harbor.loop_mode` lives beside the package root: both files sit
on the repository's file-size ratchet, and separable new code becomes its own
module rather than more length on an already-oversized one.
"""

from __future__ import annotations

import pytest

pytest.importorskip("harbor", reason="Harbor is required to import the adapter")

from stella_harbor import StellaAgent  # noqa: E402
from stella_harbor.loop_mode import (  # noqa: E402
    BARE_STEP_LOOP,
    NO_PIPELINE_ENV,
    bare_loop_selected,
    is_truthy,
    loop_argv,
    loop_mode_name,
)

#: Every spelling Stella itself treats as true (`settings.rs::truthy_flag`).
SPELLED_ON = ("1", "true", "TRUE", "  True  ", "yes", "on")

#: Absent, empty, and every way of saying no. ``"false"`` is the one that
#: matters: a plain truthiness test would read it as ``True`` because it is a
#: non-empty string, and the arm would silently run the loop it declined.
SPELLED_OFF = (None, "", "  ", "0", "false", "False", "no", "off", "nope")


def _reader(value: str | None):
    """A stand-in for ``StellaAgent._configured_value`` over one key."""
    return lambda key: value if key == NO_PIPELINE_ENV else None


def _agent(monkeypatch: pytest.MonkeyPatch, value: str | None) -> StellaAgent:
    """An agent whose only configured input is the loop selector."""
    if value is None:
        monkeypatch.delenv(NO_PIPELINE_ENV, raising=False)
    else:
        monkeypatch.setenv(NO_PIPELINE_ENV, value)
    agent = StellaAgent.__new__(StellaAgent)
    agent.model_name = "openrouter/anthropic/claude-opus-5"
    return agent


class TestSelector:
    """The pure half: one string in, a loop mode out."""

    @pytest.mark.parametrize("value", SPELLED_ON)
    def test_stellas_truthy_vocabulary_selects_the_bare_loop(self, value: str) -> None:
        assert is_truthy(value)
        assert bare_loop_selected(_reader(value))

    @pytest.mark.parametrize("value", SPELLED_OFF)
    def test_a_selector_spelled_to_close_does_not_open(self, value: str | None) -> None:
        """`STELLA_NO_PIPELINE=false` must leave the pipeline ON.

        The same discipline `STELLA_TRUST_PROJECT` learned the hard way: a flag
        whose value says no must not be read as yes merely for being present.
        """
        assert not is_truthy(value)
        assert not bare_loop_selected(_reader(value))

    @pytest.mark.parametrize("value", SPELLED_ON + SPELLED_OFF)
    def test_both_projections_agree_the_bare_loop_is_all_there_is(
        self, value: str | None
    ) -> None:
        """argv and manifest name agree regardless of the declared selection.

        Post-#3865 there is no staged pipeline for either projection to name
        — see :mod:`stella_harbor.loop_mode`'s module docstring — so both are
        constant across the whole selector vocabulary (#4023). The selector
        itself (:func:`bare_loop_selected`) still varies; it is asserted here
        precisely so a future reader can see that its variation no longer
        reaches either projection below.
        """
        selected = bare_loop_selected(_reader(value))
        assert selected == is_truthy(value)
        assert loop_argv(_reader(value)) == ()
        assert loop_mode_name(_reader(value)) == BARE_STEP_LOOP


class TestArgv:
    """`--no-pipeline` must never reach the command line (#4023)."""

    @pytest.mark.parametrize("value", SPELLED_ON + SPELLED_OFF)
    def test_no_arm_emits_the_deprecated_flag_and_argv_stays_byte_identical(
        self, monkeypatch: pytest.MonkeyPatch, value: str | None
    ) -> None:
        """Even a `bare_loop = true` arm's argv is the plain-`run` tail.

        `--no-pipeline` is a deprecated no-op post-#3865 — every invocation
        already runs the raw step loop, flag or not — and emitting it only
        earns the trial a deprecation notice on every run
        (`print_no_pipeline_notice_if_owed`) for a difference that does not
        exist. Asserting the exact tail, not just the flag's absence: this
        arm's argv must be what a plain `stella run` invocation is, or a
        published result is measuring something the command line does not say.
        """
        cmd = _agent(monkeypatch, value)._build_command("Fix the bug")

        assert "--no-pipeline" not in cmd
        tail = ["run", "--output-format", "stream-json", "--", "Fix the bug"]
        assert cmd[-5:] == tail


class TestManifest:
    """The published record must name the loop the command actually ran."""

    @staticmethod
    def _loop_mode(agent: StellaAgent) -> str:
        agent._metrics = {
            "cost_usd": None,
            "n_input_tokens": None,
            "n_output_tokens": None,
            "n_cache_tokens": None,
            "status": "completed",
            "model": None,
            "steps": None,
        }
        agent._return_code = 0

        class _Ctx:
            cost_usd = None
            n_input_tokens = None
            n_output_tokens = None
            n_cache_tokens = None
            metadata = None

        ctx = _Ctx()
        agent.populate_context_post_run(ctx)
        return ctx.metadata["stella_loop_mode"]

    @pytest.mark.parametrize("value", SPELLED_ON + SPELLED_OFF)
    def test_the_manifest_cannot_disagree_with_the_argv_that_ran(
        self, monkeypatch: pytest.MonkeyPatch, value: str | None
    ) -> None:
        """The invariant, stated over the whole vocabulary in both directions.

        A manifest is read long after the process that wrote it exits, so a
        mode that disagrees with the command is not a stale label — it is a
        published result claiming to have measured an arm it never ran. #2134
        was that shape: a record read as a pipeline run that merely never
        authored a witness; #4023 was the same shape in the other direction —
        a manifest naming a loop (`staged_pipeline`) the binary structurally
        cannot run at all.
        """
        agent = _agent(monkeypatch, value)
        flagged = "--no-pipeline" in agent._build_command("Fix the bug")

        mode = self._loop_mode(agent)

        assert not flagged, "no arm may emit the deprecated flag (#4023)"
        assert mode == BARE_STEP_LOOP

    @pytest.mark.parametrize("value", SPELLED_ON + SPELLED_OFF)
    def test_the_manifest_agrees_with_pipelinechoice_resolve(
        self, monkeypatch: pytest.MonkeyPatch, value: str | None
    ) -> None:
        """The assertion #4023 says was missing: manifest name vs CLI reality.

        `crates/stella-cli/src/wrapper_plugin.rs::PipelineChoice::resolve`
        refuses `--pipeline classic` outright and, with no `--pipeline` flag
        at all — which is all this adapter ever passes — resolves every
        remaining case to the raw step loop. That Rust decision cannot be
        called from here, so this pins the one fact this suite can check
        instead: this adapter never emits `--pipeline`, so `resolve` always
        lands on the raw step loop, and the manifest must say exactly that
        for every value the loop selector can take.
        """
        agent = _agent(monkeypatch, value)
        cmd = agent._build_command("Fix the bug")

        assert "--pipeline" not in cmd, (
            "this adapter names no plugin, so PipelineChoice::resolve sees "
            "no --pipeline flag and cannot land anywhere but the raw step loop"
        )
        assert self._loop_mode(agent) == BARE_STEP_LOOP


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
