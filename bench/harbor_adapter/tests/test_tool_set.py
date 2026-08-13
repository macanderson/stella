"""Which tools an arm advertised: validation, forwarding, disclosure, refusal.

Four levels, because the experiment this enables (#3032) is ruined at any of
them: the parser could accept a word that resolves to a set nobody recorded,
the forwarder could drop a set the match declared, the manifest could publish
a set the container never received, or the audited claim path could quietly
run a restricted arm and publish the number as the shipping configuration.

The first test is the one that protects every measurement taken before this
existed: an arm that declares no tool set must produce a byte-identical
container environment. Everything else here is new behaviour; that one is the
promise that nothing old moved.

Lives beside ``test_adapter.py`` rather than inside it for the reason
:mod:`stella_harbor.tool_set` lives beside the package root: both files sit on
the repository's file-size ratchet, and separable new code becomes its own
module rather than more length on an already-oversized one.
"""

from __future__ import annotations

import pytest

pytest.importorskip("harbor", reason="Harbor is required to import the adapter")

from stella_harbor import (  # noqa: E402 - after importorskip by design
    _CLAIM_CONTAINER_ENV,
    _LAUNCHER_CONTROLS,
)
from stella_harbor.tool_set import (  # noqa: E402 - after importorskip by design
    ENV,
    declared,
    tool_set_env,
    validated_tool_set,
)

from .test_adapter import _bare_agent  # noqa: E402 - after importorskip by design

#: A plausible baseline arm, spelled with surviving catalog names — the
#: four-tool arm #3032 originally proposed predates the 2026-08 tool purge.
_BASELINE = "task_list,save_state,get_state,get_environment"


def _reader(value: str | None):
    """A stand-in for ``StellaAgent._configured_value`` over one key."""
    return lambda key, default=None: value if key == ENV else default


def _agent_with(value: str | None):
    """An agent whose only configured value is the tool set under test."""
    agent = _bare_agent()
    agent.model_name = "openrouter/z-ai/glm-5.2"
    agent._configured_value = _reader(value)  # type: ignore[method-assign]
    return agent


class TestParsing:
    def test_a_declared_set_is_order_preserving_and_deduplicated(self) -> None:
        assert validated_tool_set(" task_list, save_state ,task_list,") == (
            "task_list",
            "save_state",
        )

    def test_absent_and_empty_both_mean_the_shipping_catalog(self) -> None:
        assert validated_tool_set(None) == ()
        assert validated_tool_set("") == ()
        assert validated_tool_set("  ,  ") == ()

    @pytest.mark.parametrize("preset", ["1", "true", "TRUE", "0", "false"])
    def test_a_preset_is_refused_rather_than_resolved(self, preset: str) -> None:
        # Valid to Stella, refused here: the tool set is this experiment's
        # independent variable, and an arm recorded as `1` is an arm whose
        # tools have to be recovered by reading whichever binary it launched.
        with pytest.raises(ValueError, match="preset"):
            validated_tool_set(preset)

    @pytest.mark.parametrize(
        "bad", ["read file", "read;file", "Read_File", "read-file", "$(id)", "9lives"]
    )
    def test_an_entry_that_is_not_a_bare_tool_name_is_refused(self, bad: str) -> None:
        # The value is spliced into ONE environment string the engine
        # re-splits, so an entry carrying a separator would advertise a
        # different set than the operator declared.
        with pytest.raises(ValueError, match="bare tool name"):
            validated_tool_set(bad)


class TestForwarding:
    def test_an_undeclared_arm_forwards_nothing_at_all(self) -> None:
        # The load-bearing test: every measurement taken before #3032 was taken
        # with no such variable in the container, and an arm that declares no
        # tool set must still be able to say that.
        assert tool_set_env(_reader(None)) == {}
        assert ENV not in _agent_with(None)._forwarded_env()

    def test_a_declared_set_reaches_the_container_as_a_closed_set(self) -> None:
        # `only:` and not the bare list: the bare form lets the engine's
        # discovery layer stack its own additions on top (three discovery
        # tools, before the 2026-08 tool purge), so a declared arm could
        # advertise more than it names and stop being the arm anyone
        # reasoned about.
        assert _agent_with(_BASELINE)._forwarded_env()[ENV] == f"only:{_BASELINE}"

    def test_an_operator_written_prefix_yields_the_same_set(self) -> None:
        assert _agent_with(f"only:{_BASELINE}")._forwarded_env()[ENV] == (
            f"only:{_BASELINE}"
        )

    def test_the_forwarded_value_is_the_normalised_one(self) -> None:
        # Not the raw string: the container must receive the same set the
        # manifest discloses, or the record describes a run that did not happen.
        agent = _agent_with(" task_list , save_state ,task_list")
        assert agent._forwarded_env()[ENV] == "only:task_list,save_state"
        assert list(declared(agent._configured_value)) == ["task_list", "save_state"]

    def test_a_malformed_set_refuses_the_run_rather_than_dropping_the_arm(
        self,
    ) -> None:
        # Failing closed is the point. A dropped restriction would run the full
        # catalog and score as the restricted arm — a measurement that is
        # wrong rather than missing, which is the worse of the two.
        with pytest.raises(ValueError):
            _agent_with("task_list,save state")._forwarded_env()


class TestRegistration:
    def test_the_tool_set_is_registered_as_a_container_knob(self) -> None:
        # The ambient check fails closed: exported but unregistered refuses the
        # run rather than enabling the policy.
        assert ENV in _CLAIM_CONTAINER_ENV

    def test_the_launcher_declares_where_the_tool_set_comes_from(self) -> None:
        assert _LAUNCHER_CONTROLS["tool_set_authority"] == "validated-container-env"


class TestClaimPathRefusal:
    """The audited claim path publishes a number; it may not vary this."""

    def test_the_secure_launcher_refuses_an_ambient_tool_set(self) -> None:
        from stella_harbor.secure_launcher import _validate_claim_environment

        with pytest.raises(RuntimeError, match=r"refuses an ambient STELLA_LEAN_TOOLS"):
            _validate_claim_environment(
                {
                    "STELLA_DISABLE_REFLECTION": "1",
                    ENV: _BASELINE,
                }
            )

    def test_the_refusal_names_where_the_arm_belongs_instead(self) -> None:
        # A refusal that does not say where to run the arm just moves the
        # problem; the pointer is the half that makes it actionable.
        from stella_harbor.secure_launcher import _validate_claim_environment

        with pytest.raises(RuntimeError, match=r"ArenaBench"):
            _validate_claim_environment(
                {"STELLA_DISABLE_REFLECTION": "1", ENV: "task_list"}
            )

    def test_an_unrestricted_claim_environment_is_unaffected(self) -> None:
        # The guard must be strictly additive: a claim run that declares no
        # tool set has to fail exactly where it failed before, on the commit.
        from stella_harbor.secure_launcher import _validate_claim_environment

        with pytest.raises(RuntimeError, match=r"STELLA_SOURCE_COMMIT"):
            _validate_claim_environment({"STELLA_DISABLE_REFLECTION": "1"})
