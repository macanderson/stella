# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The gateway balance preflight.

Three runs were killed or maimed by credit exhaustion — one partial, one
total loss, one losing 35 of 89 trials — and every time the money ran out
*after* submission, when the trials it kills are already dispatched and
score as operational aborts. These tests pin the arithmetic that refuses
such a run before it starts, and the two directions in which the preflight
must NOT refuse: an unreadable balance, and a run whose cost nobody
estimated.
"""

from __future__ import annotations

import pytest

from arenabench.balance import COMFORTABLE_HEADROOM, Wallet, openrouter_key, verdict
from arenabench.cloud import seat_gateway_credential


class TestBalanceVerdict:
    def test_a_run_costing_more_than_the_balance_is_refused(self):
        """The recorded failure: 89 trials against a wallet that funds ~30."""
        answer = verdict(remaining_usd=12.0, trials=89, cost_per_trial_usd=0.40)
        assert answer.blocks
        assert "35.60" in answer.message, answer.message

    def test_an_empty_wallet_is_refused_without_any_estimate(self):
        """No estimate is needed for zero: every trial 402s on its first call."""
        answer = verdict(remaining_usd=0.0, trials=1, cost_per_trial_usd=None)
        assert answer.blocks

    def test_a_thin_margin_warns_rather_than_refusing(self):
        """A panel's expensive tail spends multiples of the mean, so bare
        sufficiency is worth saying out loud — but it is the operator's call."""
        answer = verdict(remaining_usd=40.0, trials=89, cost_per_trial_usd=0.40)
        assert answer.level == "warn"
        assert not answer.blocks

    def test_comfortable_headroom_passes_quietly(self):
        answer = verdict(
            remaining_usd=89 * 0.40 * COMFORTABLE_HEADROOM + 1,
            trials=89,
            cost_per_trial_usd=0.40,
        )
        assert answer.level == "ok"

    def test_an_unreadable_balance_never_blocks(self):
        """An unreachable gateway must not stop a run that would have
        succeeded — the same "cannot tell" discipline the phantom-task guard
        applies to an unmaterialised dataset (#3255)."""
        answer = verdict(remaining_usd=None, trials=89, cost_per_trial_usd=0.40)
        assert answer.level == "unknown"
        assert not answer.blocks

    @pytest.mark.parametrize("estimate", [None, 0.0])
    def test_a_funded_wallet_with_no_estimate_passes_and_says_so(self, estimate):
        """There is no honest default cost per trial, so an unpriced run is
        reported rather than guessed at."""
        answer = verdict(remaining_usd=50.0, trials=89, cost_per_trial_usd=estimate)
        assert answer.level == "ok"
        assert "--est-cost-per-trial" in answer.message


class TestWalletProvenance:
    """A verdict must say whose money it counted (#3308).

    The preflight prices the submitting host's key and the trials spend a
    key from SSM. When those are different accounts, an unattributed clean
    verdict is byte-indistinguishable from a real all-clear — which is the
    reading that returned three runs to the empty-balance loss this whole
    preflight exists to stop.
    """

    LOCAL = Wallet(env_name="OPENROUTER_API_KEY")
    SPLIT = Wallet(env_name="OPENROUTER_KEY", seat_credential="OPENROUTER_API_KEY")

    def test_a_clean_verdict_names_both_wallets_when_the_seats_spend_another(self):
        answer = verdict(200.0, 89, 0.40, self.SPLIT)
        assert answer.level == "ok"
        assert "priced against OPENROUTER_KEY on this host" in answer.message
        assert "seats spend the SSM-provided OPENROUTER_API_KEY" in answer.message

    def test_with_no_declared_seat_credential_only_the_env_var_is_named(self):
        answer = verdict(200.0, 89, 0.40, self.LOCAL)
        assert "priced against OPENROUTER_API_KEY on this host" in answer.message
        assert "seats spend" not in answer.message

    @pytest.mark.parametrize(
        ("remaining", "estimate", "level"),
        [(0.0, None, "refuse"), (12.0, 0.40, "refuse"), (40.0, 0.40, "warn"),
         (50.0, None, "ok")],
    )
    def test_every_arm_that_priced_a_wallet_attributes_it(
        self, remaining, estimate, level
    ):
        """Including the refusals: a refusal over the wrong wallet is a false
        refusal, and the operator can only see that if it is named."""
        answer = verdict(remaining, 89, estimate, self.SPLIT)
        assert answer.level == level
        assert "priced against OPENROUTER_KEY on this host" in answer.message

    def test_an_unread_balance_attributes_nothing(self):
        """No wallet was read, so there is no wallet to name."""
        answer = verdict(None, 89, 0.40, self.SPLIT)
        assert answer.level == "unknown"
        assert "priced against" not in answer.message

    def test_the_key_arrives_with_the_name_it_was_read_from(self, monkeypatch):
        """`verdict` can only name the variable if `openrouter_key` says which
        one answered — the fallback name is not the primary one."""
        monkeypatch.delenv("OPENROUTER_API_KEY", raising=False)
        monkeypatch.setenv("OPENROUTER_KEY", "sk-not-a-real-key")
        assert openrouter_key() == ("OPENROUTER_KEY", "sk-not-a-real-key")

    def test_no_key_at_all_reads_as_no_reading(self, monkeypatch):
        for name in ("OPENROUTER_API_KEY", "OPENROUTER_KEY"):
            monkeypatch.delenv(name, raising=False)
        assert openrouter_key() is None


class TestSeatGatewayCredential:
    """Which wallet the seats will spend, derived from what SSM will supply."""

    def test_a_declared_gateway_key_that_ssm_provides_is_named(self):
        needed = {"stella": ["OPENROUTER_API_KEY", "ANTHROPIC_API_KEY"]}
        available = {"OPENROUTER_API_KEY", "ANTHROPIC_API_KEY"}
        assert seat_gateway_credential(needed, available) == "OPENROUTER_API_KEY"

    def test_a_declared_key_ssm_will_not_supply_is_not_named(self):
        """Naming a credential the trial will not hold would be a second false
        reassurance, not a fix for the first."""
        needed = {"stella": ["OPENROUTER_API_KEY"]}
        assert seat_gateway_credential(needed, {"ANTHROPIC_API_KEY"}) is None

    def test_seats_spending_no_gateway_key_name_nothing(self):
        needed = {"cc": ["ANTHROPIC_API_KEY"]}
        assert seat_gateway_credential(needed, {"ANTHROPIC_API_KEY"}) is None
