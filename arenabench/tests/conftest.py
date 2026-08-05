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
"""

from __future__ import annotations

import hashlib
import shutil
import zipfile
from pathlib import Path

MATCHES = Path(__file__).parent / "fixtures" / "matches"


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
