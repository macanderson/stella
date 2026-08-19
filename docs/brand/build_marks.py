#!/usr/bin/env python3
"""Export the raster forms of the stella marks — logo PNGs, PWA icons, favicon.

Two jobs, one tool, because they are the same job: every file written here is a
pixel form of artwork whose source is `logo/svg/`. Nothing in `logo/png/` or
`pwa/` may be edited by hand, and until this script existed nothing said so —
the exports had no builder, so a regeneration reached for whatever renderer was
nearby. Commit 10781aa31 did exactly that and committed 52 torn PNGs.

    python3 docs/brand/build_marks.py            # rewrite every export
    python3 docs/brand/build_marks.py --check    # validate, write nothing

`--check` deliberately does not compare bytes. librsvg's output shifts between
releases, so a byte comparison would fail the build on a Homebrew upgrade while
the art was still correct — an alarm that cries wolf gets silenced, and a
silenced guard is how the corruption travelled. What it asserts instead is what
a broken render actually violates: that every expected file exists, decodes,
inflates, and has the dimensions its name promises, and that the geometry in
`cometkit` still matches the committed SVGs.

Requires rsvg-convert (`brew install librsvg`). No imaging library: a PNG needs
zlib and four chunk headers, and an ICO is a six-byte header over PNGs.
"""

from __future__ import annotations

import argparse
import re
import struct
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import cometkit as ck  # noqa: E402  (path shim must precede the import)

SVG_DIR = HERE / "logo" / "svg"
PNG_DIR = HERE / "logo" / "png"
PWA_DIR = HERE / "pwa"


# ---------------------------------------------------------------------------
# logo exports
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Export:
    """One SVG rendered at one width. Height follows the artwork, never a guess."""

    stem: str
    width: int

    @property
    def src(self) -> Path:
        return SVG_DIR / f"{self.stem}.svg"

    @property
    def out(self) -> Path:
        return PNG_DIR / f"{self.stem}-{self.width}w.png"


# The `-adaptive` SVGs are deliberately absent: they switch on a media query,
# and a PNG cannot. Exporting one would freeze it to whichever branch librsvg
# happened to take, which is a mark that is wrong on half of all surfaces.
LOGO_EXPORTS: list[Export] = [
    *(
        Export(f"{form}-{ink}-{ground}", w)
        for form in ("lockup", "wordmark")
        for ink in ("color", "mono")
        for ground in ("dark", "light")
        for w in (1024, 2048)
    ),
    *(
        Export(stem, w)
        for stem in ("logomark-color", "logomark-mono-dark", "logomark-mono-light")
        for w in (256, 1024)
    ),
]


# ---------------------------------------------------------------------------
# app icons
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Icon:
    """One square app icon: a comet on Ink, sized by how much of it survives.

    `ink_frac` is the mark's *inked* width as a fraction of the canvas, not the
    artboard's — see `cometkit.MARK_INK`. Centring on the artboard leaves the
    comet visibly left of centre, which a circular crop makes obvious.
    """

    name: str
    size: int
    ink_frac: float
    star_only: bool = False


# Below 48px the trail cannot survive. Its bars are 7 units of a 44-unit mark,
# so at 16px they land under one pixel and render as three grey smudges beside
# the star — the mark reads as damaged, which is precisely the impression this
# kit is being rebuilt to remove. The star alone is the half that keeps its
# shape, so the two smallest favicons carry it and the rest carry the comet.
# 48px is the first size where a bar is over a pixel tall, and the ICO's own
# largest entry, so it is where the full mark comes back.
SMALL_FAVICON_CUTOFF = 48

ICONS: list[Icon] = [
    Icon("favicon-16", 16, 0.74, star_only=True),
    Icon("favicon-32", 32, 0.72, star_only=True),
    Icon("favicon-48", 48, 0.66),
    Icon("apple-touch-icon", 180, 0.62),
    Icon("icon-192", 192, 0.62),
    Icon("icon-512", 512, 0.62),
    # Maskable icons are cropped by the platform to an arbitrary shape; only a
    # centred circle of 80% diameter is guaranteed to survive. The comet's
    # diagonal is 1.14x its width, so 0.50 keeps every arm inside that circle
    # with room to spare — Android's roundest mask still shows the whole mark.
    Icon("icon-maskable-192", 192, 0.50),
    Icon("icon-maskable-512", 512, 0.50),
]

