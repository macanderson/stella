#!/usr/bin/env python3
"""The comet kit — the one copy of stella's geometry, palette, and rasteriser.

Everything visual in `docs/brand/` is the same four-point comet at a different
scale, so the paths that draw it live here and nowhere else. Three builders
import this module — `build_marks.py` (logo exports, PWA icons),
`wallpapers/build_wallpapers.py`, and `social/build_social.py` — and a fourth
copy of `STAR_PATH` in any of them is a bug, not a convenience: the kit has
been recoloured three times in its history, and a stray copy is how one surface
keeps the old palette while every other moves.

`docs/brand/css/tokens.css` is normative for colour; the constants here mirror
it. `docs/brand/logo/svg/` is normative for shape; the paths here are those
files' path data, and `check_svg_parity` in `build_marks.py` fails the build if
the two ever drift.

Rasterising goes through `rasterize`, which is deliberately the only place in
the kit that spawns a renderer. Commit 10781aa31 regenerated all 52 PNGs with a
rasteriser that emitted torn scanlines and channel-separated garbage, and the
damage reached the remote because the exports had no builder to run and no
`--check` to fail — the corruption was invisible until someone opened a file.
One entry point means one place to state the renderer requirement, and one
place a future failure can be caught.
"""

from __future__ import annotations

import base64
import random
import struct
import subprocess
import shutil
import sys
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]

# ---------------------------------------------------------------------------
# brand tokens — docs/brand/css/tokens.css is normative; these mirror it
# ---------------------------------------------------------------------------

# These were `GOLD`/`GOLD_DEEP` through the bronze era. They are role-named now
# for the same reason the CSS ramp was renamed: a hue in a name falsifies
# itself the first time the hue moves, and this one has moved five times — the
# fifth being v5.0, which left these names correct exactly because they never
# encoded a hue.
#
# Every value below is a token from `design/tokens/stella-tokens.json`, which is
# upstream of this file and of `css/tokens.css` alike. `check-tokens.py` fails
# the gate on any hex here that is not one, so the "mirror" this block used to
# promise is now a checked property.
BRAND = "#EFC53F"  # gold — the comet, on either ground
# The mark on a LIGHT ground is the SAME gold. v3.0 and v4.0 each kept a
# separate darker stop here so the mark could clear the 3:1 graphical floor on
# paper, and v5.0 retires that split for two reasons. The first is the system's
# own rule: gold, `text` white and `ink` are the only three mark colours that
# exist, so a fourth stop is out of the palette by construction. The second is
# that the floor was never the right test — WCAG 1.4.3 and 1.4.11 both exempt
# logotypes by name, which is exactly what lets rule 6 permit a gold mark on
# paper while forbidding gold body text there. `scripts/check-contrast.py`
# records this pairing at 1.65:1 as an exemption rather than hiding it: the
# number is stated, and the reason it does not fail is stated beside it.
BRAND_ON_LIGHT = BRAND
# Small brand *text* on light surfaces is not gold at all — rule 6 forbids it,
# and at 1.65:1 the measurement agrees. Brand text on paper is `ink`.
BRAND_DEEP = "#141416"  # ink
INK = "#0A0A0C"  # bg — the canvas
PAPER = "#E8E8EC"  # text — primary text on dark
PAPER_BG = "#FFFFFF"  # paper — the light-mode surface
MUTED_ON_DARK = "#777782"  # muted
# `dim` rather than `muted` on light. Both are tokens; this one is the pairing
# that measures — 8.61:1 on paper against muted's 4.42:1, which is under AA.
MUTED_ON_LIGHT = "#4B4B56"  # dim

# JetBrains Mono advances 0.6em per glyph, which is what lets every string's
# width be known before it is drawn.
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

def tw(text: str, fs: float) -> float:
    """Width of `text` at `fs`. Exact for mono, which is the point."""
    return len(text) * fs * ADVANCE


def esc(text: str) -> str:
    return text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")



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

def grain_rect(w: float, h: float) -> str:
    """The dither, laid over a whole canvas of `w`×`h`."""
    return (
        f'<rect width="{w:g}" height="{h:g}" fill="url(#grain)" '
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
# document + rasteriser
# ---------------------------------------------------------------------------


def svg_doc(w: float, h: float, body: str) -> str:
    """An SVG document of `w`×`h` user units wrapping `body`."""
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" '
        f'xmlns:xlink="http://www.w3.org/1999/xlink" '
        f'width="{w:g}" height="{h:g}" viewBox="0 0 {w:g} {h:g}">{body}</svg>'
    )


def require_renderer() -> None:
    """Fail with the install line rather than a traceback from `subprocess`."""
    if shutil.which("rsvg-convert") is None:
        raise SystemExit(
            "rsvg-convert not found — install it with `brew install librsvg`.\n"
            "Every PNG in docs/brand/ is generated; none may be edited by hand."
        )


def rasterize(markup: str, w: int, h: int | None, out: Path, keep_svg: bool = False) -> Path:
    """Render `markup` to `out` at `w`×`h`, via the kit's one renderer.

    `h` may be None to let the source's aspect ratio choose it, which is what
    the logo exports want: their heights are a property of the artwork, and a
    height asserted here would silently letterbox a mark if a path ever moved.
    """
    require_renderer()
    out.parent.mkdir(parents=True, exist_ok=True)
    svg_path = out.with_suffix(".svg")
    svg_path.write_text(markup, encoding="utf-8")
    try:
        cmd = ["rsvg-convert", "-w", str(w)]
        if h is not None:
            cmd += ["-h", str(h)]
        cmd += [str(svg_path), "-o", str(out)]
        subprocess.run(cmd, check=True)
    finally:
        if not keep_svg:
            svg_path.unlink(missing_ok=True)
    verify_png(out)
    return out


