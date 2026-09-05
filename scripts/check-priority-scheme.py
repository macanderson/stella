#!/usr/bin/env python3
"""Guard: the issue priority scheme is written down once.

`docs/scr/SCR-005-triage-separation-of-duties.md` is the home. Its Directive
names the levels and what each one means. Everything else cites it.

Four documents each named a scheme of their own and no two agreed: five levels
in two of them, three in a third, and a hedge in the fourth about repositories
that stop earlier. The regex in `.github/workflows/triage-guard.yml` is code,
so a gap there has teeth. An issue labelled with a level the regex misses
looks triaged to a person and unprioritised to the guard, which then never
strips it from a login SCR-005 says may not set it. Making the copies agree
fixes the day. Reading them fixes the shape.

Two rules:

1. The regex in the triage guard covers every level SCR-005 names, and no
   more. It is the one derived copy, because a workflow cannot read a
   document at the moment it runs.
2. No other file in the tree states the scheme. A span from the first level
   to the last one, or a character class over them, is a second copy waiting
   to drift. Cite SCR-005 there instead.

Run it with `make priority-scheme`. `scripts/test-priority-scheme.py` runs it
against throwaway trees to show it can still fail.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

SCR = "docs/scr/SCR-005-triage-separation-of-duties.md"
WORKFLOW = ".github/workflows/triage-guard.yml"

# Where the scheme is declared inside the SCR, and the token it declares each
# level as.
SCHEME_HEADING = "Priority scheme:"
LEVEL = re.compile(r"`P(\d)`")

# The two shapes that state a scheme rather than use one level of it. A
# character class is how a regex spells the range; a span is how prose spells
# it. "`P0` and `P2`" is neither, so a sentence naming two levels as examples
# is left alone.
CLASS = re.compile(r"P\[0-(\d)\]")
SPAN = re.compile(r"`P\d`\s*(?:-|–|—|\.\.\.|…|to|through)\s*`P\d`")

# Files worth reading. The rest of a checkout is build output, an image, or a
# lockfile.
SUFFIXES = (
    ".md",
    ".mdx",
    ".rs",
    ".py",
    ".sh",
    ".toml",
    ".yml",
    ".yaml",
    ".ts",
    ".tsx",
    ".js",
    ".mjs",
    ".json",
)

SKIP_DIRS = {".git", "target", "node_modules", ".next", "dist", "out", "vendor"}


def scan_files(root: Path) -> list[Path]:
    """Every text file under `root`, minus the directories nobody edits."""
    found: list[Path] = []
    stack = [root]
    while stack:
        for entry in sorted(stack.pop().iterdir()):
            if entry.is_symlink():
                continue
            if entry.is_dir():
                if entry.name not in SKIP_DIRS:
                    stack.append(entry)
            elif entry.suffix in SUFFIXES:
                found.append(entry)
    return found


def read(path: Path) -> str:
    return path.read_text(encoding="utf-8", errors="replace")


def declared_levels(text: str) -> tuple[list[int], str]:
    """The levels SCR-005 declares, or no levels and the reason why not."""
    start = text.find(SCHEME_HEADING)
    if start < 0:
        return [], f"{SCR} has no `{SCHEME_HEADING}` line"
    # The declaration runs to the end of its paragraph, so a wrapped list is
    # read whole.
    end = text.find("\n\n", start)
    block = text[start:] if end < 0 else text[start:end]
    levels = [int(n) for n in LEVEL.findall(block)]
    if not levels:
        return [], f"{SCR} has a `{SCHEME_HEADING}` line with no levels under it"
    if levels != list(range(len(levels))):
        return [], (
            f"{SCR} declares the levels {levels}, which do not run from 0 up "
            "with no gaps"
        )
    return levels, ""


def main() -> int:
    parser = argparse.ArgumentParser(description="Check the priority scheme.")
    parser.add_argument(
        "--manifest-dir",
        default=str(Path(__file__).resolve().parent.parent),
        help="tree to check (default: this repository)",
    )
    args = parser.parse_args()
    root = Path(args.manifest_dir).resolve()

    report: list[str] = []

    def note(line: str) -> None:
        report.append(f"check-priority-scheme: {line}")

    def verdict() -> int:
        print("\n".join(report), file=sys.stderr)
        return 1

    scr = root / SCR
    if not scr.is_file():
        note(f"FAIL - {SCR} is missing, so nothing declares the scheme.")
        return verdict()

    levels, why = declared_levels(read(scr))
    if not levels:
        note(f"FAIL - {why}.")
        note("     Write the scheme as one paragraph of `P0` `P1` ... tokens.")
        return verdict()
    top = levels[-1]

    workflow = root / WORKFLOW
    if not workflow.is_file():
        note(f"FAIL - {WORKFLOW} is missing, so no guard holds the scheme.")
        return verdict()

    ceilings = CLASS.findall(read(workflow))
    if not ceilings:
        note(f"FAIL - {WORKFLOW} has no `P[0-N]` pattern in it.")
        note(f"     It is the one copy of the scheme, and {SCR} says `P0`-`P{top}`.")
        return verdict()
    for ceiling in ceilings:
        if int(ceiling) != top:
            note(f"FAIL - {WORKFLOW} matches up to `P{ceiling}`.")
            note(f"     {SCR} names {len(levels)} levels, up to `P{top}`.")
            note("     A level the labels have and this regex does not is a")
            note("     hole in both directions: the guard never strips it, and")
            note("     an issue carrying only it reads as unprioritised.")
            return verdict()

    fail = False
    for path in scan_files(root):
        rel = path.relative_to(root).as_posix()
        if rel == SCR:
            continue
        for number, line in enumerate(read(path).splitlines(), 1):
            second_class = rel != WORKFLOW and CLASS.search(line)
            if second_class or SPAN.search(line):
                note(f"FAIL - {rel}:{number} states the priority scheme.")
                note(f"     {line.strip()}")
                fail = True

    if fail:
        note(f"     The scheme lives in {SCR}. {WORKFLOW} copies it, because")
        note("     code cannot read a document as it runs. Every other mention")
        note("     cites SCR-005 rather than naming the levels again.")
        return verdict()

    print(f"check-priority-scheme: ok - {len(levels)} levels, `P0` up to `P{top}`.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
