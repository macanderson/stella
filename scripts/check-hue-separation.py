#!/usr/bin/env python3
"""Guard the 30-degree hue-separation law on the *web* side of the colour system.

The law is one sentence — **no two chromatic roles may sit within 30° of each
other, and nothing may sit within 30° of the accent** — and until now it was
enforced on exactly one surface. `crates/stella-tui/src/theme/tests.rs` measures
it in OKLCH over the terminal palette; the web surfaces had no check at all.
`crates/stella-cli/tests/design_token_parity.rs` holds the eight instrument
surfaces to the same *values*, but it has no notion of hue, so the argument that
keeps the warning distinguishable from the gold identity lived entirely in a
prose comment — on the exact surface where the collision was once diagnosed with
the wrong instrument for two releases (#4071).

This is the second ruler for that law, so the whole design question is how to
stop it becoming a *different* ruler. Two rulers measuring one rule is the
defect; a copy of a matrix in a second language is how you get one.

## One ruler, checked rather than copied

The conversion cannot be *imported* across the language boundary, so it is
copied and then **held to its original**. `OKLCH_SOURCE` is the Rust
implementation; this script extracts every numeric literal from its `hue_deg`
body and every `>= <floor>` the separation assertions state, and refuses to run
if either disagrees with what it holds below. Edit the Rust matrices and this
script fails by name on the next gate run instead of quietly measuring a
different space. That includes the floor: 30 is not typed as a threshold
anywhere in here, it is read out of the assertions that already enforce it.

The extraction is deliberately brittle in the safe direction. If `hue_deg` is
lifted out of `theme/tests.rs` — which #4071's definition of done contemplates —
this script fails saying it could not find the function, which is a handoff to
whoever moved it, not a silent pass.

## What is measured

The four semantic roles of each web scheme, as declared in
`design/tokens/stella-tokens.json`: the identity, and the ok/warn/bad triple.
Every pair, both schemes. The generated CSS is checked to carry the same hex for
each of them, so a hand-edit to the stylesheet cannot dodge the law that the
JSON passed.

Then the **stated** angles. Some of these separations are quoted as figures in
prose — the Rust metric's doc comment cites two of them as the evidence for
leaving sRGB behind. A figure in a comment is exactly the thing that goes stale
under a recolour, so `CLAIMS` holds each one three ways at once: the number
must equal the computation, and the number must still appear verbatim in the
file that argues from it. Edit the palette and the computation moves; edit the
prose and the verbatim string is gone; edit this table and it agrees with
neither. No single-place change survives, which is what "generated-from or
checked-against" has to mean if it is to mean anything.

What is deliberately **not** measured: the neutral ramp, the surfaces, and the
diff row tints. Neutrals have no meaningful hue to separate — an angle between
two grays is noise, which is why the Rust side skips anything whose channels sit
within the ramp's own spread. The diff tints are the case
`crates/stella-tui-theme/src/clamp.rs` already states: a diff row is held apart
by the renderer's mandatory sign column, not by a colour test, so measuring its
hue would assert something nothing depends on.

WCAG contrast is not a substitute for any of this and is not consulted here. A
contrast ratio is a luminance relation; it cannot answer "can a reader tell
these two apart", which is a hue question. `scripts/check-contrast.py` is the
sibling that asks the luminance question, deliberately and separately.

Exit status 0 when every pair clears the floor, 1 otherwise.
"""

from __future__ import annotations

import json
import math
import re
import sys
from itertools import combinations
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TOKENS_JSON = REPO / "design" / "tokens" / "stella-tokens.json"
TOKENS_CSS = REPO / "design" / "tokens" / "stella-tokens.css"

# The Rust implementation this script is a second copy of. Everything numeric
# below is checked against it before a single angle is computed.
OKLCH_SOURCE = REPO / "crates" / "stella-tui" / "src" / "theme" / "tests.rs"

# Every numeric literal in `hue_deg`, in source order: the sRGB transfer
# function, then Björn Ottosson's linear-sRGB -> LMS matrix, then LMS -> Oklab's
# two chromatic axes, then the achromatic short-circuit and the wrap. Held as
# text, because that is the form the check compares — a float parsed and
# reformatted would let `0.5363325363` and `0.53633254` look equal.
OKLCH_LITERALS = (
    "255.0",  # 8-bit channel -> unit interval
    "0.04045",  # sRGB transfer: the linear-segment knee
    "12.92",
    "0.055",
    "1.055",
    "2.4",
    "0.4122214708",  # linear sRGB -> L
    "0.5363325363",
    "0.0514459929",
    "0.2119034982",  # linear sRGB -> M
    "0.6806995451",
    "0.1073969566",
    "0.0883024619",  # linear sRGB -> S
    "0.2817188376",
    "0.6299787005",
    "1.9779984951",  # LMS -> Oklab a
    "2.4285922050",
    "0.4505937099",
    "0.0259040371",  # LMS -> Oklab b
    "0.7827717662",
    "0.8086757660",
    "0.0",  # the achromatic short-circuit's answer
    "360.0",  # the wrap
)

