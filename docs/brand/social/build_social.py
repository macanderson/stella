#!/usr/bin/env python3
"""Draw the stella social art — the source the PNGs in this folder never had.

Every banner is one composition at four aspect ratios: the comet lockup, the
tagline, a terminal showing the Homebrew install line, and an action row that
names the repo and asks for a star. The terminal is a picture of a terminal —
nothing here copies to a clipboard — so the install line has to be *correct*
rather than convenient: it is read off README.md, and `check_install_line`
fails the build if the two ever drift apart.

Layout is derived, not measured by eye. JetBrains Mono advances 0.6em per
glyph, so every string's width is known before it is drawn; the terminal box
is sized *from* its command rather than the command being trusted to fit. That
is the whole reason the install line cannot clip at any aspect ratio.

Backgrounds are the kit's one shape repeated: the four-point comet star, blown
up past the canvas at low opacity for the corner sweeps and shrunk to specks
for the starfield. Same path, three scales. The starfield is seeded, so a
re-run reproduces the committed PNGs byte for byte.

Usage:
    python3 docs/brand/social/build_social.py           # write PNGs
    python3 docs/brand/social/build_social.py --check   # verify, write nothing

Requires rsvg-convert (brew install librsvg) and JetBrains Mono installed as a
system font (the kit ships the woff2s in ../fonts for the web; librsvg reads
fontconfig, so the desktop install is what it picks up).
"""

from __future__ import annotations

import argparse
import base64
import random
import re
import shutil
import struct
import subprocess
import sys
import zlib
from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]

# ---------------------------------------------------------------------------
# brand tokens — docs/brand/css/tokens.css is normative; these mirror it
# ---------------------------------------------------------------------------

GOLD = "#FFB000"  # Phosphor Gold — the comet, on every ground
GOLD_DEEP = "#A37200"  # small gold *text* on light surfaces only
INK = "#0B0B0C"
PAPER = "#F4F1EA"  # warm text on dark
PAPER_BG = "#F6F2E9"  # light-mode surface
MUTED_ON_DARK = "#9B9890"
MUTED_ON_LIGHT = "#6E6A5F"

# The repo this art advertises, and the one command that installs it.
REPO_SLUG = "macanderson/stella"
INSTALL_CMD = "brew install macanderson/tap/stella"
TAGLINE = "the terminal agent — faster · cheaper · more accurate"
STAR_CTA = "star the repo"

# JetBrains Mono is monospace: one advance, every glyph, forever.
ADVANCE = 0.6

# ---------------------------------------------------------------------------
# geometry — copied from docs/brand/logo/svg/ and website/src/components/brand.tsx
# ---------------------------------------------------------------------------

# The comet star in its own 96-unit box. Its ink spans (42,26)–(86,70): a
# 44-unit square centred on (64,48). Every star on every canvas is this path.
STAR_PATH = (
    "M64 26 C65.65 39.2 72.8 46.35 86 48 C72.8 49.65 65.65 56.8 64 70 "
    "C62.35 56.8 55.2 49.65 42 48 C55.2 46.35 62.35 39.2 64 26 Z"
)
STAR_BOX = 44.0
STAR_CX, STAR_CY = 64.0, 48.0

# The trail: three round-capped strokes, flattened to rects so the same numbers
# serve renderers with no stroke support. A 7-unit round-capped stroke x1→x2 is
# a rect from x1−3.5 to x2+3.5, 7 tall, rx 3.5.
TRAIL_RECTS = [
    (6.5, 44.5, 27.0, 7.0),
    (14.5, 30.5, 19.0, 7.0),
    (14.5, 58.5, 19.0, 7.0),
]

