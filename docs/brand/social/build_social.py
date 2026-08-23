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
sys.path.insert(0, str(HERE.parent))

import cometkit as ck  # noqa: E402  (path shim must precede the import)

# ---------------------------------------------------------------------------
# brand tokens and geometry — cometkit holds the only copy
# ---------------------------------------------------------------------------
#
# Bound to module names here instead of reached through `ck.` at each use,
# because this file writes them into SVG f-strings some three dozen times. A
# binding is not a copy: `docs/brand/css/tokens.css` stays normative for
# colour, `docs/brand/logo/svg/` for shape, cometkit mirrors both, and a
# recolour there moves this file with it. The literals these replaced had to
# be hand-edited for #3658 and again for #3968, which is the failure cometkit's
# module docstring names.
BRAND = ck.BRAND
BRAND_DEEP = ck.BRAND_DEEP
INK = ck.INK
PAPER = ck.PAPER
PAPER_BG = ck.PAPER_BG
MUTED_ON_DARK = ck.MUTED_ON_DARK
MUTED_ON_LIGHT = ck.MUTED_ON_LIGHT

# The repo this art advertises, and the one command that installs it.
REPO_SLUG = "macanderson/stella"
INSTALL_CMD = "brew install macanderson/tap/stella"
TAGLINE = "the terminal agent — faster · cheaper · more accurate"
STAR_CTA = "star the repo"

# JetBrains Mono is monospace: one advance, every glyph, forever.
ADVANCE = ck.ADVANCE

# ---------------------------------------------------------------------------
# geometry — cometkit is the only copy; docs/brand/logo/svg/ is normative
# ---------------------------------------------------------------------------
#
# `build_marks.check_svg_parity` measures cometkit's star path and trail rects
# against `logo/svg/logomark-color.svg`, so binding them here puts the banners
# under that check as well. The wordmark and the GitHub mark have no such
# check on either side of the binding.

STAR_PATH = ck.STAR_PATH
STAR_BOX = ck.STAR_BOX
STAR_CX, STAR_CY = ck.STAR_CX, ck.STAR_CY
TRAIL_RECTS = ck.TRAIL_RECTS
WORDMARK_PATH = ck.WORDMARK_PATH
LOCKUP_W, LOCKUP_H = ck.LOCKUP_W, ck.LOCKUP_H
GITHUB_PATH = ck.GITHUB_PATH
GITHUB_BOX = ck.GITHUB_BOX


# ---------------------------------------------------------------------------
# theme
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Theme:
    """One ground and everything that has to change with it.

    Note what does *not* change: the comet stays `BRAND` on both grounds.
    Only brand-coloured *lettering* drops, and only on light.

    That is the kit's rule now, and no longer a carve-out this file makes.
    v5.0 retired the darker light-mark stop v3.0 introduced and v4.0 kept:
    gold, `text` and `ink` are the only three mark colours the system defines,
    so there is no fourth stop for the mark to step down to — `BRAND_ON_LIGHT`
    in cometkit is `BRAND`, and `--stella-mark-shape` in
    `docs/brand/css/tokens.css` is `--stella-brand`. What licenses the gold on
    paper is the logotype exemption rather than the ratio: WCAG 1.4.3 and
    1.4.11 both carve out logos and brand marks by name, and a banner is an
    image in a feed rather than an operable control.
    `tokens.css` is where that reasoning is normative;
    `scripts/check-contrast.py` carries the same pairing on the token
    system's warm `paper` (#FFFCF5, 1.61:1) and prints the measurement with
    the verdict `exempt`, so the number is on the record beside the reason it
    does not fail.

    Lettering is the other role and gets no exemption. Gold on this canvas's
    #FFFFFF measures 1.65:1 against a 4.5:1 body-text floor, so `brand_text`
    is `BRAND_DEEP` — ink, 18.4:1 — on the light ground, and `BRAND` only on
    the dark one.
    """

    name: str
    bg: str
    fg: str
    muted: str
    brand_text: str  # brand tone that passes as small text on this ground
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
    brand_text=BRAND,
    surface="#0D1319",
    surface_top="#141B22",
    border="#26262C",
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
    brand_text=BRAND_DEEP,
    surface="#F9F6EF",
    surface_top="#E3E8EE",
    border="#E6E3DD",
    grid="#0A0A0C",
    grid_op=0.05,
    sweep_dark="#B4BCC6",
    sweep_dark_op=0.22,
    sweep_warm_op=0.10,
    glow_op=0.10,
    band="#E7EBF0",
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
        f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="3.5" fill="{BRAND}"/>'
        for x, y, w, h in TRAIL_RECTS
    )
    return (
        f'<g transform="translate({tx:.3f} {ty:.3f}) scale({k:.5f})">'
        f"{trails}"
        f'<path d="{STAR_PATH}" fill="{BRAND}"/>'
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
        f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="3.5" fill="{BRAND}"/>'
        for x, y, w, h in TRAIL_RECTS
    )
    return (
        f'<g transform="translate({tx:.3f} {ty:.3f}) scale({k:.5f})">'
        f'{trails}<path d="{STAR_PATH}" fill="{BRAND}"/></g>'
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
    out.append(sweep(lo, (0.97, 0.64), 1.90, +1, BRAND, th.sweep_warm_op))

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
        fill = BRAND if warm else (PAPER if th.is_dark else MUTED_ON_LIGHT)
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
        ((BRAND, 0.95), (th.muted, 0.45), (th.muted, 0.28))
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
    out.append(label(x + pad, base, "$", fs, th.brand_text, weight=700))
    cmd_x = x + pad + tw("$ ", fs)
    out.append(label(cmd_x, base, INSTALL_CMD, fs, th.fg, weight=500))

    # A resting block cursor — the tell that says "terminal", not "code block".
    cur_x = cmd_x + tw(INSTALL_CMD + " ", fs)
    out.append(
        f'<rect x="{cur_x:.2f}" y="{base - fs * 0.72:.2f}" '
        f'width="{fs * ADVANCE:.2f}" height="{fs * 0.95:.2f}" '
        f'fill="{th.brand_text}" opacity="0.9"/>'
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
        f'rx="{h / 2:.2f}" fill="{BRAND}" fill-opacity="{0.12 if th.is_dark else 0.16:g}" '
        f'stroke="{th.brand_text}" stroke-opacity="0.55" stroke-width="1.5"/>'
    )
    out.append(star(px + pad + sstar / 2, cy, sstar, BRAND))
    out.append(
        label(
            px + pad + sstar + fs * 0.6,
            cy + fs * 0.36,
            STAR_CTA,
            fs,
            th.brand_text,
            weight=700,
        )
    )
    return "".join(out)


# ---------------------------------------------------------------------------
# composition
# ---------------------------------------------------------------------------


def compose(lo: Layout, th: Theme) -> str:
    defs = (
        f'<defs><radialGradient id="glow"><stop offset="0" stop-color="{BRAND}" '
        f'stop-opacity="{th.glow_op:g}"/><stop offset="1" stop-color="{BRAND}" '
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
