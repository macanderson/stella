# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""#3216: a seat on an exhausted Claude subscription cannot launch.

The only claude-fable-5 head-to-head lost 4 of its 6 Claude Code trials to
the subscription's five-hour limit — three rejected before turn 1, one
throttled through 37 turns — and none of the four measured the agent. The
preflight existed only as a hand-rolled script one layer too late; these
tests pin the wired version: fail closed at launch, refusal names the reset
time, and no container ever starts.
"""

from __future__ import annotations

import pytest

from arenabench.claude_oauth import (
    CLAUDE_OAUTH_ENV,
    quota_refusal,
    seat_quota_refusals,
)
from arenabench.model import MatchSpec


def _spec(*seat_envs: dict[str, str]) -> MatchSpec:
    return MatchSpec.from_json(
        {
            "name": "m",
            "dataset": "terminal-bench-2.1",
            "tasks": ["fix-git"],
            "contestants": [
                {
                    "name": f"cc-{index}",
                    "agent": "claude-code",
                    "engine": {"api": "anthropic", "model": "claude-fable-5"},
                    "env": {"required": [CLAUDE_OAUTH_ENV], **env},
                }
                for index, env in enumerate(seat_envs)
            ],
        }
    )


class TestQuotaRefusal:
    def test_a_429_refuses_and_names_the_reset_time(self) -> None:
        reason = quota_refusal(
            429, {"anthropic-ratelimit-unified-reset": "1786700000"}
        )
        assert reason is not None
        assert "usage limit is exhausted" in reason
        # The epoch renders in the host's timezone; assert the date to the
        # month so the test is not a timezone lottery.
        assert "resets 2026-08-1" in reason

    def test_a_429_without_the_header_still_refuses(self) -> None:
        reason = quota_refusal(429, {})
        assert reason is not None and "exhausted" in reason

    def test_a_401_refuses_as_expired_or_revoked(self) -> None:
        reason = quota_refusal(401, {})
        assert reason is not None and "401" in reason

    def test_a_serving_route_does_not_refuse(self) -> None:
        # 400 for the probe body still proves the token is served.
        assert quota_refusal(200, {}) is None
        assert quota_refusal(400, {}) is None
        assert quota_refusal(500, {}) is None


class TestSeatQuotaRefusals:
    def test_an_exhausted_token_refuses_every_seat_carrying_it(self) -> None:
        probes: list[str] = []

        def probe(token: str) -> tuple[int, dict[str, str]]:
            probes.append(token)
            return 429, {}

        refusals = seat_quota_refusals(
            _spec({CLAUDE_OAUTH_ENV: "oat-1"}, {CLAUDE_OAUTH_ENV: "oat-1"}),
            probe=probe,
        )
        assert set(refusals) == {"cc-0", "cc-1"}
        # Once per distinct token: two seats on one plan share one limit.
        assert probes == ["oat-1"]

    def test_a_seat_without_the_token_is_never_probed(self) -> None:
        def probe(token: str) -> tuple[int, dict[str, str]]:
            raise AssertionError("no subscription seat, no probe")

        assert seat_quota_refusals(_spec({}), probe=probe) == {}

    def test_a_serving_token_launches(self) -> None:
        refusals = seat_quota_refusals(
            _spec({CLAUDE_OAUTH_ENV: "oat-ok"}), probe=lambda t: (200, {})
        )
        assert refusals == {}

    def test_a_transport_failure_does_not_refuse(self) -> None:
        def probe(token: str) -> tuple[int, dict[str, str]]:
            raise OSError("no route to host")

        assert seat_quota_refusals(
            _spec({CLAUDE_OAUTH_ENV: "oat-x"}), probe=probe
        ) == {}


class TestLaunchIsRefusedBeforeAnyContainer:
    def test_create_match_refuses_a_429_seat(self, tmp_path, monkeypatch) -> None:
        import arenabench.claude_oauth as oauth_mod
        from arenabench.server import ArenaServer

        monkeypatch.setenv("ARENABENCH_CREDENTIALS", str(tmp_path / "none.env"))
        monkeypatch.setattr(
            oauth_mod,
            "_post_preflight",
            lambda token: (429, {"anthropic-ratelimit-unified-reset": "1786700000"}),
        )
        arena = ArenaServer(tmp_path / "ws")
        payload = {
            "name": "m",
            "dataset": "terminal-bench-2.1",
            "contestants": [
                {
                    "name": "cc",
                    "agent": "claude-code",
                    "engine": {"api": "anthropic", "model": "claude-fable-5"},
                    "env": {
                        "required": [CLAUDE_OAUTH_ENV],
                        CLAUDE_OAUTH_ENV: "oat-throttled",
                    },
                }
            ],
        }
        with pytest.raises(ValueError) as caught:
            arena.create_match(payload)
        assert "exhausted" in str(caught.value)
        assert arena.runner.matches == {}, "a refused match must not exist"