# "stella" outlined — no font needed, so the name never falls back to a
# substitute face. docs/brand/logo/svg/lockup-color-dark.svg, letters only.
WORDMARK_PATH = (
    "M122.62 70.5Q118.54 70.5 115.48 69.21Q112.42 67.92 110.71 65.64Q109.0 63.36 109.0 60.3H118.0"
    "Q118.0 61.74 119.29 62.61Q120.58 63.48 122.62 63.48H125.26Q127.72 63.48 129.01 62.58"
    "Q130.3 61.68 130.3 60.06Q130.3 58.56 129.16 57.75Q128.02 56.94 125.62 56.64L121.78 56.16"
    "Q115.6 55.38 112.78 53.16Q109.96 50.94 109.96 46.38Q109.96 41.58 113.26 38.94"
    "Q116.56 36.3 122.92 36.3H125.2Q131.26 36.3 134.86 38.94Q138.46 41.58 138.46 46.02H129.46"
    "Q129.46 44.82 128.29 44.07Q127.12 43.32 125.2 43.32H122.92Q120.7 43.32 119.68 44.07"
    "Q118.66 44.82 118.66 46.32Q118.66 47.7 119.59 48.42Q120.52 49.14 122.56 49.44L126.7 49.98"
    "Q132.94 50.76 135.97 53.1Q139.0 55.44 139.0 60.06Q139.0 65.1 135.52 67.8Q132.04 70.5 125.26 70.5Z "
    "M163.6 69.9Q158.56 69.9 155.68 67.02Q152.8 64.14 152.8 59.1V45.0H144.1V36.9H152.8V27.6H161.8V36.9"
    "H174.1V45.0H161.8V59.1Q161.8 61.8 164.5 61.8H173.5V69.9Z "
    "M196.06 70.5Q191.68 70.5 188.41 68.85Q185.14 67.2 183.37 64.23Q181.6 61.26 181.6 57.3V49.5"
    "Q181.6 45.54 183.37 42.57Q185.14 39.6 188.41 37.95Q191.68 36.3 196.06 36.3Q200.44 36.3 203.65 37.95"
    "Q206.86 39.6 208.63 42.57Q210.4 45.54 210.4 49.5V55.5H190.18V57.3Q190.18 60.42 191.68 62.01"
    "Q193.18 63.6 196.18 63.6Q198.28 63.6 199.57 62.88Q200.86 62.16 201.28 60.9H210.1"
    "Q209.02 65.22 205.21 67.86Q201.4 70.5 196.06 70.5ZM201.82 50.88V49.38Q201.82 46.32 200.41 44.7"
    "Q199.0 43.08 196.06 43.08Q193.12 43.08 191.65 44.76Q190.18 46.44 190.18 49.5V50.4L202.42 50.28Z "
    "M237.4 69.9Q233.92 69.9 231.28 68.43Q228.64 66.96 227.17 64.32Q225.7 61.68 225.7 58.2V34.2H215.5"
    "V26.1H234.7V58.2Q234.7 59.82 235.69 60.81Q236.68 61.8 238.3 61.8H247.9V69.9Z "
    "M273.4 69.9Q269.92 69.9 267.28 68.43Q264.64 66.96 263.17 64.32Q261.7 61.68 261.7 58.2V34.2H251.5"
    "V26.1H270.7V58.2Q270.7 59.82 271.69 60.81Q272.68 61.8 274.3 61.8H283.9V69.9Z "
    "M299.74 70.5Q294.76 70.5 291.85 67.8Q288.94 65.1 288.94 60.48Q288.94 55.5 292.39 52.83"
    "Q295.84 50.16 302.32 50.16H309.16V47.7Q309.16 45.78 307.81 44.64Q306.46 43.5 304.18 43.5"
    "Q302.08 43.5 300.7 44.46Q299.32 45.42 299.02 47.1H290.32Q290.86 42.12 294.67 39.21"
    "Q298.48 36.3 304.48 36.3Q310.78 36.3 314.47 39.39Q318.16 42.48 318.16 47.7V69.9H309.46V64.5H308.02"
    "L309.52 62.4Q309.52 66.12 306.85 68.31Q304.18 70.5 299.74 70.5ZM303.1 63.9Q305.74 63.9 307.45 62.49"
    "Q309.16 61.08 309.16 58.8V55.26H302.5Q300.46 55.26 299.2 56.43Q297.94 57.6 297.94 59.52"
    "Q297.94 61.56 299.32 62.73Q300.7 63.9 303.1 63.9Z"
)
LOCKUP_W, LOCKUP_H = 326.0, 96.0

# The GitHub mark, same path the site's nav and doc footers use.
GITHUB_PATH = (
    "M12 .297c-6.63 0-12 5.373-12 12 0 5.303 3.438 9.8 8.205 11.385.6.113.82-.258.82-.577 "
    "0-.285-.01-1.04-.015-2.04-3.338.724-4.042-1.61-4.042-1.61C4.422 18.07 3.633 17.7 3.633 17.7"
    "c-1.087-.744.084-.729.084-.729 1.205.084 1.838 1.236 1.838 1.236 1.07 1.835 2.809 1.305 3.495.998"
    ".108-.776.417-1.305.76-1.605-2.665-.3-5.466-1.332-5.466-5.93 0-1.31.465-2.38 1.235-3.22"
    "-.135-.303-.54-1.523.105-3.176 0 0 1.005-.322 3.3 1.23.96-.267 1.98-.399 3-.405 1.02.006 2.04.138 3 .405"
    " 2.28-1.552 3.285-1.23 3.285-1.23.645 1.653.24 2.873.12 3.176.765.84 1.23 1.91 1.23 3.22 0 4.61-2.805 "
    "5.625-5.475 5.92.42.36.81 1.096.81 2.22 0 1.606-.015 2.896-.015 3.286 0 .315.21.69.825.57"
    "C20.565 22.092 24 17.592 24 12.297c0-6.627-5.373-12-12-12"
)
GITHUB_BOX = 24.0


