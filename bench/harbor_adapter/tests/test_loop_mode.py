"""Which loop an arm ran: the selector, the argv it produces, and the manifest.

Three levels, because the bug this guards against can enter at any of them:
the selector could accept a word spelled to close, the argv could drop a flag
the match declared, or the manifest could publish a mode the command never
ran. The last is the one nothing would have caught — a manifest is read long
after the process that wrote it is gone.

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
    STAGED_PIPELINE,
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
    def test_both_projections_derive_from_the_one_selection(
        self, value: str | None
    ) -> None:
        """argv and manifest name are two views of one boolean, never two reads."""
        selected = bare_loop_selected(_reader(value))
        assert loop_argv(_reader(value)) == (("--no-pipeline",) if selected else ())
        assert loop_mode_name(_reader(value)) == (
            BARE_STEP_LOOP if selected else STAGED_PIPELINE
        )


class TestArgv:
    """The flag has to reach the command line, in the right position."""

    def test_bare_loop_arm_passes_no_pipeline_after_the_run_token(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """The bare-loop arm exists at all: `--no-pipeline` reaches argv.

        Without it there is no way to measure the raw step loop, so a match
        comparing agent architectures silently measured the pipeline instead.
        """
        cmd = _agent(monkeypatch, "1")._build_command("Fix the bug")

        assert "--no-pipeline" in cmd
        # A flag OF `run`, never a global — clap rejects it before the subcommand.
        assert cmd.index("--no-pipeline") > cmd.index("run")
        assert cmd[-5:] == [
            "--no-pipeline",
            "--output-format",
            "stream-json",
            "--",
            "Fix the bug",
        ]

    @pytest.mark.parametrize("value", SPELLED_OFF)
    def test_the_pipeline_is_the_default_and_stays_byte_identical(
        self, monkeypatch: pytest.MonkeyPatch, value: str | None
    ) -> None:
        """Absent or explicitly-off leaves the shipping configuration running.

        Asserting the exact tail, not just the flag's absence: the default
        arm's argv must be what it was before this arm existed, or every
        already-published pipeline result is measuring something new.
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
        authored a witness.
        """
        agent = _agent(monkeypatch, value)
        flagged = "--no-pipeline" in agent._build_command("Fix the bug")

        mode = self._loop_mode(agent)

        assert mode == (BARE_STEP_LOOP if flagged else STAGED_PIPELINE)
        # And pinned absolutely, so a rename of both sides at once still fails.
        assert mode == (BARE_STEP_LOOP if is_truthy(value) else STAGED_PIPELINE)


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