# The favicon.ico carries these three, largest last (the order Explorer wants).
ICO_SIZES = (16, 32, 48)


def icon_markup(icon: Icon) -> str:
    """A square of Ink with the mark centred on it."""
    c = icon.size / 2
    if icon.star_only:
        art = ck.star(c, c, icon.size * icon.ink_frac, ck.BRAND)
    else:
        art = ck.mark(c, c, icon.size * icon.ink_frac)
    ground = f'<rect width="{icon.size}" height="{icon.size}" fill="{ck.INK}"/>'
    return ck.svg_doc(icon.size, icon.size, ground + art)


def write_ico(out: Path, pngs: dict[int, bytes]) -> None:
    """Pack PNG-encoded entries into an ICO.

    PNG-in-ICO has been read by every browser since IE11 and keeps the file to
    a few KB where BMP entries would need an AND mask per size. The header is
    6 bytes, then one 16-byte directory entry each, then the payloads — a size
    of 256 is written as 0, which is why nothing here may exceed 255.
    """
    sizes = sorted(pngs)
    if any(s > 255 for s in sizes):
        raise SystemExit("an ICO entry cannot exceed 255px; use a PNG icon instead")
    header = struct.pack("<HHH", 0, 1, len(sizes))
    offset = 6 + 16 * len(sizes)
    entries, payloads = b"", b""
    for s in sizes:
        data = pngs[s]
        entries += struct.pack(
            "<BBBBHHII", s, s, 0, 0, 1, 32, len(data), offset + len(payloads)
        )
        payloads += data
    out.write_bytes(header + entries + payloads)


# ---------------------------------------------------------------------------
# guards
# ---------------------------------------------------------------------------

# A 7-unit round-capped stroke from x1 to x2 at y is a rect from x1-3.5 to
# x2+3.5, 7 tall. `cometkit.TRAIL_RECTS` is that arithmetic already applied, so
# re-deriving it from the SVG is what proves the two have not drifted.
_LINE = re.compile(
    r'<line x1="([\d.]+)" y1="([\d.]+)" x2="([\d.]+)" y2="([\d.]+)" '
    r'stroke="(#[0-9A-Fa-f]{6})" stroke-width="([\d.]+)"'
)
_STAR = re.compile(r'<path d="(M64 26[^"]*)" fill="(#[0-9A-Fa-f]{6})"')


def check_light_cut_colour() -> None:
    """The light cuts paint the mark in `BRAND_ON_LIGHT`, not full `BRAND`.

    v3.0 split the mark's colour by ground and v4.0 kept the split: the brand
    measures 2.63:1 on paper, below the 3:1 non-text floor, so the light cuts
    step down the ramp
    while the dark and adaptive ones keep full strength. That is a *rule*, and
    a rule the kit only writes down is a rule the next recolour silently
    breaks — the bronze era's own drift (a bronze wordmark shipped beside
    phosphor chrome for the life of a rebrand) is what this file exists to
    stop happening again. So it is checked rather than documented.

    The adaptive cuts are deliberately not checked here: they carry BOTH
    values, the dark one as the fill and the light one behind a
    `prefers-color-scheme` override, so a fill-colour assertion is the wrong
    shape for them.
    """
    for name in sorted(p.name for p in SVG_DIR.glob("*-color-light.svg")):
        src = (SVG_DIR / name).read_text(encoding="utf-8")
        for found in set(re.findall(r'(?:fill|stroke)="(#[0-9A-Fa-f]{6})"', src)):
            if found.upper() == ck.BRAND:
                raise SystemExit(
                    f"{name} paints the mark in full BRAND ({ck.BRAND}), which "
                    f"measures 2.63:1 on paper. Light cuts take BRAND_ON_LIGHT "
                    f"({ck.BRAND_ON_LIGHT})."
                )
        if ck.BRAND_ON_LIGHT.upper() not in src.upper():
            raise SystemExit(
                f"{name} never paints {ck.BRAND_ON_LIGHT} — a colour cut with no "
                f"brand colour in it is a mono cut wearing the wrong name."
            )