# ---------------------------------------------------------------------------
# theme
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Theme:
    """One ground and everything that has to change with it.

    Note what does *not* change: the comet stays #FFB000 on both grounds. The
    kit's rule is that the mark is a shape, not small text, so it never drops
    to gold-deep — only gold *lettering* does, and only on light.
    """

    name: str
    bg: str
    fg: str
    muted: str
    gold_text: str  # gold that passes contrast as small text on this ground
    surface: str  # terminal body
    surface_top: str  # terminal title bar
    border: str
    grid: str
    grid_op: float
    sweep_dark: str  # the corner sweep that sits back
    sweep_dark_op: float
    sweep_warm_op: float
    glow_op: float
    band: str

    @property
    def is_dark(self) -> bool:
        return self.name == "dark"


DARK = Theme(
    name="dark",
    bg=INK,
    fg=PAPER,
    muted=MUTED_ON_DARK,
    gold_text=GOLD,
    surface="#121214",
    surface_top="#17171A",
    border="#2B2B2E",
    grid="#FFFFFF",
    grid_op=0.045,
    sweep_dark="#000000",
    sweep_dark_op=0.38,
    sweep_warm_op=0.055,
    glow_op=0.10,
    band=INK,
)

LIGHT = Theme(
    name="light",
    bg=PAPER_BG,
    fg=INK,
    muted=MUTED_ON_LIGHT,
    gold_text=GOLD_DEEP,
    surface="#FFFDF7",
    surface_top="#F1ECDF",
    border="#DCD6C7",
    grid="#0B0B0C",
    grid_op=0.05,
    sweep_dark="#C9BFA6",
    sweep_dark_op=0.22,
    sweep_warm_op=0.10,
    glow_op=0.10,
    band="#F1ECE1",
)


# ---------------------------------------------------------------------------
# layouts — one entry per surface stella is advertised on
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Layout:
    name: str
    w: int
    h: int
    lockup_h: float
    tagline_fs: float
    term_fs: float
    action_fs: float
    gaps: tuple[float, float, float]  # after lockup / tagline / terminal
    band_frac: float
    # Centred region content must stay inside (YouTube crops hard to this).
    safe: tuple[int, int] | None = None
    mark_only: bool = False
    # Nudge the stack up so a platform's own overlay (avatar, logo) misses it.
    lift: float = 0.0


LAYOUTS = [
    # Link previews. Seen small in a timeline, so type runs large for the box.
    Layout("og-image", 1200, 630, 112, 21, 26, 23, (30, 42, 38), 0.135),
    # X: the avatar punches into the bottom-left, so the stack lifts clear.
    Layout("x-banner", 1500, 500, 92, 18, 23, 20, (24, 32, 30), 0.16, lift=14),
    # LinkedIn: shortest canvas in the set — everything tightens.
    Layout("linkedin-banner", 1584, 396, 64, 15, 19, 17, (18, 24, 22), 0.14, lift=6),
    # YouTube: 2560×1440 renders down to a 1546×423 safe box on TV and mobile.
    Layout(
        "youtube-banner",
        2560,
        1440,
        100,
        21,
        27,
        24,
        (28, 38, 34),
        0.06,
        safe=(1546, 423),
    ),
    # The avatar carries the mark alone, so `lockup_h` is read as the comet's
    # ink width — see `mark_only` in `compose` for why it has no words.
    Layout("avatar", 1024, 1024, 530, 0, 0, 0, (0, 0, 0), 0.0, mark_only=True),
]


# ---------------------------------------------------------------------------
# primitives
# ---------------------------------------------------------------------------


def tw(text: str, fs: float) -> float:
    """Width of `text` at `fs`. Exact for mono, which is the point."""
    return len(text) * fs * ADVANCE


def esc(text: str) -> str:
    return text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


