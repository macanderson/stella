#!/usr/bin/env python3
"""Emit the brand-font coverage table for SPEC 4's glyph vocabulary.

    uv run --with fonttools --with pyobjc-framework-CoreText \\
        python scripts/gen-glyph-coverage.py

It parses every `char` literal out of `crates/stella-tui-theme/src/glyph.rs`,
asks JetBrains Mono's `cmap` whether each one is there, and — on macOS — asks
CoreText which face actually draws the ones that are not. The output is a Rust
table to paste into `glyph.rs`; the walking test lives beside it.

## Why this is a script and not a gate step

`make gate` may not assume a font is installed, and a guard that silently
skips when one is missing reports green over an unrun check. So the *table* is
committed and the test walks `glyph::ALL` against it with no font involved,
which is what makes the check portable; this script is how a human regenerates
the table after adding a glyph, and it says so in the table's own doc comment.

## What each half of a row can be trusted to mean

The `Native`/`Substituted` split is a fact about the font file and travels
anywhere. The substitute *face* is a fact about this machine's fallback chain:
CoreText on macOS. A bare Linux terminal running DejaVu Sans Mono resolves the
same codepoints differently, and for some of them to nothing at all — which is
the hazard the table exists to keep visible, not one it can measure from here.
"""

from __future__ import annotations

import os
import re
import sys
import unicodedata
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
GLYPHS = REPO / "crates" / "stella-tui-theme" / "src" / "glyph.rs"
BRAND_FONT = Path.home() / "Library" / "Fonts" / "JetBrainsMono-Regular.ttf"

# A `char` literal in a const or an array: `'◐'` or `'\u{FF0B}'`. Deliberately
# narrow — it reads declarations, not prose, so a glyph named in a doc comment
# does not enter the table without being declared.
CHAR_LITERAL = re.compile(r"'(\\u\{([0-9A-Fa-f]{2,6})\}|[^'\\])'")


def vocabulary() -> list[str]:
    """Every character `glyph.rs` declares, in codepoint order."""
    source = GLYPHS.read_text(encoding="utf-8")
    chars: set[str] = set()
    for whole, escape in CHAR_LITERAL.findall(source):
        chars.add(chr(int(escape, 16)) if escape else whole)
    chars.discard(" ")  # BLOCK_EIGHTHS[0] is an empty cell, not a glyph.
    return sorted(chars)


def brand_coverage() -> set[int]:
    from fontTools.ttLib import TTFont  # noqa: PLC0415 — optional, script-only

    font = TTFont(os.fspath(BRAND_FONT))
    return set().union(*(set(table.cmap) for table in font["cmap"].tables))


def substitute_face(ch: str) -> str:
    """The face CoreText picks for `ch` with JetBrains Mono as the base."""
    try:
        from CoreText import (  # noqa: PLC0415 — optional, macOS-only
            CTFontCopyFamilyName,
            CTFontCreateForString,
            CTFontCreateWithName,
        )
    except ImportError:
        return "unmeasured"
    base = CTFontCreateWithName("JetBrains Mono", 12.0, None)
    resolved = CTFontCreateForString(base, ch, (0, len(ch)))
    return str(CTFontCopyFamilyName(resolved))


def main() -> int:
    if not BRAND_FONT.exists():
        print(
            f"gen-glyph-coverage: {BRAND_FONT} is not installed. This script "
            "needs the brand font; the committed table is what the gate reads.",
            file=sys.stderr,
        )
        return 1

    covered = brand_coverage()
    rows: list[str] = []
    for ch in vocabulary():
        name = unicodedata.name(ch, "<unnamed>")
        if ord(ch) in covered:
            rows.append(f"    ('\\u{{{ord(ch):04X}}}', Coverage::Native), // {name}")
        else:
            face = substitute_face(ch)
            rows.append(
                f'    (\'\\u{{{ord(ch):04X}}}\', Coverage::Substituted("{face}")), // {name}'
            )

    print(f"pub const COVERAGE: [(char, Coverage); {len(rows)}] = [")
    print("\n".join(rows))
    print("];")
    return 0


if __name__ == "__main__":
    sys.exit(main())
