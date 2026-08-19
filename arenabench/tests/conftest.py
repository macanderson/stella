# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Session bootstrap: unpack the fixture matches the witness tests replay.

Fixture match trees (``tests/fixtures/matches/<match-id>/``) are tracked as
one archive per match — ``<match-id>.zip`` — rather than as dozens of
individual result files. Before collection, every archive is extracted next
to itself so the tests' ``fixtures/matches/<match-id>`` paths resolve exactly
as they did when the trees were tracked directly. The extracted trees are
gitignored; the archive is the only tracked artifact, and
``fixtures/matches/repack.py`` regenerates it deterministically after an
edit.

Each extraction is stamped with the archive's digest, so an unchanged
archive costs one hash per session and an updated one re-extracts from
scratch. The first-ever extraction is not concurrency-safe across processes
(the suite runs single-process; revisit the stamp if pytest-xdist arrives).

Also home to the pacing every snapshot test shares — see
:func:`_wait_for_snapshots`.
"""

from __future__ import annotations

import hashlib
import os
import shutil
import time
import zipfile
from pathlib import Path

import pytest

from arenabench import sut
from arenabench.snapshot import load_capture_status, load_manifest

MATCHES = Path(__file__).parent / "fixtures" / "matches"

#: How long a snapshot wait tolerates before calling the host broken. Sized as
#: roughly twenty times the dearest capture ever measured (5.6 s, against a
#: 2.0 s interval, on the run that filed #2114), so it cannot lose a race a
#: working host would eventually win — while still bounding a wedged one.
SNAPSHOT_DEADLINE = 120.0


def _unpack(archive: Path) -> None:
    target = archive.with_suffix("")
    stamp = target / ".stamp"
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    if stamp.is_file() and stamp.read_text(encoding="utf-8").strip() == digest:
        return
    if target.exists():
        shutil.rmtree(target)
    with zipfile.ZipFile(archive) as zf:
        # Member paths are relative and ZipFile.extractall sanitizes
        # traversal; the archive is tracked content, not an upload.
        zf.extractall(MATCHES)
    stamp.write_text(digest + "\n", encoding="utf-8")


def pytest_configure(config) -> None:
    for archive in sorted(MATCHES.glob("*.zip")):
        _unpack(archive)
    _isolate_dataset_corpus(config)


def _isolate_dataset_corpus(config) -> None:
    """Point dataset enumeration at empty directories for the whole session.

    The suite names synthetic tasks (``task-a``, ``alpha``) against the real
    ``terminal-bench-2.1`` key, which is fine — the tests are about slicing,
    submission and scoring, not about the corpus. It stops being fine the
    moment anything *checks* those names: #3255's phantom-task refusal reads
    the local corpus, so on a developer machine with the dataset unpacked it
    correctly rejected specs the tests deliberately invent, while the same
    tests passed on a CI runner with no corpus at all.

    Isolating here rather than weakening the guard keeps both truths: the
    guard stays strict against a real submission, and the suite's outcome
    stops depending on what happens to be in the developer's home directory.
    A test that wants a corpus supplies one to
    :meth:`MatchSpec.unknown_tasks` directly — it takes the names as an
    argument for exactly this reason.
    """
    empty = Path(config.invocation_params.dir) / ".pytest-empty-corpus"
    (empty / "datasets").mkdir(parents=True, exist_ok=True)
    (empty / "harbor").mkdir(parents=True, exist_ok=True)
    os.environ["ARENABENCH_DATASETS"] = str(empty / "datasets")
    os.environ["HARBOR_CACHE_DIR"] = str(empty / "harbor")


# ---------------------------------------------------------------------------
# The Stella checkout the cross-language ratchets read
# ---------------------------------------------------------------------------


@pytest.fixture
def stella_checkout() -> Path:
    """The Stella checkout to read Rust sources from, or skip the test.

    A handful of tests are *ratchets across the language boundary*: they read
    a file in Stella's tree and assert that a constant here still agrees with
    it. They are the reason a Rust rename cannot silently leave ArenaBench
    spelling a role the engine no longer resolves (#1449).

    They used to reach that tree as ``Path(__file__).parents[2]`` — true only
    while this folder sits inside the Stella monorepo. That is a hard-coded
    layout, and it fails two ways: it went stale in place when
    ``crates/stella-pipeline`` was deleted (#3865, filed as #3919), and it
    becomes meaningless the moment this folder is its own repository, where
    ``parents[2]`` is whatever directory happens to be above the checkout.

    So the checkout is resolved through the seam ArenaBench already publishes
    for exactly this question — ``ARENABENCH_STELLA_REPO``, then
    ``ARENABENCH_STELLA_ADAPTER``, then the walk-up from this package
    (:func:`arenabench.sut.stella_repo`) — and a run with no checkout skips
    rather than fails. That asymmetry is the whole design:

    * **No checkout: skip.** ArenaBench is agent-agnostic and its own gate
      installs no Rust tree. A ratchet against a file that is not there is not
      a failure of this repository.
    * **A checkout, but the file is gone or unreadable: FAIL.** That is the
      ratchet doing its job — the authority moved and nobody told the reader.
      Repoint the test in the same change; never soften it to a skip, which is
      how #3919 stayed invisible for as long as it did.
    """
    repo = sut.stella_repo()
    if repo is None:
        pytest.skip(
            "no Stella checkout to read: "
            f"{sut.repo_problem()} These assertions are a cross-language "
            "ratchet against Stella's Rust sources; point "
            "ARENABENCH_STELLA_REPO at a checkout to run them."
        )
    return repo


@pytest.fixture
def stella_adapter(tmp_path_factory, monkeypatch: pytest.MonkeyPatch) -> Path:
    """A synthetic ``stella_harbor`` source tree, wired as this rig's adapter.

    Building a seat's environment goes through
    :func:`arenabench.adapter.seat_import_roots`, which stages the adapter and
    raises :class:`~arenabench.adapter.AdapterUnavailableError` when neither
    ``ARENABENCH_STELLA_ADAPTER`` nor a Stella checkout names one. The tests
    that request this fixture are not about *finding* the adapter — they are
    about what the seat's environment contains once one exists (lineage opt-in,
    tool-set closure, credential screening, the launch command) — so they
    supply one instead of borrowing whatever happens to sit beside the
    checkout.

    That is a strict improvement even inside the monorepo: their outcome stops
    depending on whether the developer's `bench/harbor_adapter` is present and
    intact. It is what makes them run at all once this folder is its own
    repository, where there is no Stella tree to borrow from (#3919). The
    tests that genuinely assert discovery and refusal — `test_adapter.py`'s
    unconfigured cases — deliberately do not take this fixture.

    The package holds one empty ``__init__.py`` and nothing else: staging
    digests the files under it and refuses an empty directory, and no test
    here imports it. The seat subprocess that would is never launched.
    """
    root = tmp_path_factory.mktemp("stella-adapter")
    package = root / "stella_harbor"
    package.mkdir()
    (package / "__init__.py").write_text("", encoding="utf-8")
    monkeypatch.setenv("ARENABENCH_STELLA_ADAPTER", str(root))
    monkeypatch.setenv("ARENABENCH_HOME", str(root / "home"))
    return root


# ---------------------------------------------------------------------------
# Snapshot pacing
# ---------------------------------------------------------------------------


def _wait_for_snapshots(
    trial_dir: Path,
    at_least: int,
    *,
    what: str,
    deadline: float = SNAPSHOT_DEADLINE,
) -> int:
    """Block until ``trial_dir`` holds ``at_least`` snapshots; return the count.

    ``SnapshotSupervisor.interval`` is a floor on the cadence, never a period:
    ``_snapshot`` stamps its clock *before* running the container's ``git
    add``/``commit``/``diff``, so a capture dearer than the interval stretches
    the real cadence to whatever the host allows. One was measured at 5.6 s
    against a 2.0 s interval.

    So a ``time.sleep`` sized from the interval is a bet on capture speed, and
    losing it is what made the docker snapshot tests flaky under load — three
    snapshots where the arithmetic assumed seven, failing as "too few
    snapshots to bisect meaningfully" (#2114). Waiting on the manifest is the
    same test with the bet removed: it costs a fast host nothing extra and
    lets a slow one take as long as it needs, while still failing loudly —
    never skipping — on a host that has genuinely stopped capturing.
    """
    limit = time.monotonic() + deadline
    while True:
        count = len(load_manifest(trial_dir))
        if count >= at_least:
            return count
        if time.monotonic() >= limit:
            # "Not keeping up" and "stopped working" are different faults, and
            # this message used to be unable to tell them apart — it could only
            # suggest re-running at DEBUG, because that was the only level the
            # supervisor said anything at. It now records every capture attempt,
            # so the fault names itself here instead of in a second run (#2196).
            status = load_capture_status(trial_dir)
            why = (
                f"capture reports {status.state} after {status.attempts} attempt(s): "
                f"{status.reason}"
                if status is not None and status.reason
                else "capture is not keeping up on this host"
            )
            raise AssertionError(
                f"only {count} of {at_least} snapshots {what} after "
                f"{deadline:.0f}s — {why}"
            )
        time.sleep(0.2)


@pytest.fixture
def wait_for_snapshots():
    """The condition-driven pacing every snapshot test uses instead of sleeping."""
    return _wait_for_snapshots