@lru_cache(maxsize=1)
def noise_tile(size: int = 128) -> str:
    """A grey-noise PNG as a data URI, encoded here rather than filtered.

    SVG's own `feTurbulence` does this too, but librsvg evaluates it per pixel:
    it costs two minutes across this set, almost all of it on the 2560×1440
    YouTube canvas. A tile encoded once and repeated is visually the same
    dither for a few milliseconds, so the noise is built by hand — a greyscale
    PNG needs only zlib and four chunk headers, no imaging library.
    """
    rng = random.Random("stella/grain")
    raw = bytearray()
    for _ in range(size):
        raw.append(0)  # PNG per-scanline filter: none
        raw.extend(rng.randrange(256) for _ in range(size))

    def chunk(tag: bytes, data: bytes) -> bytes:
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 0, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )
    return "data:image/png;base64," + base64.b64encode(png).decode("ascii")


def grain_defs(size: int = 128) -> str:
    return (
        f'<pattern id="grain" width="{size}" height="{size}" '
        f'patternUnits="userSpaceOnUse">'
        f'<image href="{noise_tile(size)}" xlink:href="{noise_tile(size)}" '
        f'width="{size}" height="{size}"/></pattern>'
    )


# Kept low on purpose. The dither only has to break a one-level colour step, so
# a few levels of jitter is plenty; any more and the mean of the noise starts
# lifting Ink off its own value.
GRAIN_OP = 0.03


def grain_rect(lo: Layout) -> str:
    return (
        f'<rect width="{lo.w}" height="{lo.h}" fill="url(#grain)" '
        f'opacity="{GRAIN_OP:g}"/>'
    )


def label(x: float, y: float, text: str, fs: float, fill: str, **kw) -> str:
    """A run of text at its own baseline, anchored however the caller asks."""
    anchor = kw.get("anchor", "start")
    weight = kw.get("weight", 400)
    op = kw.get("opacity", 1.0)
    return (
        f'<text x="{x:.2f}" y="{y:.2f}" font-family="JetBrains Mono, monospace" '
        f'font-size="{fs:.2f}" font-weight="{weight}" fill="{fill}" '
        f'opacity="{op:g}" text-anchor="{anchor}" '
        f'xml:space="preserve">{esc(text)}</text>'
    )


def star(cx: float, cy: float, size: float, fill: str, op: float = 1.0) -> str:
    """The comet star, centred on (cx, cy) and `size` across."""
    k = size / STAR_BOX
    tx = cx - STAR_CX * k
    ty = cy - STAR_CY * k
    return (
        f'<g transform="translate({tx:.3f} {ty:.3f}) scale({k:.5f})" '
        f'opacity="{op:g}"><path d="{STAR_PATH}" fill="{fill}"/></g>'
    )


# Where a star's outline sits at 45°, as a fraction of its size: the arm cubic
# evaluated at its midpoint lands 9.43 units out in the 44-unit box. Anchoring
# on this lets a sweep be placed by its *curve* rather than by its centre —
# which is what keeps the four arm tips off-canvas at every aspect ratio.
WAIST = 9.43 / STAR_BOX


def sweep(lo: Layout, waist: tuple[float, float], scale: float, side: int, fill: str, op: float) -> str:
    """A corner sweep: one star so large only its concave waist is in frame.

    `waist` is where the curve should cross, in canvas fractions; `side` is -1
    to hang the star off the top-left corner and +1 for the bottom-right. Size
    it too small and an arm tip drifts into the canvas as a spike — the reason
    this is solved rather than eyeballed.
    """
    size = max(lo.w, lo.h) * scale
    off = WAIST * size * 0.7071
    cx = waist[0] * lo.w + side * off
    cy = waist[1] * lo.h + side * off
    return star(cx, cy, size, fill, op)


def lockup(cx: float, cy: float, height: float, letters: str) -> str:
    """Comet + trail + "stella", centred on (cx, cy) at `height` tall."""
    k = height / LOCKUP_H
    tx = cx - (LOCKUP_W * k) / 2
    ty = cy - height / 2
    trails = "".join(
        f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="3.5" fill="{GOLD}"/>'
        for x, y, w, h in TRAIL_RECTS
    )
    return (
        f'<g transform="translate({tx:.3f} {ty:.3f}) scale({k:.5f})">'
        f"{trails}"
        f'<path d="{STAR_PATH}" fill="{GOLD}"/>'
        f'<path d="{WORDMARK_PATH}" fill="{letters}"/>'
        f"</g>"
    )


