# SPDX-License-Identifier: AGPL-3.0-only
"""A broken SUT pin fails closed at binary resolution (#2098).

``_locate_binary``'s ``PATH`` and cwd-walk tiers exist for genuinely unpinned
development runs and stay (#2051 covers their visibility separately). What
must never happen is a *pinned* trial reaching them: on 2026-08-07 a match
pinned to ``main`` lost its staged binary to the ref moving mid-build, the
launcher fell through without setting ``STELLA_BINARY``, and the ``PATH``
tier uploaded the operator's native macOS arm64 build into every Linux task
container — seven trials dead on ``Exec format error``, silently at every
layer.

The launcher (arenabench's runner) sets ``ARENABENCH_SUT_COMMIT`` beside
``STELLA_BINARY`` for every pinned trial, so the pin marker present without
the explicit binary is *always* a broken pin. These tests plant working
binaries on both ambient tiers and assert they are never reached while the
marker is set — the fallback would succeed, which is exactly why it must not
be attempted.
"""

from pathlib import Path

import pytest

from stella_harbor import _locate_binary


@pytest.fixture
def ambient_tiers(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """A ``stella`` on PATH and one at ``./target/release``, both resolvable.

    Both tiers would succeed, so any test that still fails proves the tier
    was refused rather than merely missing.
    """
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    on_path = bin_dir / "stella"
    on_path.write_text("#!/bin/sh\n")
    on_path.chmod(0o755)

    project = tmp_path / "project"
    (project / "target" / "release").mkdir(parents=True)
    walked = project / "target" / "release" / "stella"
    walked.write_text("#!/bin/sh\n")
    walked.chmod(0o755)

    monkeypatch.setenv("PATH", str(bin_dir))
    monkeypatch.chdir(project)
    monkeypatch.delenv("STELLA_BINARY", raising=False)
    monkeypatch.delenv("ARENABENCH_SUT_COMMIT", raising=False)
    return on_path


class TestBrokenPinFailsClosed:
    def test_a_pin_without_stella_binary_never_reaches_path_or_cwd_tiers(
        self, ambient_tiers: Path, monkeypatch: pytest.MonkeyPatch
    ):
        """The #2098 regression witness: pin marker set, explicit binary lost,
        two working fallbacks available — refuse anyway."""
        monkeypatch.setenv("ARENABENCH_SUT_COMMIT", "a" * 40)
        with pytest.raises(FileNotFoundError) as refusal:
            _locate_binary()
        message = str(refusal.value)
        assert "pinned" in message
        assert "fails closed" in message
        assert ("a" * 12) in message, "the refusal must name the pinned commit"

    def test_an_explicit_binary_satisfies_a_pinned_trial(
        self, ambient_tiers: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ):
        staged = tmp_path / "staged-stella"
        staged.write_text("#!/bin/sh\n")
        staged.chmod(0o755)
        monkeypatch.setenv("ARENABENCH_SUT_COMMIT", "a" * 40)
        monkeypatch.setenv("STELLA_BINARY", str(staged))
        assert _locate_binary() == staged

    def test_a_pinned_trial_whose_binary_path_is_gone_still_refuses(
        self, ambient_tiers: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ):
        """A dangling STELLA_BINARY on a pinned trial is the same broken pin —
        the existing explicit-tier error already refuses it; pin or no pin,
        it must not degrade to the ambient tiers."""
        monkeypatch.setenv("ARENABENCH_SUT_COMMIT", "a" * 40)
        monkeypatch.setenv("STELLA_BINARY", str(tmp_path / "deleted-stella"))
        with pytest.raises(FileNotFoundError):
            _locate_binary()


class TestUnpinnedFallbackSurvives:
    """#2098's constraint: only a broken *pin* fails closed. The ambient
    tiers stay for unpinned development runs — removing them is explicitly
    out of scope (#2051 tracks their visibility)."""

    def test_an_unpinned_run_still_resolves_from_path(self, ambient_tiers: Path):
        assert _locate_binary() == ambient_tiers
