#!/usr/bin/env python3
"""Pack a release directory into a byte-reproducible .tar.gz (#910).

`tar -czf` records every file's mtime, uid/gid, owner names and directory
traversal order, and gzip stamps the current time into its header — so two
runners that produce a byte-identical *binary* still produce different
*tarballs*, and `SHA256SUMS` (which is what install.sh and the Homebrew formula
actually check) diverges anyway.

This is a Python script rather than tar flags because there are no tar flags
that work: the macOS runners have bsdtar and the Linux runners have GNU tar,
and their determinism options do not overlap (`--sort`/`--mtime`/`--owner` vs
`--uid`/`--gid`). stdlib `tarfile` + `gzip` behave identically on both.

Determinism rules, all of them load-bearing:

  * entries are emitted in sorted path order, not readdir order;
  * uid/gid are 0 and uname/gname are empty, so the builder's account never
    reaches the archive;
  * every mtime is SOURCE_DATE_EPOCH;
  * modes are normalised to 0755 (directories and executables) or 0644, so an
    inherited umask cannot change the bytes;
  * gzip is level 9 with mtime 0 (CPython also hardcodes the OS byte to 255,
    so the header carries no platform of its own).

Usage:
    scripts/package-tarball.py <dir-to-pack> <output.tar.gz>

The directory's basename becomes the archive's single top-level entry, matching
the layout `tar -C dist -czf <stem>.tar.gz <stem>` produced before.
SOURCE_DATE_EPOCH must be set (scripts/repro-build.sh exports it).
"""

from __future__ import annotations

import gzip
import os
import sys
import tarfile
from pathlib import Path


def normalise(info: tarfile.TarInfo, mtime: int) -> tarfile.TarInfo:
    """Strip every builder-specific field from one archive entry."""
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    info.mtime = mtime
    if info.isdir() or (info.mode & 0o100):
        info.mode = 0o755
    else:
        info.mode = 0o644
    return info


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print(
            "package-tarball: usage: package-tarball.py <dir-to-pack> <output.tar.gz>",
            file=sys.stderr,
        )
        return 2

    source = Path(argv[1])
    output = Path(argv[2])

    if not source.is_dir():
        print(f"package-tarball: not a directory: {source}", file=sys.stderr)
        return 1

    epoch = os.environ.get("SOURCE_DATE_EPOCH")
    if not epoch:
        print(
            "package-tarball: SOURCE_DATE_EPOCH is unset — refusing to bake the "
            "current time into the archive (scripts/repro-build.sh exports it).",
            file=sys.stderr,
        )
        return 1
    mtime = int(epoch)

    stem = source.name
    # Sorted, so the entry order is a property of the names and not of the
    # filesystem. Relative paths keep the archive rooted at <stem>/. Directories
    # get their own entries — every extractor creates missing parents anyway,
    # but an explicit entry is what carries the normalised mode.
    members = sorted(
        p.relative_to(source.parent)
        for p in source.rglob("*")
        if p.is_file() or p.is_dir()
    )

    output.parent.mkdir(parents=True, exist_ok=True)
    with open(output, "wb") as raw:
        # mtime=0 rather than SOURCE_DATE_EPOCH: the gzip header timestamp is
        # metadata about the compression, describes nothing a verifier needs,
        # and zeroing it is what `gzip -n` does.
        with gzip.GzipFile(filename="", mode="wb", compresslevel=9, fileobj=raw, mtime=0) as gz:
            with tarfile.open(fileobj=gz, mode="w", format=tarfile.GNU_FORMAT) as tar:
                root = tarfile.TarInfo(stem)
                root.type = tarfile.DIRTYPE
                tar.addfile(normalise(root, mtime))
                for rel in members:
                    path = source.parent / rel
                    info = normalise(tar.gettarinfo(str(path), arcname=str(rel)), mtime)
                    if info.isdir():
                        tar.addfile(info)
                    else:
                        with open(path, "rb") as fh:
                            tar.addfile(info, fh)

    print(f"package-tarball: wrote {output}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