# The comet's *inked* extent inside its 96-unit box: the trail starts at x=6.5
# and the star ends at x=86, top and bottom at y=26 and y=70. Centring on the
# box instead of on this leaves the mark visibly off-centre, which an avatar —
# cropped to a circle by every platform that shows one — makes obvious.
MARK_INK = (6.5, 26.0, 79.5, 44.0)


def mark(cx: float, cy: float, ink_w: float) -> str:
    """The comet alone, centred on its ink and `ink_w` across."""
    ix, iy, iw, ih = MARK_INK
    k = ink_w / iw
    tx = cx - (ix + iw / 2) * k
    ty = cy - (iy + ih / 2) * k
    trails = "".join(
        f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="3.5" fill="{GOLD}"/>'
        for x, y, w, h in TRAIL_RECTS
    )
    return (
        f'<g transform="translate({tx:.3f} {ty:.3f}) scale({k:.5f})">'
        f'{trails}<path d="{STAR_PATH}" fill="{GOLD}"/></g>'
    )


def github_mark(cx: float, cy: float, size: float, fill: str, op: float = 1.0) -> str:
    k = size / GITHUB_BOX
    tx = cx - (GITHUB_BOX * k) / 2
    ty = cy - (GITHUB_BOX * k) / 2
    return (
        f'<g transform="translate({tx:.3f} {ty:.3f}) scale({k:.5f})" '
        f'opacity="{op:g}"><path d="{GITHUB_PATH}" fill="{fill}"/></g>'
    )


# ---------------------------------------------------------------------------
# background
# ---------------------------------------------------------------------------


def background(lo: Layout, th: Theme, keep_out: tuple[float, float, float, float]) -> str:
    """Ground, glow, grid, two corner sweeps, and a seeded starfield.

    `keep_out` is the content's bounding box: no speck is drawn inside it, so
    the install line never has a star sitting in a descender.
    """
    w, h = lo.w, lo.h
    out = [f'<rect width="{w}" height="{h}" fill="{th.bg}"/>']

    # Warm glow behind the lockup — the comet lighting the ground it flies over.
    gx, gy = w / 2, keep_out[1] + lo.lockup_h * 0.5
    out.append(
        f'<ellipse cx="{gx:.1f}" cy="{gy:.1f}" rx="{w * 0.42:.1f}" '
        f'ry="{h * 0.46:.1f}" fill="url(#glow)"/>'
    )

    # Grid: eight columns of it, squared off so rows match columns.
    step = w / 8.0
    lines = []
    x = step
    while x < w:
        lines.append(f'<line x1="{x:.1f}" y1="0" x2="{x:.1f}" y2="{h}"/>')
        x += step
    y = step
    while y < h:
        lines.append(f'<line x1="0" y1="{y:.1f}" x2="{w}" y2="{y:.1f}"/>')
        y += step
    out.append(
        f'<g stroke="{th.grid}" stroke-width="1" opacity="{th.grid_op:g}">'
        f'{"".join(lines)}</g>'
    )

    # Two sweeps: the same star again, past canvas scale, one receding and one
    # warm, hung off opposite corners so the eye reads a diagonal not a pair.
    out.append(sweep(lo, (0.03, 0.36), 2.00, -1, th.sweep_dark, th.sweep_dark_op))
    out.append(sweep(lo, (0.97, 0.64), 1.90, +1, GOLD, th.sweep_warm_op))

    out.append(starfield(lo, th, keep_out))

    # The floor: a flat band under the texture, where profile chrome overlaps.
    if lo.band_frac > 0:
        band_h = h * lo.band_frac
        out.append(
            f'<rect x="0" y="{h - band_h:.1f}" width="{w}" height="{band_h:.1f}" '
            f'fill="{th.band}"/>'
        )

    # Grain. A glow this wide steps through 8-bit colour slowly enough that the
    # steps become visible rings — the banner picks them up as contour lines
    # around the lockup. A few percent of noise dithers the steps away, and it
    # is the texture the kit's tokens already call for.
    out.append(grain_rect(lo))
    return "".join(out)


