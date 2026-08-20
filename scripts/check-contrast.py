#!/usr/bin/env python3
"""Measure every pairing the colour system licenses, against WCAG 2.1 contrast.

Phase 5 of the alignment asks for a contrast audit, and an audit is worth
exactly as much as its willingness to return a number nobody wanted. So this
script states each pairing's measured ratio and the threshold it is being held
to, and fails on the pairings the system actually promises rather than on every
pair that happens to be under 4.5.

Three thresholds, and which one applies is a property of the *role*, not of the
colour:

  - 4.5:1  normal body text (WCAG 1.4.3 AA).
  - 3.0:1  large text (>=18.66px bold or >=24px), and non-text UI components and
           graphical objects (1.4.11).
  - exempt logotypes. 1.4.3 and 1.4.11 both carve out logos and brand marks by
    name, which is why a gold mark is allowed on paper where gold *text* is not.

The exemption is the load-bearing one for this palette: it is what lets rule 6
say "gold for the mark and for icons at 24px and larger, never gold body text on
light" without contradicting itself.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TOKENS = REPO / "design" / "tokens" / "stella-tokens.json"


def channels(hex_value: str) -> tuple[int, int, int]:
    raw = hex_value.lstrip("#")
    return int(raw[0:2], 16), int(raw[2:4], 16), int(raw[4:6], 16)


def relative_luminance(hex_value: str) -> float:
    """WCAG 2.1 relative luminance."""

    def linear(component: int) -> float:
        srgb = component / 255.0
        return srgb / 12.92 if srgb <= 0.04045 else ((srgb + 0.055) / 1.055) ** 2.4

    r, g, b = (linear(c) for c in channels(hex_value))
    return 0.2126 * r + 0.7152 * g + 0.0722 * b


def contrast(fg: str, bg: str) -> float:
    lum_a, lum_b = relative_luminance(fg), relative_luminance(bg)
    lighter, darker = max(lum_a, lum_b), min(lum_a, lum_b)
    return (lighter + 0.05) / (darker + 0.05)


# (foreground, background, role, threshold). A threshold of None is an
# exemption and is reported but never failed on.
PAIRINGS = [
    # ── dark surfaces ───────────────────────────────────────────────────
    ("text", "bg", "primary text on canvas", 4.5),
    ("text", "panel", "primary text on panel", 4.5),
    ("text", "hl", "primary text on a selected row", 4.5),
    ("muted", "bg", "secondary text on canvas", 4.5),
    ("muted", "panel", "secondary text on panel", 4.5),
    ("silver", "bg", "incoming-context emphasis", 4.5),
    ("silver-type", "bg", "syntax types", 4.5),
    ("gold", "bg", "actions, money, active tabs", 4.5),
    ("gold", "panel", "actions on a panel", 4.5),
    ("green", "bg", "pass verdict", 4.5),
    ("red", "bg", "fail verdict", 4.5),
    ("dim", "bg", "hints and line numbers (large/decorative floor)", 3.0),
    ("comment", "panel", "code comments (large/decorative floor)", 3.0),
    ("border", "bg", "hairlines — a divider, not information", None),
    ("rule", "bg", "section rules — a divider, not information", None),
    ("gold-bright", "bg", "single-cell live indicators", 3.0),
    # ── light surfaces ──────────────────────────────────────────────────
    ("ink", "paper", "primary text on light", 4.5),
    ("ink", "paper-panel", "primary text on a light panel", 4.5),
    ("dim", "paper", "secondary text on light", 4.5),
    ("paper-border", "paper", "light hairlines", None),
    # Rule 6: gold on light is the mark, icons >=24px, borders, and filled
    # elements carrying ink text — never body text or links. The first is a
    # logotype exemption; the last is a real 4.5 pairing and is measured as one.
    ("gold", "paper", "the MARK on light — logotype exemption", None),
    ("ink", "gold", "ink text on a filled gold button", 4.5),
]


def main() -> int:
    doc = json.loads(TOKENS.read_text())
    hexes = {t["name"]: t["hex"] for t in doc["tokens"]}

    failures: list[str] = []
    rows: list[tuple[str, str, str, float, str]] = []

    for fg, bg, role, threshold in PAIRINGS:
        ratio = contrast(hexes[fg], hexes[bg])
        if threshold is None:
            verdict = "exempt"
        elif ratio >= threshold:
            verdict = f"pass ({threshold})"
        else:
            verdict = f"FAIL ({threshold})"
            failures.append(
                f"{fg} on {bg} — {role}: {ratio:.2f}:1, needs {threshold}:1"
            )
        rows.append((fg, bg, role, ratio, verdict))

    width = max(len(f"{fg} on {bg}") for fg, bg, _, _, _ in rows)
    print(f"{'pairing'.ljust(width)}  ratio   verdict   role")
    for fg, bg, role, ratio, verdict in rows:
        print(f"{f'{fg} on {bg}'.ljust(width)}  {ratio:5.2f}:1  {verdict:<9} {role}")

    if failures:
        print(f"\n{len(failures)} pairing(s) below threshold:", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        return 1

    print("\nevery licensed pairing clears its threshold")
    return 0


if __name__ == "__main__":
    sys.exit(main())
