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

The logotype exemption is what lets rule 6 say "gold for the mark and for icons
at 24px and larger, never gold body text on light" without contradicting
itself.

Four pairings sit under their threshold today, and they are palette values
rather than an oversight: `muted` is 4.47:1 on the canvas against a 4.5 floor,
and `dim`/`comment` are decorative tiers the terminal already documents as
below every text floor. Moving them is a recolour and belongs to whoever owns
the palette (#4063), so this guard records them in a **down-only ratchet**
(`scripts/contrast-baseline.txt`) rather than waiting to become a gate step
until they are fixed -- which is what it did for two releases, in no gate at
all, while the tree carried figures that disagreed with it (#4423).

The ratchet records a **measured ratio**, not a count, which is what makes it
strictly stronger than the threshold it stands in for: a baselined pairing is
held to the exact number it shipped at, so darkening `muted` by one point fails
even though it was already failing. `--update` rewrites a recorded ratio
upward, drops a pairing that has climbed back over its threshold, and refuses
both to lower a floor and to admit a pairing that is not already listed.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TOKENS_REL = Path("design") / "tokens" / "stella-tokens.json"
BASELINE_REL = Path("scripts") / "contrast-baseline.txt"

BASELINE_HEADER = """\
# Down-only ratchet for scripts/check-contrast.py.
#
# Each line is `<foreground> <background> <ratio>` -- the WCAG 2.1 contrast the
# pairing measured when it was recorded. A pairing may only get *lighter*: the
# guard fails if the measured ratio drops below the number here, and it fails on
# any sub-threshold pairing that has no line at all.
#
# `make contrast-update` rewrites a number upward, drops a pairing that now
# clears its threshold, and refuses to lower a floor or to add a pairing. So the
# only way past a red gate is to move the colour -- see `make contrast-report`
# for every pairing and the ground it is measured on.
#
# This file records the debt #4423 found; it is meant to reach empty.
"""


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


def measure(root: Path) -> list[tuple[str, str, str, float | None, float]]:
    """Every licensed pairing, as (fg, bg, role, threshold, ratio)."""
    doc = json.loads((root / TOKENS_REL).read_text())
    hexes = {t["name"]: t["hex"] for t in doc["tokens"]}
    return [
        (fg, bg, role, threshold, round(contrast(hexes[fg], hexes[bg]), 2))
        for fg, bg, role, threshold in PAIRINGS
    ]


def read_baseline(root: Path) -> dict[tuple[str, str], float]:
    path = root / BASELINE_REL
    if not path.exists():
        return {}
    floors: dict[tuple[str, str], float] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        fg, bg, ratio = line.split()
        floors[(fg, bg)] = float(ratio)
    return floors


def write_baseline(root: Path, floors: dict[tuple[str, str], float]) -> None:
    body = "".join(f"{fg} {bg} {ratio:.2f}\n" for (fg, bg), ratio in sorted(floors.items()))
    (root / BASELINE_REL).write_text(BASELINE_HEADER + body, encoding="utf-8")


def report(root: Path, rows: list[tuple[str, str, str, float | None, float]]) -> int:
    floors = read_baseline(root)
    width = max(len(f"{fg} on {bg}") for fg, bg, _, _, _ in rows)
    print(f"{'pairing'.ljust(width)}  ratio   verdict    role")
    for fg, bg, role, threshold, ratio in rows:
        if threshold is None:
            verdict = "exempt"
        elif ratio >= threshold:
            verdict = f"pass ({threshold})"
        elif (fg, bg) in floors:
            verdict = f"held ({floors[(fg, bg)]:.2f})"
        else:
            verdict = f"FAIL ({threshold})"
        print(f"{f'{fg} on {bg}'.ljust(width)}  {ratio:5.2f}:1  {verdict:<10} {role}")
    print(
        "\nEvery ratio is measured against the named background token in "
        "design/tokens/stella-tokens.json, so a figure quoted elsewhere against "
        "a different ground will not match this table."
    )
    return 0


def update(root: Path, rows: list[tuple[str, str, str, float | None, float]]) -> int:
    floors = read_baseline(root)
    merged: dict[tuple[str, str], float] = {}
    lowered: list[str] = []
    unlisted: list[str] = []

    for fg, bg, role, threshold, ratio in rows:
        if threshold is None or ratio >= threshold:
            continue
        recorded = floors.get((fg, bg))
        if recorded is None:
            unlisted.append(f"{fg} on {bg} — {role}: {ratio:.2f}:1, needs {threshold}:1")
            continue
        if ratio < recorded:
            lowered.append(f"{fg} on {bg}: {recorded:.2f}:1 -> {ratio:.2f}:1")
            continue
        merged[(fg, bg)] = ratio

    if unlisted or lowered:
        print("check-contrast: refusing to update.", file=sys.stderr)
        for line in unlisted:
            print(f"  new sub-threshold pairing: {line}", file=sys.stderr)
        for line in lowered:
            print(f"  darkened: {line}", file=sys.stderr)
        print(
            "\nThis ratchet only ever tightens. Move the colour, or take the "
            "pairing out of PAIRINGS if the system no longer licenses it.",
            file=sys.stderr,
        )
        return 1

    write_baseline(root, merged)
    print(f"check-contrast: {BASELINE_REL.name} retightened to {len(merged)} pairing(s).")
    return 0


def bootstrap(root: Path, rows: list[tuple[str, str, str, float | None, float]]) -> int:
    """Write the ratchet for the first time. Runs once, and says so afterwards."""
    if (root / BASELINE_REL).exists():
        print(
            f"check-contrast: refusing to bootstrap -- {BASELINE_REL.name} already "
            "exists. Use --update, which only ever raises a floor.",
            file=sys.stderr,
        )
        return 1
    floors = {
        (fg, bg): ratio
        for fg, bg, _, threshold, ratio in rows
        if threshold is not None and ratio < threshold
    }
    write_baseline(root, floors)
    print(f"check-contrast: wrote {BASELINE_REL.name} with {len(floors)} pairing(s).")
    return 0


def main() -> int:
    flags = {a for a in sys.argv[1:] if a.startswith("--")}
    positional = [a for a in sys.argv[1:] if not a.startswith("--")]
    # A root argument is what lets scripts/test-contrast.sh drive the writing
    # paths against a fixture tree instead of rewriting this repository's own
    # baseline as a side effect of running the tests.
    root = Path(positional[0]).resolve() if positional else REPO
    rows = measure(root)

    if "--report" in flags:
        return report(root, rows)
    if "--bootstrap" in flags:
        return bootstrap(root, rows)
    if "--update" in flags:
        return update(root, rows)

    floors = read_baseline(root)
    failures: list[str] = []
    held = 0

    for fg, bg, role, threshold, ratio in rows:
        if threshold is None or ratio >= threshold:
            continue
        recorded = floors.get((fg, bg))
        if recorded is None:
            failures.append(
                f"{fg} on {bg} — {role}: {ratio:.2f}:1, needs {threshold}:1 "
                "(no baseline entry; a new sub-threshold pairing is a defect)"
            )
        elif ratio < recorded:
            failures.append(
                f"{fg} on {bg} — {role}: {ratio:.2f}:1, darker than the "
                f"{recorded:.2f}:1 the ratchet holds it to"
            )
        else:
            held += 1

    stale = sorted(
        f"{fg} on {bg}"
        for (fg, bg), _ in floors.items()
        if all(
            not (fg == f and bg == b and t is not None and r < t)
            for f, b, _, t, r in rows
        )
    )
    for pairing in stale:
        failures.append(
            f"{pairing}: baselined, but it clears its threshold now — "
            "run `make contrast-update` so the ratchet retightens"
        )

    if failures:
        print("check-contrast: FAIL\n", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        print(
            "\nA pairing that got darker is fixed by moving the colour, never by "
            "editing the ratchet; one that got lighter is fixed by "
            "`make contrast-update`. `make contrast-report` names every pairing "
            "and the ground it is measured on.",
            file=sys.stderr,
        )
        return 1

    print(
        f"check-contrast: {len(rows)} licensed pairing(s), "
        f"{held} held by the ratchet, none darker than it allows."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