# The semantic roles of each web scheme, keyed by the `surfaces` tag the token
# JSON uses for that scheme and valued by token name. One row per scheme rather
# than one shared row, because the light scheme is not a tint of the dark one:
# it re-cuts every value against paper and has to re-earn the separations rather
# than inherit them.
#
# The key doubles as the membership test — every token in a row must declare
# that surface — so a token quietly leaving a scheme fails here instead of
# leaving the row measuring a colour the scheme never draws.
SCHEMES = {
    "web-dark": {"identity": "gold", "ok": "green", "warn": "amber", "bad": "red"},
    "web-light": {
        "identity": "gold-ink",
        "ok": "green-ink",
        "warn": "amber-ink",
        "bad": "red-ink",
    },
}


# Separations that are quoted as figures somewhere, and the file that quotes
# them. `(scheme, role_a, role_b, stated, source)` — `stated` is the exact
# string the prose uses, so it can be looked for as well as compared against.
#
# Only figures a document actually argues *from* belong here, and only in files
# this guard already reads. `hue_deg`'s doc comment cites these two as the
# evidence that sRGB hue was the wrong space for a yellow brand — the second
# link in the metric's `RGB distance -> sRGB hue -> OKLCH` chain, which #4071
# asks it to keep carrying. If the palette moves under that argument, the
# argument is wrong, and this is what says so.
#
# The Observatory's `--warn` derivation quotes four more figures of the same
# kind and they are deliberately not pinned here yet: that file is prose about
# three schemes, one of which (`prefers-color-scheme:light`) declares values
# that live nowhere in the token system, so a claim table over it would be
# asserting things this guard cannot measure. See #4071's follow-up.
CLAIMS = (
    ("web-dark", "identity", "ok", "63.1°", OKLCH_SOURCE),
    ("web-dark", "identity", "bad", "78.0°", OKLCH_SOURCE),
)


class GuardError(Exception):
    """The guard cannot run — a source it reads is missing or has changed shape.

    Distinct from a law violation on purpose. A violation names two colours and
    an angle; this names a file and what could not be found in it, because the
    remedy is different and confusing the two is how a broken guard reads as a
    green palette.
    """


def _read(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except OSError as exc:
        raise GuardError(f"could not read {path.relative_to(REPO)}: {exc}") from exc


def _hue_deg_body(source: str) -> str:
    """The body of `fn hue_deg`, from its signature to the closing brace."""
    start = source.find("fn hue_deg(")
    if start < 0:
        raise GuardError(
            f"{OKLCH_SOURCE.relative_to(REPO)} no longer defines `fn hue_deg` — the "
            "OKLCH conversion this script mirrors has moved or been renamed. Point "
            "OKLCH_SOURCE at its new home and re-check OKLCH_LITERALS against it."
        )
    end = source.find("\n}\n", start)
    if end < 0:
        raise GuardError(
            f"{OKLCH_SOURCE.relative_to(REPO)}: could not find the end of `hue_deg`."
        )
    return source[start:end]


def check_ruler(source: str) -> float:
    """Assert the Rust ruler still says what this script says, and return its floor.

    Returns the separation floor in degrees, read out of the assertions that
    enforce it rather than typed here, so the two surfaces cannot disagree about
    how strict the law is.
    """
    literals = tuple(
        m.replace("_", "")
        for m in re.findall(r"\d[\d_]*\.\d[\d_]*", _hue_deg_body(source))
    )
    if literals != OKLCH_LITERALS:
        raise GuardError(
            "the OKLCH conversion in "
            f"{OKLCH_SOURCE.relative_to(REPO)}::hue_deg has changed.\n"
            f"     Rust says:   {', '.join(literals)}\n"
            f"     This script: {', '.join(OKLCH_LITERALS)}\n"
            "     Two rulers measuring one law is the defect #4071 names. Update "
            "OKLCH_LITERALS and the conversion in this file together."
        )

    floors = {
        m
        for m in re.findall(
            r"(?:\bsep|hue_separation\([^)]*\))\s*>=\s*(\d+(?:\.\d+)?)", source
        )
    }
    if len(floors) != 1:
        raise GuardError(
            f"{OKLCH_SOURCE.relative_to(REPO)} states "
            f"{len(floors) or 'no'} separation floor(s) {sorted(floors)}; this "
            "script reads the floor from there rather than declaring one, so it "
            "needs exactly one."
        )
    return float(next(iter(floors)))


def hue_deg(hexstr: str) -> float:
    """OKLCH hue in degrees `[0, 360)` — the port of `hue_deg`, held to it above."""
    raw = hexstr.lstrip("#")
    channels = (int(raw[0:2], 16), int(raw[2:4], 16), int(raw[4:6], 16))

    def linear(c: int) -> float:
        v = c / 255.0
        return v / 12.92 if v <= 0.04045 else ((v + 0.055) / 1.055) ** 2.4

    r, g, b = (linear(c) for c in channels)
    lc = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b) ** (1 / 3)
    mc = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b) ** (1 / 3)
    sc = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b) ** (1 / 3)
    a_axis = 1.9779984951 * lc - 2.4285922050 * mc + 0.4505937099 * sc
    b_axis = 0.0259040371 * lc + 0.7827717662 * mc - 0.8086757660 * sc
    if math.hypot(a_axis, b_axis) < 1e-6:
        return 0.0
    return math.degrees(math.atan2(b_axis, a_axis)) % 360.0


