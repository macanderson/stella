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

from arenabench.balance import COMFORTABLE_HEADROOM, verdict


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