def check_svg_parity() -> None:
    """`cometkit`'s geometry must still be the committed artwork's geometry.

    The kit has been recoloured three times, and every time the SVGs and the
    Python copies had to move together. This is the test that they did.
    """
    check_light_cut_colour()
    src = (SVG_DIR / "logomark-color.svg").read_text(encoding="utf-8")

    star = _STAR.search(src)
    if star is None:
        raise SystemExit("logomark-color.svg: no star path found — has it been redrawn?")
    if star.group(1) != ck.STAR_PATH:
        raise SystemExit(
            "STAR_PATH drift: cometkit.py and logo/svg/logomark-color.svg disagree.\n"
            f"  svg:      {star.group(1)}\n  cometkit: {ck.STAR_PATH}"
        )
    if star.group(2).upper() != ck.BRAND:
        raise SystemExit(f"star fill is {star.group(2)}, cometkit says {ck.BRAND}")

    derived = []
    for x1, y1, x2, _y2, stroke, sw in _LINE.findall(src):
        if stroke.upper() != ck.BRAND:
            raise SystemExit(f"trail stroke is {stroke}, cometkit says {ck.BRAND}")
        x1, y1, x2, sw = float(x1), float(y1), float(x2), float(sw)
        derived.append((x1 - sw / 2, y1 - sw / 2, (x2 - x1) + sw, sw))
    if derived != [tuple(map(float, r)) for r in ck.TRAIL_RECTS]:
        raise SystemExit(
            "TRAIL_RECTS drift: cometkit.py and logomark-color.svg disagree.\n"
            f"  from svg: {derived}\n  cometkit: {ck.TRAIL_RECTS}"
        )


def check_sources() -> None:
    for exp in sorted({e.src for e in LOGO_EXPORTS}):
        if not exp.exists():
            raise SystemExit(f"missing source artwork: {exp}")


def check_outputs() -> int:
    """Every expected file exists, decodes, and is the size its name promises."""
    bad = 0
    for exp in LOGO_EXPORTS:
        if not exp.out.exists():
            print(f"missing: {exp.out.relative_to(HERE)}")
            bad += 1
            continue
        ck.verify_png(exp.out)
        w, _h = ck.png_size(exp.out)
        if w != exp.width:
            print(f"wrong width: {exp.out.relative_to(HERE)} is {w}, want {exp.width}")
            bad += 1

    for icon in ICONS:
        out = PWA_DIR / f"{icon.name}.png"
        if not out.exists():
            print(f"missing: {out.relative_to(HERE)}")
            bad += 1
            continue
        ck.verify_png(out)
        if ck.png_size(out) != (icon.size, icon.size):
            print(f"wrong size: {out.relative_to(HERE)} is {ck.png_size(out)}")
            bad += 1

    ico = PWA_DIR / "favicon.ico"
    if not ico.exists():
        print("missing: pwa/favicon.ico")
        bad += 1
    elif struct.unpack("<HHH", ico.read_bytes()[:6])[2] != len(ICO_SIZES):
        print(f"pwa/favicon.ico does not carry {len(ICO_SIZES)} entries")
        bad += 1
    return bad


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------


def build() -> None:
    for exp in LOGO_EXPORTS:
        ck.rasterize_file(exp.src, exp.width, exp.out)
        print(f"  {exp.out.relative_to(HERE)}")

    ico_payloads: dict[int, bytes] = {}
    for icon in ICONS:
        out = PWA_DIR / f"{icon.name}.png"
        ck.rasterize(icon_markup(icon), icon.size, icon.size, out)
        print(f"  {out.relative_to(HERE)}")

    # The ICO reuses the favicon PNGs already on disk rather than re-rendering,
    # so the .ico and the .png of a given size can never disagree.
    for size in ICO_SIZES:
        ico_payloads[size] = (PWA_DIR / f"favicon-{size}.png").read_bytes()
    write_ico(PWA_DIR / "favicon.ico", ico_payloads)
    print("  pwa/favicon.ico")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--check", action="store_true", help="validate only, write nothing")
    args = ap.parse_args()

    check_svg_parity()
    check_sources()

    if args.check:
        bad = check_outputs()
        if bad:
            print(f"\n{bad} export(s) wrong — run `python3 docs/brand/build_marks.py`")
            return 1
        print(f"ok: {len(LOGO_EXPORTS)} logo exports, {len(ICONS)} icons, 1 ico")
        return 0

    ck.require_renderer()
    build()
    if check_outputs():
        raise SystemExit("build wrote something the check rejects")
    print(f"\n{len(LOGO_EXPORTS)} logo exports, {len(ICONS)} icons, 1 ico")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
