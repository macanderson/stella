#!/usr/bin/env python3
"""Guard: no code file sits beside a folder with the same name.

`src/anthropic.rs` next to `src/anthropic/` is banned (AGENTS.md, Code
style). The fix is to split the file into modules inside the folder and
re-export them from `anthropic/mod.rs`, so every import keeps working.
That also gives a file near the size limit its room: more hierarchy,
smaller files, same public names.

The baseline (`scripts/module-layout-baseline.txt`) lists the pairs that
existed before this guard. It only shrinks: `--update` removes entries
whose pair is gone and refuses to add one, so a new pair fails the gate
until the file is split.

A baseline entry whose pair is gone does not fail the plain check. Two
branches can each split one pair and merge cleanly; the next `--update`
sweeps the stale entries.

Usage:

    ./scripts/check-module-layout.py [--update] [--bootstrap] [ROOT]

    --update     remove baseline entries whose pair is gone; refuse to add
                 (`make module-layout-update`)
    --bootstrap  create the baseline from the current tree; one-time, and
                 refuses to run when the baseline already exists
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

BASELINE = "scripts/module-layout-baseline.txt"

HEADER = """\
# Down-only list for the no-file-beside-same-named-folder rule (AGENTS.md,
# Code style).
#
# Each line is a code file that sits beside a folder with its own name.
# Splitting the file into modules under the folder (re-exported from its
# mod.rs) removes the line via `make module-layout-update`, which refuses
# to add one. A pair not listed here fails the gate.
#
# This file is meant to reach empty. Do not add a line here to turn the
# gate green -- split the file instead.
"""

REMEDY = (
    "Move the file's code into modules inside the folder and re-export\n"
    "them from the folder's mod.rs, so every existing import keeps\n"
    "working. AGENTS.md's Code style section states the rule."
)


def tracked_rs_and_dirs(root: Path) -> tuple[list[str], set[str]]:
    """Every tracked `.rs` path under `crates/`, and every folder that holds
    a tracked file."""
    out = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard"],
        cwd=root,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.splitlines()
    rs_files: list[str] = []
    dirs: set[str] = set()
    for path in out:
        if not path:
            continue
        parts = path.split("/")
        for i in range(1, len(parts)):
            dirs.add("/".join(parts[:i]))
        if path.startswith("crates/") and path.endswith(".rs"):
            rs_files.append(path)
    return rs_files, dirs


def pairs_in(root: Path) -> list[str]:
    rs_files, dirs = tracked_rs_and_dirs(root)
    return sorted(p for p in rs_files if p[: -len(".rs")] in dirs)


def read_baseline(path: Path) -> list[str]:
    if not path.exists():
        return []
    entries = []
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            entries.append(line)
    return entries


def write_baseline(path: Path, entries: list[str]) -> None:
    body = "".join(f"{entry}\n" for entry in sorted(entries))
    path.write_text(HEADER + body, encoding="utf-8")


def main() -> int:
    argv = [a for a in sys.argv[1:] if not a.startswith("--")]
    flags = {a for a in sys.argv[1:] if a.startswith("--")}
    root = Path(argv[0]) if argv else Path(__file__).resolve().parent.parent
    root = root.resolve()

    pairs = pairs_in(root)
    baseline_path = root / BASELINE
    baseline = read_baseline(baseline_path)

    if "--bootstrap" in flags:
        if baseline_path.exists():
            print(
                "check-module-layout: refusing to bootstrap -- "
                f"{BASELINE} already exists. Use --update, which only "
                "removes entries.",
                file=sys.stderr,
            )
            return 1
        write_baseline(baseline_path, pairs)
        print(
            f"check-module-layout: wrote {BASELINE} with {len(pairs)} "
            "pair(s). Every one is debt; split the file to remove its line."
        )
        return 0

    known = set(baseline)
    new = [p for p in pairs if p not in known]
    stale = [e for e in baseline if e not in set(pairs)]

    if "--update" in flags:
        if new:
            print(
                "check-module-layout: refusing to update -- new pair(s):",
                file=sys.stderr,
            )
            for p in new:
                print(f"  {p}  (beside {p[:-3]}/)", file=sys.stderr)
            print(f"\n{REMEDY}", file=sys.stderr)
            return 1
        write_baseline(baseline_path, [e for e in baseline if e not in set(stale)])
        print(
            f"check-module-layout: {BASELINE} now lists "
            f"{len(baseline) - len(stale)} pair(s) "
            f"({len(stale)} retired)."
        )
        return 0

    if new:
        print(
            "check-module-layout: FAIL -- a code file sits beside a folder "
            "with the same name.\n",
            file=sys.stderr,
        )
        for p in new:
            print(f"  {p}  (beside {p[:-3]}/)", file=sys.stderr)
        print(f"\n{REMEDY}", file=sys.stderr)
        return 1

    kept = len(pairs)
    note = f", {len(stale)} baseline entr(y/ies) retired -- run `make module-layout-update`" if stale else ""
    print(
        f"check-module-layout: OK -- {kept} grandfathered pair(s), "
        f"none added{note}."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