def starfield(lo: Layout, th: Theme, keep_out: tuple[float, float, float, float]) -> str:
    """Specks and sparkles. Seeded per canvas, so re-runs are byte-identical."""
    rng = random.Random(f"stella/{lo.name}/{th.name}/v2")
    kx, ky, kw, kh = keep_out
    pad = lo.w * 0.02
    # Size specks off the canvas's geometric mean, not its width: a 2560-wide
    # YouTube banner and a 1584-wide LinkedIn strip then get stars that read
    # the same, instead of the wide one getting boulders.
    unit = (lo.w * lo.h) ** 0.5
    # Count grows with the square root of area, not area. Specks already scale
    # with `unit`, so counting by area would keep the *number* per pixel fixed
    # while each speck got bigger — which is how the 2560px YouTube canvas ends
    # up looking like static. Square root holds the density the eye reads.
    count = int(44 * ((lo.w * lo.h) / 756000.0) ** 0.5)
    out = []
    for _ in range(count):
        x = rng.uniform(0, lo.w)
        y = rng.uniform(0, lo.h)
        if kx - pad < x < kx + kw + pad and ky - pad < y < ky + kh + pad:
            continue
        warm = rng.random() < 0.42
        fill = GOLD if warm else (PAPER if th.is_dark else "#6E6A5F")
        # Gold reads quieter than paper on ink, so it carries the higher cap;
        # a paper speck at full strength competes with the wordmark.
        top = 0.42 if warm else 0.26
        if rng.random() < 0.42:
            out.append(
                star(x, y, rng.uniform(0.011, 0.026) * unit, fill, rng.uniform(0.08, top))
            )
        else:
            r = rng.uniform(0.0018, 0.0040) * unit
            out.append(
                f'<circle cx="{x:.1f}" cy="{y:.1f}" r="{r:.2f}" fill="{fill}" '
                f'opacity="{rng.uniform(0.08, top):.2f}"/>'
            )
    return "".join(out)


# ---------------------------------------------------------------------------
# the two new blocks: the terminal, and the ask
# ---------------------------------------------------------------------------


def terminal_width(fs: float) -> float:
    """Width the install line demands. Nothing chooses this but the string."""
    pad = fs * 1.5
    line = f"$ {INSTALL_CMD}"
    return pad * 2 + tw(line, fs) + tw(" ", fs) + fs * ADVANCE


def terminal_height(fs: float) -> float:
    return round(fs * 2.0) + round(fs * 2.5)


def terminal(x: float, y: float, fs: float, th: Theme) -> str:
    """A picture of a terminal: chrome, prompt, command, resting cursor.

    It cannot be copied from — it is a PNG on a profile page — so it is drawn
    to be *read* and retyped. That is why the command is the widest thing in
    the composition and why the box is sized from it.
    """
    w = terminal_width(fs)
    top_h = round(fs * 2.0)
    body_h = round(fs * 2.5)
    h = top_h + body_h
    r = fs * 0.55
    pad = fs * 1.5

    out = [
        f'<rect x="{x:.2f}" y="{y:.2f}" width="{w:.2f}" height="{h:.2f}" '
        f'rx="{r:.2f}" fill="{th.surface}" stroke="{th.border}" stroke-width="1.5"/>',
        # Title bar. Clipped to the top corners by redrawing the seam below.
        f'<path d="M{x:.2f} {y + top_h:.2f} V{y + r:.2f} '
        f"a{r:.2f} {r:.2f} 0 0 1 {r:.2f} -{r:.2f} "
        f"H{x + w - r:.2f} a{r:.2f} {r:.2f} 0 0 1 {r:.2f} {r:.2f} "
        f'V{y + top_h:.2f} Z" fill="{th.surface_top}"/>',
        f'<line x1="{x:.2f}" y1="{y + top_h:.2f}" x2="{x + w:.2f}" '
        f'y2="{y + top_h:.2f}" stroke="{th.border}" stroke-width="1.5"/>',
    ]

    # Three dots. One gold — gold is the signal, so exactly one thing gets it.
    dr = fs * 0.24
    dx = x + pad * 0.78
    dy = y + top_h / 2
    for i, (col, op) in enumerate(
        ((GOLD, 0.95), (th.muted, 0.45), (th.muted, 0.28))
    ):
        out.append(
            f'<circle cx="{dx + i * dr * 3.1:.2f}" cy="{dy:.2f}" r="{dr:.2f}" '
            f'fill="{col}" opacity="{op:g}"/>'
        )

    # The window's title is the repo it belongs to.
    out.append(
        label(
            x + w / 2,
            dy + fs * 0.24,
            REPO_SLUG,
            fs * 0.66,
            th.muted,
            anchor="middle",
            opacity=0.85,
        )
    )

    # The line itself: gold prompt, then the command in full contrast.
    base = y + top_h + body_h * 0.62
    out.append(label(x + pad, base, "$", fs, th.gold_text, weight=700))
    cmd_x = x + pad + tw("$ ", fs)
    out.append(label(cmd_x, base, INSTALL_CMD, fs, th.fg, weight=500))

    # A resting block cursor — the tell that says "terminal", not "code block".
    cur_x = cmd_x + tw(INSTALL_CMD + " ", fs)
    out.append(
        f'<rect x="{cur_x:.2f}" y="{base - fs * 0.72:.2f}" '
        f'width="{fs * ADVANCE:.2f}" height="{fs * 0.95:.2f}" '
        f'fill="{th.gold_text}" opacity="0.9"/>'
    )
    return "".join(out)