def rasterize_file(src: Path, w: int, out: Path, h: int | None = None) -> Path:
    """Render an SVG *file* — the logo exports, whose source is committed art."""
    require_renderer()
    out.parent.mkdir(parents=True, exist_ok=True)
    cmd = ["rsvg-convert", "-w", str(w)]
    if h is not None:
        cmd += ["-h", str(h)]
    cmd += [str(src), "-o", str(out)]
    subprocess.run(cmd, check=True)
    verify_png(out)
    return out


def png_size(path: Path) -> tuple[int, int]:
    """Width and height straight out of the IHDR chunk."""
    raw = path.read_bytes()
    if raw[:8] != b"\x89PNG\r\n\x1a\n":
        raise SystemExit(f"{path}: not a PNG")
    w, h = struct.unpack(">II", raw[16:24])
    return w, h


def verify_png(path: Path) -> None:
    """Assert the file is a decodable PNG whose pixel data inflates cleanly.

    This is the guard the kit did not have. A torn render still produces a
    structurally valid PNG, so this cannot catch bad *pixels* — but it does
    catch a truncated or half-written file, which is the failure a build script
    is most likely to commit. Judging the pixels is the reviewer's job, and
    `build_marks.py --check` is what puts the diff in front of them.
    """
    raw = path.read_bytes()
    if raw[:8] != b"\x89PNG\r\n\x1a\n":
        raise SystemExit(f"{path}: not a PNG (renderer wrote something else)")
    idat = bytearray()
    off, saw_iend = 8, False
    while off + 8 <= len(raw):
        (length,) = struct.unpack(">I", raw[off : off + 4])
        tag = raw[off + 4 : off + 8]
        data = raw[off + 8 : off + 8 + length]
        (want,) = struct.unpack(">I", raw[off + 8 + length : off + 12 + length])
        if zlib.crc32(tag + data) != want:
            raise SystemExit(f"{path}: CRC failure in {tag.decode()} chunk")
        if tag == b"IDAT":
            idat += data
        if tag == b"IEND":
            saw_iend = True
        off += 12 + length
    if not saw_iend:
        raise SystemExit(f"{path}: truncated — no IEND chunk")
    try:
        zlib.decompress(bytes(idat))
    except zlib.error as exc:  # pragma: no cover - only on a broken writer
        raise SystemExit(f"{path}: pixel data will not inflate ({exc})") from exc


def png_opaque_colours(path: Path) -> dict[tuple[int, int, int], int]:
    """Every fully-opaque RGB value in an 8-bit RGBA PNG, with its pixel count.

    The half `verify_png` deliberately could not do. Its docstring called
    judging the pixels "the reviewer's job", which was honest about the gap and
    wrong about who could close it: a colour-profile drift moves the gold by a
    few points in a direction no reviewer can see on an uncalibrated panel, and
    that is precisely the failure this kit's recolour has to rule out.

    Semi-transparent pixels are skipped rather than un-multiplied. Every edge
    pixel of an antialiased mark is a blend between the fill and the ground, so
    including them would report hundreds of intermediate colours and prove
    nothing; the interior pixels are where the fill either survived or did not.
    """
    raw = path.read_bytes()
    if raw[:8] != b"\x89PNG\r\n\x1a\n":
        raise SystemExit(f"{path}: not a PNG")
    width, height, depth, colour_type = struct.unpack(">IIBB", raw[16:26])
    if (depth, colour_type) != (8, 6):
        raise SystemExit(
            f"{path}: expected 8-bit RGBA (depth 8, colour type 6), "
            f"got depth {depth}, colour type {colour_type}"
        )
    idat = bytearray()
    off = 8
    while off + 8 <= len(raw):
        (length,) = struct.unpack(">I", raw[off : off + 4])
        if raw[off + 4 : off + 8] == b"IDAT":
            idat += raw[off + 8 : off + 8 + length]
        off += 12 + length

    data = zlib.decompress(bytes(idat))
    stride, bpp = width * 4, 4
    counts: dict[tuple[int, int, int], int] = {}
    prev = bytearray(stride)
    pos = 0
    for _ in range(height):
        filter_type = data[pos]
        pos += 1
        line = bytearray(data[pos : pos + stride])
        pos += stride
        # PNG's five per-scanline filters, undone in place (RFC 2083 §6).
        for x in range(stride):
            left = line[x - bpp] if x >= bpp else 0
            up = prev[x]
            up_left = prev[x - bpp] if x >= bpp else 0
            if filter_type == 1:
                line[x] = (line[x] + left) & 0xFF
            elif filter_type == 2:
                line[x] = (line[x] + up) & 0xFF
            elif filter_type == 3:
                line[x] = (line[x] + (left + up) // 2) & 0xFF
            elif filter_type == 4:
                estimate = left + up - up_left
                d_left, d_up = abs(estimate - left), abs(estimate - up)
                d_up_left = abs(estimate - up_left)
                if d_left <= d_up and d_left <= d_up_left:
                    predictor = left
                elif d_up <= d_up_left:
                    predictor = up
                else:
                    predictor = up_left
                line[x] = (line[x] + predictor) & 0xFF
        for x in range(width):
            r, g, b, a = line[x * 4 : x * 4 + 4]
            if a == 0xFF:
                key = (r, g, b)
                counts[key] = counts.get(key, 0) + 1
        prev = line
    return counts

