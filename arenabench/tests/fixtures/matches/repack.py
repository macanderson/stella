# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Repack an extracted fixture match tree into its tracked archive.

Fixture matches are tracked as one ``<match-id>.zip`` per match rather than
as dozens of individual result files; ``tests/conftest.py`` unpacks them
before a test session. To change a fixture: run pytest once (which extracts
the tree), edit the extracted files, then

    python tests/fixtures/matches/repack.py tests/fixtures/matches/<match-id>

The archive is deterministic — sorted member order, a fixed timestamp, and
fixed permissions — so regenerating it without changing any file's bytes
produces an identical artifact and an empty diff.
"""

from __future__ import annotations

import sys
import zipfile
from pathlib import Path

# Any fixed value works; zipfile just refuses pre-1980 dates.
_EPOCH = (2026, 1, 1, 0, 0, 0)


def repack(match_dir: Path) -> Path:
    """Archive ``match_dir`` as a sibling ``<match_dir>.zip`` and return it."""
    archive = match_dir.with_suffix(".zip")
    members = sorted(
        path
        for path in match_dir.rglob("*")
        if path.is_file() and path.name != ".stamp"
    )
    if not members:
        raise SystemExit(f"repack: nothing to pack under {match_dir}")
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as zf:
        for path in members:
            info = zipfile.ZipInfo(
                str(path.relative_to(match_dir.parent)), date_time=_EPOCH
            )
            info.external_attr = 0o644 << 16
            zf.writestr(info, path.read_bytes())
    return archive


def main(argv: list[str]) -> int:
    if len(argv) != 1:
        print("usage: repack.py <extracted-match-dir>", file=sys.stderr)
        return 2
    match_dir = Path(argv[0])
    if not match_dir.is_dir():
        print(f"repack: no such directory: {match_dir}", file=sys.stderr)
        return 2
    archive = repack(match_dir)
    print(f"{archive}: {archive.stat().st_size} bytes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
