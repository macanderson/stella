# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""A seat that never loaded its adapter did not lose a contest.

Two defects with one consequence, both observed on real paid matches:

* The arena resolved the ``stella_harbor`` sources out of its launch checkout
  at **every** trial's install. Deleting that worktree mid-match killed the
  four trials of ``cc00894779ff`` that installed afterwards — 0 steps, 0
  tokens each — and the dashboard scored all four as agent losses, publishing
  ``solve_rate`` 40% against a true agent record of 4/6 (#2127).
* When the adapter was simply absent, ``harbor_agent`` fell back to
  ``_Base = object``, discarding the actionable ``ImportError`` it had just
  built. ``ArenaStellaAgent(...)`` then raised ``TypeError: takes no
  arguments`` and Harbor **exited 0**, so the seat reported *done* having run
  nothing (#2192).

Both now fail by name, and both names are classified as infrastructure, so
neither can re-enter ``solve_rate``'s denominator.
"""

from __future__ import annotations

import shutil
from pathlib import Path

import pytest

from arenabench import adapter
from arenabench.adapter import (
    PACKAGE_NAME,
    AdapterUnavailableError,
    adapter_root,
    stage_adapter,
)
from arenabench.harbor_agent import StellaAdapterMissingError, _unavailable_base
from arenabench.telemetry import INFRASTRUCTURE_FAILURES, VOID_SETUP


@pytest.fixture(autouse=True)
def _isolated_home(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    """A private arena home, and an empty process cache, per test."""
    monkeypatch.setenv("ARENABENCH_HOME", str(tmp_path / "home"))
    monkeypatch.setattr(adapter, "_staged", {})
    yield


def _source_tree(root: Path) -> Path:
    """A minimal adapter checkout: the package, some code, and a data file."""
    package = root / PACKAGE_NAME
    (package / "sub").mkdir(parents=True)
    (package / "__init__.py").write_text("VERSION = '1'\n", encoding="utf-8")
    (package / "sub" / "posture.py").write_text("X = 1\n", encoding="utf-8")
    (package / "prices.json").write_text('{"a": 1}\n', encoding="utf-8")
    return root


def test_a_staged_adapter_outlives_the_checkout_it_was_copied_from(tmp_path: Path):
    """The #2127 witness: trial 1 stages, the worktree dies, trial 2 runs."""
    source = _source_tree(tmp_path / "worktree" / "bench" / "harbor_adapter")

    first = stage_adapter(source)
    assert (first / PACKAGE_NAME / "__init__.py").is_file()
    assert adapter_root() in first.parents

    # The launch worktree is deleted mid-match, exactly as in cc00894779ff.
    shutil.rmtree(tmp_path / "worktree")
    assert not source.exists()

    second = stage_adapter(source)
    assert second == first
    assert (second / PACKAGE_NAME / "sub" / "posture.py").read_text() == "X = 1\n"
    assert (second / PACKAGE_NAME / "prices.json").is_file()


def test_staging_is_content_addressed_so_an_edited_adapter_is_not_shadowed(
    tmp_path: Path,
):
    source = _source_tree(tmp_path / "adapter")
    before = stage_adapter(source)

    (source / PACKAGE_NAME / "sub" / "posture.py").write_text("X = 2\n", encoding="utf-8")
    adapter._staged.clear()  # a later match, same process
    after = stage_adapter(source)

    assert after != before, "an edited adapter must stage a fresh directory"
    assert (after / PACKAGE_NAME / "sub" / "posture.py").read_text() == "X = 2\n"


def test_a_data_file_change_alone_restages(tmp_path: Path):
    """The digest folds in every file, not just ``*.py``."""
    source = _source_tree(tmp_path / "adapter")
    before = stage_adapter(source)

    (source / PACKAGE_NAME / "prices.json").write_text('{"a": 2}\n', encoding="utf-8")
    adapter._staged.clear()

    assert stage_adapter(source) != before


def test_absent_sources_with_nothing_staged_refuse_by_name(tmp_path: Path):
    """The honest failure — and a name telemetry can classify."""
    with pytest.raises(AdapterUnavailableError) as caught:
        stage_adapter(tmp_path / "gone")
    assert "ARENABENCH_STELLA_ADAPTER" in str(caught.value)


def test_an_empty_package_directory_is_refused_rather_than_staged(tmp_path: Path):
    (tmp_path / "adapter" / PACKAGE_NAME).mkdir(parents=True)
    with pytest.raises(AdapterUnavailableError):
        stage_adapter(tmp_path / "adapter")


def test_a_missing_adapter_refuses_at_construction_naming_the_fix():
    """The #2192 witness.

    The old stand-in was ``object``; constructing against it raised
    ``TypeError: takes no arguments`` and Harbor exited 0.
    """
    base = _unavailable_base(
        StellaAdapterMissingError(
            "arenabench's Stella contestant needs the `stella_harbor` adapter "
            "on PYTHONPATH. Point ARENABENCH_STELLA_ADAPTER at "
            "<stella>/bench/harbor_adapter."
        )
    )

    class Seat(base):  # type: ignore[misc,valid-type]
        pass

    with pytest.raises(StellaAdapterMissingError) as caught:
        Seat("task-id", model="x")
    assert "ARENABENCH_STELLA_ADAPTER" in str(caught.value)
    assert not isinstance(caught.value, TypeError)


def test_both_failures_are_infrastructure_not_agent_losses():
    """The half that actually moves the published number."""
    for name in ("AdapterUnavailableError", "StellaAdapterMissingError"):
        assert name in INFRASTRUCTURE_FAILURES, f"{name} would score as a loss"
        assert name in VOID_SETUP, f"{name} needs an outcome-taxonomy bucket"


def test_the_named_error_is_still_an_import_error():
    """Existing ``except ImportError`` paths must keep catching it."""
    assert issubclass(StellaAdapterMissingError, ImportError)