def action_row(x: float, y: float, w: float, fs: float, th: Theme) -> str:
    """Repo on the left, the ask on the right, spanning the terminal's width.

    Two jobs in one line: say *which* repo, and ask for the star. The slug is
    plain — it is a fact — and the ask is a pill, because it is the one thing
    on the canvas a reader is meant to act on.
    """
    h = round(fs * 2.4)
    cy = y + h / 2
    out = []

    # Left: the mark, then the slug, in the muted register facts live in.
    icon = fs * 1.12
    out.append(github_mark(x + icon / 2, cy, icon, th.fg, 0.88))
    out.append(
        label(
            x + icon + fs * 0.62,
            cy + fs * 0.36,
            REPO_SLUG,
            fs,
            th.fg,
            weight=500,
            opacity=0.92,
        )
    )

    # Right: the ask. Gold hairline and a gold wash — a button's posture,
    # without pretending to be clickable art.
    pad = fs * 0.95
    sstar = fs * 1.15
    pill_w = pad + sstar + fs * 0.6 + tw(STAR_CTA, fs) + pad
    px = x + w - pill_w
    out.append(
        f'<rect x="{px:.2f}" y="{y:.2f}" width="{pill_w:.2f}" height="{h:.2f}" '
        f'rx="{h / 2:.2f}" fill="{GOLD}" fill-opacity="{0.12 if th.is_dark else 0.16:g}" '
        f'stroke="{th.gold_text}" stroke-opacity="0.55" stroke-width="1.5"/>'
    )
    out.append(star(px + pad + sstar / 2, cy, sstar, GOLD))
    out.append(
        label(
            px + pad + sstar + fs * 0.6,
            cy + fs * 0.36,
            STAR_CTA,
            fs,
            th.gold_text,
            weight=700,
        )
    )
    return "".join(out)


# ---------------------------------------------------------------------------
# composition
# ---------------------------------------------------------------------------


def compose(lo: Layout, th: Theme) -> str:
    defs = (
        f'<defs><radialGradient id="glow"><stop offset="0" stop-color="{GOLD}" '
        f'stop-opacity="{th.glow_op:g}"/><stop offset="1" stop-color="{GOLD}" '
        f'stop-opacity="0"/></radialGradient>' + grain_defs() + "</defs>"
    )

    if lo.mark_only:
        # The avatar renders at ~48px in a timeline. A slug there is mud, so it
        # stays the mark alone — the one surface in the set that carries no
        # words. See README.md for why that is deliberate.
        body = [
            f'<rect width="{lo.w}" height="{lo.h}" fill="{th.bg}"/>',
            f'<ellipse cx="{lo.w / 2}" cy="{lo.h / 2}" rx="{lo.w * 0.46}" '
            f'ry="{lo.h * 0.46}" fill="url(#glow)"/>',
            grain_rect(lo),
            mark(lo.w / 2, lo.h / 2, lo.lockup_h),
        ]
        return svg(lo, defs + "".join(body))

    # Vertical stack, measured before it is placed.
    g0, g1, g2 = lo.gaps
    term_h = terminal_height(lo.term_fs)
    action_h = round(lo.action_fs * 2.4)
    stack_h = lo.lockup_h + g0 + lo.tagline_fs + g1 + term_h + g2 + action_h

    # Centre it in whatever the platform actually shows: the safe box if the
    # platform crops to one, otherwise the canvas above the bottom band.
    if lo.safe:
        area_y = (lo.h - lo.safe[1]) / 2
        area_h = lo.safe[1]
    else:
        area_y = 0.0
        area_h = lo.h * (1 - lo.band_frac)
    top = area_y + (area_h - stack_h) / 2 - lo.lift

    term_w = terminal_width(lo.term_fs)
    term_x = (lo.w - term_w) / 2

    y = top
    parts = [lockup(lo.w / 2, y + lo.lockup_h / 2, lo.lockup_h, th.fg)]
    y += lo.lockup_h + g0
    parts.append(
        label(lo.w / 2, y + lo.tagline_fs, TAGLINE, lo.tagline_fs, th.muted, anchor="middle")
    )
    y += lo.tagline_fs + g1
    parts.append(terminal(term_x, y, lo.term_fs, th))
    y += term_h + g2
    parts.append(action_row(term_x, y, term_w, lo.action_fs, th))

    keep_out = (term_x, top, term_w, stack_h)
    return svg(lo, defs + background(lo, th, keep_out) + "".join(parts))


