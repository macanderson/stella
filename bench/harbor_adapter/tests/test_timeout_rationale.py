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
_DRIVER_SRC = _REPO_ROOT / "crates" / "stella-core" / "src"
_MODEL_TIMEOUT_DEFAULT = re.compile(
    r"model_timeout:\s*Some\(Duration::from_secs\((\d+)\)\)"
)


def _engine_default_seconds() -> int:
    """The engine's `model_timeout` default, found wherever it currently lives.

    Deliberately a search over the whole driver module rather than a fixed
    path. `EngineConfig` was seeded in `driver.rs` when this guard was written
    and now lives in `driver/config.rs` (#3410); the literal did not change,
    only its file, and a hardcoded path turns that ordinary split into a red
    bench job. The same shape broke `test_assurance_tiers.py` when
    `settings.rs` was split (#3390).
    """
    candidates = [
        _DRIVER_SRC / "driver.rs",
        *sorted((_DRIVER_SRC / "driver").glob("*.rs")),
    ]
    found = [
        (path, match)
        for path in candidates
        if path.exists()
        for match in [_MODEL_TIMEOUT_DEFAULT.search(path.read_text(encoding="utf-8"))]
        if match is not None
    ]
    assert found, (
        "could not find the engine's `model_timeout` default anywhere under "
        f"{_DRIVER_SRC / 'driver'} or {_DRIVER_SRC / 'driver.rs'} — the "
        "seeding was renamed or the default removed, so this rationale no "
        "longer pins anything"
    )
    assert len(found) == 1, (
        "the `model_timeout` default is seeded in more than one file "
        f"({[str(path) for path, _ in found]}); this guard can no longer say "
        "which one the engine uses"
    )
    return int(found[0][1].group(1))


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

    def test_the_816_matches_the_engine_default_in_the_driver(self) -> None:
        """The documented number pins against the driver's literal — the
        `TestOutputCeilingParity` discipline, so the two cannot drift while
        both look deliberate.

        Which *file* seeds the default is not part of the claim: see
        `_engine_default_seconds`, which searches the driver module so that
        splitting it moves no goalposts. What is pinned is the number.
        """
        assert _engine_default_seconds() == _ENGINE_DEFAULT_MODEL_TIMEOUT_SECS, (
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
