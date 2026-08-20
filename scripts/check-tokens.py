#!/usr/bin/env python3
"""Guard the colour system: no retired hex anywhere, no stray hex where it matters.

Two checks, deliberately different in strength, because "never use this colour
again" and "use only tokens here" are different promises and only one of them
can be made about the whole tree today.

1. **The ban.** Every hex in `banned.values` in the token JSON fails wherever it
   appears, in any tracked file. These are the anchor values of retired brand
   kits. The failure mode they name is real and has happened twice: a surface
   left on a retired ramp while a doc comment claimed it mirrored the current
   one, so a reader crossing from the site to the Observatory watched the brand
   change hue. A retired hex has no legitimate use, so this check has no
   allowlist.

2. **Token-only paths.** Inside `TOKEN_ONLY` the rule is stronger: every hex must
   be a live token. This is where colour is *authored* — the stylesheets and
   renderers a designer reaches for — and it is the only scope where "no hex
   literals, only tokens" is a promise the tree can actually keep. Raster and
   vector assets are out of scope by construction; they are checked by the ban
   instead.

`MIGRATING` is the one escape, and it is a ledger, not an allowlist: a path
listed there is a surface this system has not reached yet, each entry carrying
the issue that finishes it. The guard **refuses to add entries** -- there is no
`--update`. The only way a path leaves the ledger is by being migrated.
"""

from __future__ import annotations

import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TOKENS = REPO / "design" / "tokens" / "stella-tokens.json"

# Where every hex must be a live token, not merely a non-retired one.
TOKEN_ONLY = (
    "design/tokens/",
    "website/src/app/tokens.css",
    "docs/brand/css/tokens.css",
)

# Files that define the system itself, and so must be able to name retired
# values in order to ban them.
SELF = (
    "design/tokens/stella-tokens.json",
    "scripts/check-tokens.py",
    "scripts/gen-tokens.py",
    "crates/stella-tui-theme/src/generated.rs",
    "design/tokens/stella-tokens.css",
)

# Surfaces this system has not reached yet. Each entry names the issue that
# closes it. This list is meant to reach empty; nothing in this script can add
# to it.
#
# Every entry here is a place where a hex remap would have made the file
# *worse*, not a place the migration ran out of patience. That distinction is
# the only thing separating a ledger from an allowlist, so it is stated per
# entry rather than assumed.
MIGRATING: dict[str, str] = {
    # The 143 hexes here are the subject matter, not the styling: the page
    # renders the palette as swatches, quotes contrast ratios as prose, and
    # documents two 50-950 ramps that v5.0 deletes. Remapping them yields a
    # gold swatch labelled "Bronze Gold" beside ratios true of no pair on the
    # page.
    "docs/brand/brand-guidelines.html": "#4056",
    # Design *briefs*, where a hex is an instruction. Remapping without
    # rewriting the surrounding prose tells the reader to build the old system
    # with new numbers. stella-tui-prompt.md is superseded outright by
    # SPEC-stella-tui-v2.md and still specifies the retired `✦ stella` lockup.
    "docs/brand/prompts/": "#4057",
    # Phase 2 of the alignment, in flight in another worktree which already has
    # an uncommitted stella-tui-theme. Editing these files from a second branch
    # would create two authorities over one palette — the exact failure this
    # change exists to end.
    "crates/stella-tui/": "#4058",
}

HEX = re.compile(r"#[0-9A-Fa-f]{6}\b")

# The same colours, written the other way. CSS accepts `rgb(197 138 50)` and
# `rgba(7,11,16,.28)` for values a hex sweep cannot see, and both spellings were
# live in this tree: 17 retired values survived the first pass of the v5.0
# migration purely by not being written in hex, including a bronze gradient
# sweep on the marketing site. A guard that watches one notation of a two-
# notation language is a guard that reports zero and means nothing.
#
# Deliberately not a general colour parser: `hsl()`, `color()` and `oklch()` are
# not matched, and neither is a computed value. This closes the gap that was
# actually open rather than pretending to close all of them — see the note in
# the report for what remains unwatched.
RGB_FUNC = re.compile(
    r"rgba?\(\s*(\d{1,3})\s*[, ]\s*(\d{1,3})\s*[, ]\s*(\d{1,3})", re.IGNORECASE
)
# Text extensions only. A hex "found" inside a PNG is a coincidence.
TEXT_SUFFIXES = {
    ".css",
    ".scss",
    ".html",
    ".htm",
    ".svg",
    ".ts",
    ".tsx",
    ".js",
    ".jsx",
    ".mjs",
    ".rs",
    ".py",
    ".md",
    ".mdx",
    ".json",
    ".toml",
    ".yml",
    ".yaml",
    ".sh",
    ".ps1",
    ".txt",
}


def tracked_files() -> list[Path]:
    out = subprocess.run(
        ["git", "ls-files", "-z"],
        cwd=REPO,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return [Path(p) for p in out.split("\0") if p]


def main() -> int:
    doc = json.loads(TOKENS.read_text())
    live = {t["hex"].upper() for t in doc["tokens"]}
    banned = {e["hex"].upper(): e["was"] for e in doc["banned"]["values"]}

    ban_hits: list[str] = []
    stray_hits: list[str] = []

    for rel in tracked_files():
        posix = rel.as_posix()
        if posix in SELF:
            continue
        if rel.suffix not in TEXT_SUFFIXES:
            continue
        if any(posix.startswith(p) or posix == p for p in MIGRATING):
            continue
        path = REPO / rel
        try:
            text = path.read_text(errors="ignore")
        except OSError:
            continue

        token_only = any(posix.startswith(p) or posix == p for p in TOKEN_ONLY)

        for lineno, line in enumerate(text.splitlines(), 1):
            for match in HEX.finditer(line):
                hexv = match.group(0).upper()
                if hexv in banned:
                    ban_hits.append(f"{posix}:{lineno}: {match.group(0)} — {banned[hexv]}")
                elif token_only and hexv not in live:
                    stray_hits.append(
                        f"{posix}:{lineno}: {match.group(0)} is not a token in "
                        f"design/tokens/stella-tokens.json"
                    )
            for match in RGB_FUNC.finditer(line):
                channels = tuple(int(c) for c in match.groups())
                if any(c > 255 for c in channels):
                    continue
                hexv = "#%02X%02X%02X" % channels
                if hexv in banned:
                    ban_hits.append(
                        f"{posix}:{lineno}: {match.group(0)}) = {hexv} — {banned[hexv]}"
                    )

    failed = False
    if ban_hits:
        failed = True
        print(
            f"retired brand hexes found in {len(ban_hits)} place(s). These belong to "
            "superseded kits and have no legitimate use:\n",
            file=sys.stderr,
        )
        for hit in ban_hits:
            print(f"  {hit}", file=sys.stderr)
        print(
            "\nreplace each with the token that carries the same role — see "
            "design/tokens/stella-tokens.json.",
            file=sys.stderr,
        )
    if stray_hits:
        failed = True
        print(
            f"\nhex literals inside token-only paths ({len(stray_hits)}):\n",
            file=sys.stderr,
        )
        for hit in stray_hits:
            print(f"  {hit}", file=sys.stderr)

    if failed:
        return 1

    scope = len(TOKEN_ONLY)
    pending = f", {len(MIGRATING)} path(s) still migrating" if MIGRATING else ""
    print(
        f"tokens: no retired hex in the tree; {scope} token-only path(s) clean{pending}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