def svg(lo: Layout, body: str) -> str:
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" '
        f'xmlns:xlink="http://www.w3.org/1999/xlink" '
        f'width="{lo.w}" height="{lo.h}" viewBox="0 0 {lo.w} {lo.h}">{body}</svg>'
    )


# ---------------------------------------------------------------------------
# guards
# ---------------------------------------------------------------------------


def check_install_line() -> None:
    """The banner's command must be the command README.md tells people to run.

    A banner is the one place a wrong install line is unfixable after the fact:
    it has already been rendered, uploaded, and cached by a platform. So the
    string is checked against the repo's own documentation every build.
    """
    readme = (REPO / "README.md").read_text(encoding="utf-8")
    if INSTALL_CMD not in readme:
        raise SystemExit(
            f"install line drift: {INSTALL_CMD!r} is not in README.md.\n"
            "Update INSTALL_CMD (and re-render) or fix the README."
        )
    if not re.fullmatch(r"[a-z0-9._-]+/[a-z0-9._-]+", REPO_SLUG):
        raise SystemExit(f"repo slug is not org/repo: {REPO_SLUG!r}")


def check_fits(lo: Layout) -> None:
    """Nothing may reach the edge, and the safe box is a hard boundary."""
    if lo.mark_only:
        return
    term_w = terminal_width(lo.term_fs)
    margin = lo.w * 0.04
    if term_w + margin * 2 > lo.w:
        raise SystemExit(f"{lo.name}: terminal ({term_w:.0f}px) crowds the canvas")
    if tw(TAGLINE, lo.tagline_fs) + margin * 2 > lo.w:
        raise SystemExit(f"{lo.name}: tagline overruns the canvas")

    # The action row's two halves must not collide at any aspect ratio.
    fs = lo.action_fs
    left = fs * 1.12 + fs * 0.62 + tw(REPO_SLUG, fs)
    right = fs * 0.95 * 2 + fs * 1.15 + fs * 0.6 + tw(STAR_CTA, fs)
    if left + right + fs > term_w:
        raise SystemExit(f"{lo.name}: slug and star CTA collide")

    if lo.safe:
        term_h = terminal_height(lo.term_fs)
        stack = (
            lo.lockup_h
            + sum(lo.gaps)
            + lo.tagline_fs
            + term_h
            + round(lo.action_fs * 2.4)
        )
        if stack > lo.safe[1]:
            raise SystemExit(f"{lo.name}: stack escapes the {lo.safe[1]}px safe box")
        if term_w > lo.safe[0]:
            raise SystemExit(f"{lo.name}: terminal escapes the safe box")


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------


def render(lo: Layout, th: Theme, out_dir: Path, keep_svg: bool) -> Path:
    markup = compose(lo, th)
    svg_path = out_dir / f"stella-{lo.name}-{th.name}.svg"
    png_path = out_dir / f"stella-{lo.name}-{th.name}.png"
    svg_path.write_text(markup, encoding="utf-8")
    subprocess.run(
        ["rsvg-convert", "-w", str(lo.w), "-h", str(lo.h), str(svg_path), "-o", str(png_path)],
        check=True,
    )
    if not keep_svg:
        svg_path.unlink()
    return png_path


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--check", action="store_true", help="validate only, write nothing")
    ap.add_argument("--keep-svg", action="store_true", help="leave the SVG beside each PNG")
    ap.add_argument("--out", type=Path, default=HERE)
    args = ap.parse_args()

    check_install_line()
    for lo in LAYOUTS:
        check_fits(lo)
    if args.check:
        print(f"ok — install line and {len(LAYOUTS)} layouts fit")
        return 0

    if not shutil.which("rsvg-convert"):
        print("rsvg-convert not found (brew install librsvg)", file=sys.stderr)
        return 1

    args.out.mkdir(parents=True, exist_ok=True)
    for lo in LAYOUTS:
        for th in (DARK, LIGHT):
            path = render(lo, th, args.out, args.keep_svg)
            print(f"  {path.relative_to(REPO)}  {lo.w}×{lo.h}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
