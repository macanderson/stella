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
)

# Surfaces this system has not reached yet. Each entry names the issue that
# closes it. This list is meant to reach empty; nothing in this script can add
# to it.
MIGRATING: dict[str, str] = {}

HEX = re.compile(r"#[0-9A-Fa-f]{6}\b")
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
