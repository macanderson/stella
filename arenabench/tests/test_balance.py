# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The OpenRouter balance preflight's decisions, pinned.

The submit-side integration — an underfunded ``cloud run`` refusing before
any upload, a funded one submitting, a broken preflight failing open — lives
in ``tests/test_cloud.py`` beside the other submission witnesses. This file
pins the pure halves: the credits arithmetic, the refusal boundary, the
message contents an operator acts on, and the fail-open ladder. No test here
reads the environment or the network: the HTTP adapter rides behind the
``fetch`` seam.
"""

from __future__ import annotations

import pytest

from arenabench.balance import (
    BalanceUnknownError,
    preflight,
    projected_spend,
    remaining_credits,
    shortfall_message,
)

#: The live endpoint's shape, verbatim from a real response (2026-08-14).
LIVE_SHAPE = {"data": {"total_credits": 1650, "total_usage": 1606.027034237}}


class TestRemainingCredits:
    def test_the_live_endpoint_shape_parses_to_the_difference(self) -> None:
        assert remaining_credits(LIVE_SHAPE) == pytest.approx(43.972965763)

    def test_string_totals_still_parse(self) -> None:
        # JSON numbers sometimes arrive as strings through intermediaries;
        # float() accepts them, and rejecting would fail-open a check that
        # actually had the answer in hand.
        payload = {"data": {"total_credits": "10.5", "total_usage": "0.5"}}
        assert remaining_credits(payload) == pytest.approx(10.0)

    @pytest.mark.parametrize(
        "payload",
        [
            {},
            [],
            "not an object",
            None,
            {"data": None},
            {"data": []},
            {"data": {}},
            {"data": {"total_credits": 10}},
            {"data": {"total_credits": "much", "total_usage": 0}},
            {"data": {"total_credits": None, "total_usage": 0}},
        ],
    )
    def test_any_shape_surprise_is_unknown_never_zero(self, payload) -> None:
        """A parsing surprise must raise — a guessed 0.0 would refuse a
        funded run on a bug, the inversion of the fail-open contract."""
        with pytest.raises(BalanceUnknownError):
            remaining_credits(payload)


class TestShortfall:
    def test_a_covering_balance_produces_no_refusal(self) -> None:
        assert shortfall_message(89.0, 89, 1.0) is None

    def test_one_cent_short_refuses(self) -> None:
        assert shortfall_message(88.99, 89, 1.0) is not None

    def test_the_refusal_names_balance_projection_and_the_override(self) -> None:
        """The three facts an operator needs, in one message: what was
        found, what was projected (with the knob that shapes it), and the
        flag that overrides the refusal."""
        message = shortfall_message(12.34, 89, 1.0)
        assert message is not None
        assert "$12.34" in message
        assert "$89.00" in message
        assert "89 trial(s)" in message
        assert "--est-per-trial" in message
        assert "--skip-balance-check" in message

    def test_the_estimate_scales_the_projection(self) -> None:
        assert projected_spend(30, 0.5) == pytest.approx(15.0)
        assert shortfall_message(20.0, 30, 0.5) is None
        assert shortfall_message(14.0, 30, 0.5) is not None


class TestPreflight:
    def test_no_key_skips_without_touching_the_seam(self) -> None:
        lines: list[str] = []

        def must_not_fetch(key: str) -> dict:
            raise AssertionError("the credits endpoint must not be queried without a key")

        refusal = preflight(
            trials=89,
            est_per_trial=1.0,
            api_key=None,
            fetch=must_not_fetch,
            out=lines.append,
        )
        assert refusal is None
        assert any("skipped" in line and "OPENROUTER_API_KEY" in line for line in lines)

    def test_a_fetch_failure_fails_open_with_a_printed_reason(self) -> None:
        lines: list[str] = []

        def broken(key: str) -> dict:
            raise BalanceUnknownError("GET https://openrouter.ai/api/v1/credits failed")

        refusal = preflight(
            trials=89, est_per_trial=1.0, api_key="sk-or-x", fetch=broken, out=lines.append
        )
        assert refusal is None
        assert any("skipped" in line and "failed" in line for line in lines)

    def test_even_an_unexpected_bug_fails_open(self) -> None:
        """The catch is deliberately broad: the preflight is advisory, and
        its own defect must never block a funded run — visibly, though."""
        lines: list[str] = []

        def buggy(key: str) -> dict:
            raise TypeError("a bug in the preflight itself")

        refusal = preflight(
            trials=89, est_per_trial=1.0, api_key="sk-or-x", fetch=buggy, out=lines.append
        )
        assert refusal is None
        assert any("skipped" in line for line in lines)

    def test_a_funded_balance_proceeds_and_says_so(self) -> None:
        lines: list[str] = []
        refusal = preflight(
            trials=1,
            est_per_trial=1.0,
            api_key="sk-or-x",
            fetch=lambda key: LIVE_SHAPE,
            out=lines.append,
        )
        assert refusal is None
        assert any("balance" in line and "covers" in line for line in lines)

    def test_an_underfunded_balance_returns_the_refusal(self) -> None:
        refusal = preflight(
            trials=89,
            est_per_trial=1.0,
            api_key="sk-or-x",
            fetch=lambda key: LIVE_SHAPE,  # ~$43.97 remaining
            out=lambda line: None,
        )
        assert refusal is not None
        assert "--skip-balance-check" in refusal
