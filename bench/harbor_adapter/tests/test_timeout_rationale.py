"""Every benchmarked slug's model timeout is a decision, never a silence
(#2070, promised in #2021's refutation).

`default_model_timeout()` returning ``None`` means "inherit the engine's
816s idle-silence default" — correct behaviour, but until now
indistinguishable from forgetting: the next slug added to
`_BENCHMARKED_SLUGS` inherited with zero friction, and the inherited number
was findable only in the Rust tree (which is how #2021 was filed on a false
premise). These tests make the inherit explicit and pin the 816 against the
driver's own literal.

Its own file rather than more length on ``test_posture.py``: two open PRs
append to that file's tail, and a third would guarantee a textual conflict.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

pytest.importorskip("harbor", reason="Harbor is required to import the adapter")

from stella_harbor import _benchmark_engine_posture  # noqa: E402

# Internals of the posture's ceiling policy, imported from the module like
# test_posture.py imports its siblings.
from stella_harbor.posture import (  # noqa: E402 - after importorskip by design
    _BENCHMARKED_SLUGS,
    _ENGINE_DEFAULT_MODEL_TIMEOUT_SECS,
    _INHERITED_TIMEOUT_RATIONALE,
    _MODEL_TIMEOUT_BY_SLUG,
    default_model_timeout,
)

_REPO_ROOT = Path(__file__).resolve().parents[3]
_DRIVER_RS = _REPO_ROOT / "crates" / "stella-core" / "src" / "driver.rs"


class TestInheritedTimeoutRationale:
    def test_every_benchmarked_slug_decides_its_timeout(self) -> None:
        """Explicit row XOR documented inherit — never neither, never both.

        Neither = the omission #2070 exists to prevent: the next slug added
        to `_BENCHMARKED_SLUGS` fails here until someone decides. Both = a
        stale rationale left behind after a real row was added, which would
        document an inherit that no longer happens.
        """
        for slug in sorted(_BENCHMARKED_SLUGS):
            has_row = slug in _MODEL_TIMEOUT_BY_SLUG
            has_rationale = bool(_INHERITED_TIMEOUT_RATIONALE.get(slug, "").strip())
            assert has_row != has_rationale, (
                f"{slug}: a benchmarked slug needs exactly one of a "
                "`_MODEL_TIMEOUT_BY_SLUG` row or an "
                "`_INHERITED_TIMEOUT_RATIONALE` sentence — "
                f"row={has_row}, rationale={has_rationale}. An undecided "
                "timeout looks deliberate and is not."
            )

    def test_rationales_name_the_number_they_inherit(self) -> None:
        """A rationale that omits the 816 re-creates the original problem:
        a reader who cannot learn what the slug actually runs under."""
        for slug, rationale in _INHERITED_TIMEOUT_RATIONALE.items():
            assert str(_ENGINE_DEFAULT_MODEL_TIMEOUT_SECS) in rationale, (
                f"{slug}: the rationale must name the inherited "
                f"{_ENGINE_DEFAULT_MODEL_TIMEOUT_SECS}s default"
            )
            assert default_model_timeout(slug) is None, (
                f"{slug}: carries an inherit rationale but resolves an "
                "explicit timeout — the rationale is stale"
            )

    def test_the_816_matches_the_engine_default_in_driver_rs(self) -> None:
        """The documented number pins against the driver's literal — the
        `TestOutputCeilingParity` discipline, so the two cannot drift while
        both look deliberate. The path is a literal in this file; a crate
        move is fixed here."""
        try:
            source = _DRIVER_RS.read_text(encoding="utf-8")
        except OSError as exc:
            raise AssertionError(
                f"cannot read the engine default at {_DRIVER_RS}: "
                f"{exc.strerror}. That path is a literal in this file — if "
                "the crate moved, update `_DRIVER_RS` to match."
            ) from exc
        match = re.search(
            r"model_timeout:\s*Some\(Duration::from_secs\((\d+)\)\)", source
        )
        assert match is not None, (
            "could not find the engine's `model_timeout` default in "
            f"{_DRIVER_RS} — the seeding moved or was renamed; repoint the "
            "regex in this test"
        )
        assert int(match.group(1)) == _ENGINE_DEFAULT_MODEL_TIMEOUT_SECS, (
            "the engine default changed: update "
            "`_ENGINE_DEFAULT_MODEL_TIMEOUT_SECS` and every rationale that "
            "names it, in the same PR"
        )

    def test_the_emitted_posture_is_untouched(self) -> None:
        """Zero digest cost is the constraint the whole change lives under
        (#2021's refutation): the sentences are module constants, and the
        Sonnet posture still emits no `model_timeout_secs` key."""
        posture, _normalized, _digest = _benchmark_engine_posture(
            "openrouter/anthropic/claude-sonnet-5"
        )
        assert "model_timeout_secs" not in posture
