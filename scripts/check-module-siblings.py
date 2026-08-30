#!/usr/bin/env python3
"""Guard: no NEW `foo.rs` sitting beside a `foo/` directory.

AGENTS.md § "Code style and conventions" has forbidden this pair for as long as
it has existed, and nothing enforced it. The rule held by reviewer memory alone,
which lasts exactly as long as every reviewer remembers. A pull request created
one, called it an improvement in its own description, and merged green through
the pre-push hook, every CI workflow and the post-merge canary; a second undid
it. This is the check that would have caught it at the author's push.

## What it does NOT claim

`foo.rs` beside `foo/` is the module layout Rust 2018 introduced, and it is the
mainstream convention today — the Book presents it as the current form and calls
`mod.rs` the older style. This repository chose the other one, and the entries in
`scripts/module-siblings-baseline.txt` record how far the tree is from that
choice: they outnumber the production `mod.rs` files by more than two orders of
magnitude, and the tree has been moving further that way rather than back.

So this guard enforces the half of the rule the tree can hold today — *do not add
new ones* — and takes no position on the pairs already there. Whether the rule
survives or the mainstream layout wins is a maintainer's decision. If it is the
latter, delete this file and its baseline; do not weaken it into something that
reports enforcement it is not doing.

## The baseline is a down-only ratchet

`scripts/module-siblings-baseline.txt` records today's pairs, so this lands green
and any cleanup shrinks a list the guard already enforces. It may never gain an
entry: `--update` refuses to add one, the same door the typed-errors and
core-reachability ratchets use, and the only way past a red run is to put the
file inside the folder as `mod.rs`.

Scoped to `crates/*/src`. A `tests/common/mod.rs` is the canonical integration
-test helper layout and is not a module of the crate under test.

This is a fact about the repository rather than about a crate, so it is never
scoped by CARGO_SCOPE (AGENTS.md § "The gate"), and reading directory entries
keeps it in the toolchain-free `guards-fast` rung.

Usage:
    ./scripts/check-module-siblings.py [ROOT]
    ./scripts/check-module-siblings.py --update [ROOT]      # shrink the baseline
    ./scripts/check-module-siblings.py --bootstrap [ROOT]   # once, to create it
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

BASELINE = "scripts/module-siblings-baseline.txt"

BASELINE_HEADER = """\
# Every `foo.rs` that sits beside a `foo/` directory, which AGENTS.md forbids.
#
# A down-only ratchet: `check-module-siblings.py --update` refuses to add a
# line, so a red run is cleared by moving the file to `foo/mod.rs`, never by
# recording it here. Generated — do not hand-edit.
"""


def tracked_sources(root: Path) -> list[str]:
    """Every tracked or untracked-but-not-ignored `.rs` file under a crate's src.

    Untracked files count, so a pair fails on the push that creates it rather
    than on the one after it.
    """
    try:
        proc = subprocess.run(
            ["git", "ls-files", "--cached", "--others", "--exclude-standard"],
            cwd=root,
            capture_output=True,
            text=True,
            timeout=60,
        )
    except (OSError, subprocess.SubprocessError):
        return []
    if proc.returncode != 0:
        return []
    return [
        line
        for line in proc.stdout.splitlines()
        if line.endswith(".rs")
        and line.startswith("crates/")
        and "/src/" in line
    ]


def pairs(root: Path) -> list[str]:
    """The `foo.rs` paths whose sibling `foo/` directory exists."""
    found = []
    for path in tracked_sources(root):
        if (root / path[: -len(".rs")]).is_dir():
            found.append(path)
    return sorted(found)


def read_baseline(path: Path) -> set[str]:
    if not path.exists():
        return set()
    return {
        line.strip()
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip() and not line.startswith("#")
    }


def main() -> int:
    argv = [a for a in sys.argv[1:] if not a.startswith("--")]
    update = "--update" in sys.argv[1:]
    root = Path(argv[0]) if argv else Path(__file__).resolve().parent.parent
    root = root.resolve()

    baseline_path = root / BASELINE
    current = set(pairs(root))
    baseline = read_baseline(baseline_path)

    # The one-time door. `--update` cannot create the baseline, because it
    # refuses to add an entry and every pair would be one; this records the
    # pre-existing debt once and then closes behind itself, so a later run can
    # never re-grandfather a pair somebody added in the meantime.
    if "--bootstrap" in sys.argv[1:]:
        if baseline_path.exists():
            print(
                f"check-module-siblings: refusing to bootstrap -- {BASELINE} "
                "already exists. Use --update, which only ever removes a line.",
                file=sys.stderr,
            )
            return 1
        listed = sorted(current)
        baseline_path.write_text(
            BASELINE_HEADER + "\n".join(listed) + ("\n" if listed else ""),
            encoding="utf-8",
        )
        print(f"check-module-siblings: wrote {BASELINE} with {len(listed)} pair(s).")
        return 0

    if update:
        added = sorted(current - baseline)
        if added:
            print("check-module-siblings: REFUSING to grow the baseline.\n")
            print("These pairs are new debt, not recorded debt:")
            for path in added:
                print(f"  {path}  (beside {path[:-3]}/)")
            print(
                "\nThe ratchet only goes down. Move the file inside the folder as\n"
                "`mod.rs` and re-export its modules from there, so every existing\n"
                "import keeps working — do not record it here."
            )
            return 1
        kept = sorted(baseline & current)
        baseline_path.write_text(
            BASELINE_HEADER + "\n".join(kept) + ("\n" if kept else ""),
            encoding="utf-8",
        )
        for path in sorted(baseline - current):
            print(f"check-module-siblings: retired {path} — the pair is gone")
        print(f"check-module-siblings: baseline holds {len(kept)} pair(s)")
        return 0

    new_debt = sorted(current - baseline)
    resolved = sorted(baseline - current)

    if new_debt:
        print("check-module-siblings: FAILED\n")
        print("These code files sit beside a folder of the same name:\n")
        for path in new_debt:
            print(f"  {path}  (beside {path[:-3]}/)")
        print(
            "\nAGENTS.md: a code file may not sit beside a folder with the same\n"
            "name. Split it into modules inside the folder and re-export them\n"
            "from `mod.rs`, so every existing import keeps working.\n"
            "Do NOT add a baseline entry — the ratchet only goes down."
        )
        return 1

    if resolved:
        print("check-module-siblings: baseline is STALE\n")
        print("These pairs are gone, so their baseline entries are dead:\n")
        for path in resolved:
            print(f"  {path}")
        print("\nRun ./scripts/check-module-siblings.py --update and commit the diff.")
        return 1

    print(
        f"check-module-siblings: OK — {len(baseline)} recorded pair(s), none added."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
