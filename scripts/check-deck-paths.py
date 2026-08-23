#!/usr/bin/env python3
"""Guard: every repository path a deck cites still resolves. See #3573.

The decks under ``website/public/presentations/`` name source files so a
reader can go open them -- the turn-loop deck's appendix slide "A1 . Code map
-- where each phase lives" is a table of nothing but paths, and its whole value
is that each row is clickable in the reader's editor. Its audience is new
engineers on their first engine change, which is exactly the reader least able
to tell a moved module from one they cannot find.

Nothing checked them. ``deck-fit.mjs`` measures overflow and skips the
turn-loop deck outright (exit 3: it is a scrolling document, not a fixed-canvas
deck), and ``check-doc-links`` walks documents under ``docs/`` that carry
frontmatter ids. So a crate split, a module move or a rename left the deck
pointing at nothing, and the failure was invisible until a reader clicked
through -- the shared-cell drift shape ``check-god-files.sh`` was written for
(#1435): one authoritative source, several prose copies, no tiebreaker.

Two spellings, because the code-map table uses both. Some ``<td class="mono">``
cells carry the ``crates/`` prefix and some do not, so a bare
``stella-core/src/driver.rs`` is normalised to ``crates/stella-core/src/...``
before the existence check rather than being skipped.

Compiles nothing, so it is a ``guards-fast`` step. ``ci.yml`` ignores
``website/**`` entirely, which would leave a deck-only PR unchecked -- so
``deck-fit.yml`` runs it too, before its browser setup, since that workflow's
trigger is the deck directory itself.

    ./scripts/check-deck-paths.py [root ...]

Exit 0 when every cited path resolves, 1 otherwise, naming each dead path with
the deck and line it came from.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_ROOT = REPO_ROOT / "website" / "public" / "presentations"

# A repository path, in either spelling. The character class deliberately
# excludes `:` and space, so the `::symbol` and ` -- note` suffixes the deck
# appends to some cells (`prompt.rs::assemble_system_prompt`,
# `loop_detect.rs -- same_record`) end the match instead of corrupting it.
PATH_RE = re.compile(r"(?:crates/)?stella-[a-z0-9-]+/src/[A-Za-z0-9_/.]*")


def normalise(raw: str) -> str:
    """The path as it exists on disk, or "" if the match is not one.

    Trailing dots are sentence punctuation, not part of a filename. A match
    that is only a crate prefix (`stella-core/src/`) is a directory citation
    and stays as it is -- the caller checks directories too.
    """
    path = raw.rstrip(".")
    if not path:
        return ""
    if not path.startswith("crates/"):
        path = "crates/" + path
    return path


def scan(root: Path) -> tuple[list[str], int]:
    """Return (failures, paths_checked) for every deck under *root*."""
    failures: list[str] = []
    checked = 0
    seen: set[tuple[str, str]] = set()

    for html in sorted(root.rglob("*.html")):
        # Relative to the repository when the deck lives in it, absolute when
        # it does not -- scripts/test-deck-paths.sh points this at a fixture
        # tree in $TMPDIR, because a deliberately-dead path committed under
        # website/public/presentations/ would red-line the real job.
        try:
            rel_deck: Path | str = html.relative_to(REPO_ROOT)
        except ValueError:
            rel_deck = html
        text = html.read_text(encoding="utf-8", errors="replace")
        for lineno, line in enumerate(text.splitlines(), start=1):
            for match in PATH_RE.finditer(line):
                path = normalise(match.group(0))
                if not path:
                    continue
                key = (str(rel_deck), path)
                if key in seen:
                    continue
                seen.add(key)
                checked += 1
                if not (REPO_ROOT / path).exists():
                    failures.append(f"  {rel_deck}:{lineno}  {path}")

    return failures, checked


def main(argv: list[str]) -> int:
    roots = [Path(a).resolve() for a in argv[1:]] or [DEFAULT_ROOT]

    failures: list[str] = []
    checked = 0
    for root in roots:
        if not root.is_dir():
            print(f"check-deck-paths: no such directory: {root}", file=sys.stderr)
            return 1
        root_failures, root_checked = scan(root)
        failures.extend(root_failures)
        checked += root_checked

    if failures:
        print(
            f"check-deck-paths: FAIL -- {len(failures)} cited path(s) do not exist:",
            file=sys.stderr,
        )
        for line in failures:
            print(line, file=sys.stderr)
        print(
            "\nA deck names a source file so a reader can open it. Repoint each\n"
            "path at where the code moved, or drop the row. The decks are prose\n"
            "copies of the tree, so they follow it and never the reverse.",
            file=sys.stderr,
        )
        return 1

    print(f"check-deck-paths: OK -- {checked} cited path(s) resolve.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
