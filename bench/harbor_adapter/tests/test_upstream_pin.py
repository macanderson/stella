"""The gateway upstream pin reaches the CLI as an argument.

Its own module rather than a block in `test_adapter.py`, which is over its
recorded size ceiling (`scripts/check-file-size.sh`).

The subject is one seam and one property: trials run with
`STELLA_NO_SETTINGS=1`, so a settings-defined pin cannot reach a measured
trial at all — an argument is the only authority that survives, which is why
`--base-url` is one too. A pin that quietly failed to apply is worse than no
pin: the run would still record a provider id and read as controlled.
"""

from __future__ import annotations

import pytest

pytest.importorskip("harbor", reason="Harbor is required to import the adapter")

from stella_harbor import (  # noqa: E402 - after importorskip by design
    _HOST_ONLY_STELLA_ENV,
    _LAUNCHER_CONTROLS,
)
from stella_harbor.upstream_pin import (  # noqa: E402 - after importorskip by design
    validated_upstream_pin as _validated_upstream_pin,
)

from .test_adapter import _bare_agent  # noqa: E402 - after importorskip by design


def _agent_with(pin: str | None):
    """An agent whose only configured value is the pin under test."""
    agent = _bare_agent()
    agent.model_name = "openrouter/z-ai/glm-5.2"
    values = {"STELLA_UPSTREAM_PIN": pin} if pin is not None else {}
    agent._configured_value = values.get  # type: ignore[method-assign]
    return agent


class TestParsing:
    def test_an_order_is_preserved_and_blanks_dropped(self) -> None:
        assert _validated_upstream_pin("z-ai, anthropic ,") == ["z-ai", "anthropic"]

    def test_absent_and_empty_both_mean_unpinned(self) -> None:
        assert _validated_upstream_pin(None) == []
        assert _validated_upstream_pin("") == []
        assert _validated_upstream_pin("  ,  ") == []

    def test_an_entry_with_whitespace_is_refused(self) -> None:
        # The value is spliced into ONE argv element, so an entry containing a
        # separator would apply a different pin than the operator wrote —
        # exactly what a pin exists to prevent, arriving through the pin.
        with pytest.raises(ValueError, match="bare provider slug"):
            _validated_upstream_pin("z ai")


class TestArgv:
    def test_a_configured_pin_becomes_a_cli_argument(self) -> None:
        argv = _agent_with("z-ai")._build_command("do the thing")
        assert "--upstream-pin" in argv
        assert argv[argv.index("--upstream-pin") + 1] == "z-ai"

    def test_an_order_rides_as_one_comma_separated_argument(self) -> None:
        argv = _agent_with("z-ai,anthropic")._build_command("do the thing")
        assert argv[argv.index("--upstream-pin") + 1] == "z-ai,anthropic"

    def test_an_unpinned_run_passes_no_flag_at_all(self) -> None:
        # Not `--upstream-pin ""`: an empty value is a malformed argument, and
        # an unpinned request must also keep its pre-field wire bytes, since
        # the request body is the prompt-cache key.
        assert "--upstream-pin" not in _agent_with(None)._build_command("go")

    def test_the_flag_precedes_the_run_subcommand(self) -> None:
        # `--upstream-pin` is a global flag; after the subcommand token clap
        # would reject it, which exits 2 before the agent starts.
        argv = _agent_with("z-ai")._build_command("go")
        assert argv.index("--upstream-pin") < argv.index("run")

    def test_the_instruction_still_binds_after_the_end_of_options(self) -> None:
        argv = _agent_with("z-ai")._build_command("- a dashed instruction")
        assert argv[-2:] == ["--", "- a dashed instruction"]


class TestRegistration:
    def test_the_pin_is_registered_host_only(self) -> None:
        # The ambient check fails closed: exported but unregistered refuses the
        # run rather than enabling the policy (an unlisted STELLA_TURN_BUDGET
        # once killed all ten trials of a run).
        assert "STELLA_UPSTREAM_PIN" in _HOST_ONLY_STELLA_ENV

    def test_the_launcher_declares_where_the_pin_comes_from(self) -> None:
        assert _LAUNCHER_CONTROLS["upstream_pin_authority"] == "validated-cli-argument"
