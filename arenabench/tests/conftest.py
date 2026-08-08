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
import shutil
import time
import zipfile
from pathlib import Path

import pytest

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