def separation(a: str, b: str) -> float:
    """The shortest angular distance between two hues, in degrees."""
    d = abs(hue_deg(a) - hue_deg(b))
    return min(d, 360.0 - d)


def load_tokens() -> dict[str, dict]:
    """Every token in the JSON, by name."""
    try:
        doc = json.loads(_read(TOKENS_JSON))
    except json.JSONDecodeError as exc:
        raise GuardError(f"{TOKENS_JSON.relative_to(REPO)} is not valid JSON: {exc}") from exc
    tokens = {}
    for entry in doc.get("tokens", ()):
        name = entry.get("name")
        if name:
            tokens[name] = entry
    if not tokens:
        raise GuardError(f"{TOKENS_JSON.relative_to(REPO)} declares no tokens.")
    return tokens


def load_css() -> dict[str, str]:
    """Every custom property the generated stylesheet declares, by property name."""
    return {
        prop: value.upper()
        for prop, value in re.findall(
            r"(--[a-z0-9-]+)\s*:\s*(#[0-9A-Fa-f]{6})\s*;", _read(TOKENS_CSS)
        )
    }


def main() -> int:
    failures: list[str] = []
    try:
        floor = check_ruler(_read(OKLCH_SOURCE))
        tokens = load_tokens()
        css = load_css()
    except GuardError as exc:
        print(f"check-hue-separation: FAIL — {exc}", file=sys.stderr)
        return 1

    measured = 0
    resolved: dict[str, dict[str, str]] = {}
    for scheme, roles in SCHEMES.items():
        values: dict[str, str] = {}
        resolved[scheme] = values
        for role, token_name in roles.items():
            entry = tokens.get(token_name)
            if entry is None:
                failures.append(
                    f"{scheme}: --{role} names token `{token_name}`, which "
                    f"{TOKENS_JSON.relative_to(REPO)} does not declare."
                )
                continue
            if scheme not in entry.get("surfaces", ()):
                failures.append(
                    f"{scheme}: token `{token_name}` no longer declares the "
                    f"`{scheme}` surface, so this row measures a colour the "
                    "scheme does not draw."
                )
                continue
            hexstr = entry["hex"].upper()
            declared = css.get(entry.get("css", ""))
            if declared is not None and declared != hexstr:
                failures.append(
                    f"{scheme}: {entry['css']} is {declared} in "
                    f"{TOKENS_CSS.relative_to(REPO)} but {hexstr} in the JSON — "
                    "regenerate with `make tokens-update`."
                )
                continue
            values[role] = hexstr

        for a, b in combinations(sorted(values), 2):
            measured += 1
            sep = separation(values[a], values[b])
            if sep < floor:
                failures.append(
                    f"{scheme}: --{a} ({values[a]}) is {sep:.1f}° from --{b} "
                    f"({values[b]}) in OKLCH; {floor:.0f}° is the floor for two "
                    "hues to be told apart. Move one of them — the floor is the "
                    "law, not the knob."
                )

    for scheme, role_a, role_b, stated, source in CLAIMS:
        values = resolved.get(scheme, {})
        if role_a not in values or role_b not in values:
            continue  # already reported above; do not report it twice
        computed = f"{separation(values[role_a], values[role_b]):.1f}°"
        where = source.relative_to(REPO)
        if computed != stated:
            failures.append(
                f"{where} states --{role_a} is {stated} from --{role_b} on the "
                f"{scheme} scheme; it is {computed}. Either the palette moved and "
                "the prose did not, or this table did not follow the prose."
            )
            continue
        try:
            if stated not in _read(source):
                failures.append(
                    f"{where} no longer states {stated} anywhere, but this guard "
                    f"holds it to that figure for --{role_a} vs --{role_b} on the "
                    f"{scheme} scheme. Restore the figure or drop the claim here."
                )
        except GuardError as exc:
            failures.append(str(exc))

    if failures:
        for line in failures:
            print(f"check-hue-separation: FAIL — {line}", file=sys.stderr)
        print(
            "check-hue-separation: the law is stated in "
            f"{OKLCH_SOURCE.relative_to(REPO)} and enforced on the terminal "
            "palette there; this is the same law on the web tokens.",
            file=sys.stderr,
        )
        return 1

    print(
        f"check-hue-separation: OK — {measured} role pairs across "
        f"{len(SCHEMES)} web schemes, all at or above {floor:.0f}° OKLCH; "
        f"{len(CLAIMS)} stated figures agree with the computation."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
