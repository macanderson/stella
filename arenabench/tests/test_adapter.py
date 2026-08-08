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

import itertools
import shutil
import threading
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


def test_concurrent_staging_never_publishes_a_half_copied_tree(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """The server is threaded, so two matches stage one digest at once.

    A staging directory named after the digest alone is shared, and the
    damage is permanent: thread B's ``rmtree`` deletes A's finished copy, A
    then renames B's *in-flight* tree into place, and the cache check never
    revisits a digest that already exists — every later trial in the process
    imports the partial adapter.

    The interleaving is forced rather than raced, so this fails
    deterministically against a shared staging name.
    """
    source = _source_tree(tmp_path / "adapter")
    real_copytree = shutil.copytree
    a_copied = threading.Event()
    b_mid_copy = threading.Event()
    a_done = threading.Event()
    calls = itertools.count()

    def sequenced(src, dst, *args, **kwargs):
        # ``copytree`` recurses through this same module attribute for every
        # subdirectory; only the top-level package copy is a staging attempt.
        if Path(dst).name != PACKAGE_NAME:
            return real_copytree(src, dst, *args, **kwargs)
        if next(calls) == 0:
            # A: copy in full, then hold before the rename while B runs.
            result = real_copytree(src, dst, *args, **kwargs)
            a_copied.set()
            assert b_mid_copy.wait(timeout=10), "B never reached its copy"
            return result
        # B: land one file, let A rename whatever is at the staging path,
        # then finish. Mid-copy is the state A must not be able to publish.
        src, dst = Path(src), Path(dst)
        dst.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src / "__init__.py", dst / "__init__.py")
        b_mid_copy.set()
        assert a_done.wait(timeout=10), "A never finished"
        for path in sorted(p for p in src.rglob("*") if p.is_file()):
            target = dst / path.relative_to(src)
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, target)
        return str(dst)

    monkeypatch.setattr(adapter.shutil, "copytree", sequenced)
    failures: list[BaseException] = []

    def stage(record_done: bool):
        try:
            stage_adapter(source)
        except BaseException as error:  # a thread's failure is asserted below
            failures.append(error)
        finally:
            if record_done:
                a_done.set()

    first = threading.Thread(target=stage, args=(True,))
    first.start()
    assert a_copied.wait(timeout=10), f"A never copied: {failures}"
    adapter._staged.clear()  # a second match, same process, same digest
    second = threading.Thread(target=stage, args=(False,))
    second.start()
    first.join(timeout=15)
    second.join(timeout=15)
    assert not first.is_alive() and not second.is_alive(), "staging wedged"
    assert not failures, f"staging raised: {failures}"

    published = adapter_root() / adapter._digest(source) / PACKAGE_NAME
    assert (published / "__init__.py").is_file()
    assert (published / "prices.json").is_file(), "published a half-copied tree"
    assert (published / "sub" / "posture.py").read_text() == "X = 1\n"


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
