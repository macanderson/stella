#!/usr/bin/env python3
"""Mirror the kit's raster icons onto the website, favicon.ico included.

`docs/brand/build_marks.py` regenerates the kit. Nothing regenerated the
website's copies of it, so every kit recolour left a second step that existed
only as a sentence -- "re-copy it when the kit regenerates" in
`brand-parity.test.ts`, and "re-run build_marks.py and re-encode favicon.ico to
RGBA" in its failure message. A step nobody can run is a step that gets skipped:
v5.0 regenerated all nine kit PWA icons (#4066) and shipped the site still
serving the v4.0 bronze ones.

Two jobs, because the site's copies are not all byte-copies:

1. **The PWA set** is a straight copy, with two renames the site has always
   used (`icon-maskable-*` -> `maskable-*`).

2. **`src/app/favicon.ico` is deliberately not a byte-copy.** The kit renders
   its favicons opaque, so its ICO embeds **RGB** PNGs (colour type 2). Next's
   image pipeline decodes this file through the `ico` crate, which accepts only
   RGBA and fails the production build outright on anything else:

       Caused by: Format error decoding Ico: The PNG is not in RGBA format!

   So the kit's pixels are re-encoded here with an opaque alpha channel. That
   is the one difference between the two files, and it is now produced by a
   command rather than by hand -- which is what let the previous one rot on
   v2.0 art through two whole rebrands while the guard, which checked only the
   encoding, reported green.

No imaging library: a PNG is zlib plus four chunk headers, the same reason
`docs/brand/cometkit.py` gives for writing its own.

Usage:
    scripts/mirror-brand-icons.py            # write the site's copies
    scripts/mirror-brand-icons.py --check    # fail if any is stale
"""

from __future__ import annotations

import struct
import sys
import zlib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
KIT_PWA = REPO / "docs" / "brand" / "pwa"
SITE_ICONS = REPO / "website" / "public" / "icons"
SITE_FAVICON = REPO / "website" / "src" / "app" / "favicon.ico"

# (site name, kit name). The site renames the kit's two maskables; every other
# file keeps its name. Kept in step with the PAIRS table in
# website/src/lib/brand-parity.test.ts, which asserts this exact mapping.
PAIRS: list[tuple[str, str]] = [
    ("favicon-16.png", "favicon-16.png"),
    ("favicon-32.png", "favicon-32.png"),
    ("favicon-48.png", "favicon-48.png"),
    ("icon-192.png", "icon-192.png"),
    ("icon-512.png", "icon-512.png"),
    ("maskable-192.png", "icon-maskable-192.png"),
    ("maskable-512.png", "icon-maskable-512.png"),
    ("safari-pinned-tab.svg", "safari-pinned-tab.svg"),
]

PNG_MAGIC = b"\x89PNG\r\n\x1a\n"


def _chunk(tag: bytes, data: bytes) -> bytes:
    body = tag + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))


def _unfilter(data: bytes, width: int, height: int, bpp: int) -> bytearray:
    """Undo PNG's five per-scanline filters (RFC 2083 §6)."""
    stride = width * bpp
    out = bytearray()
    prev = bytearray(stride)
    pos = 0
    for _ in range(height):
        filter_type = data[pos]
        pos += 1
        line = bytearray(data[pos : pos + stride])
        pos += stride
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
            elif filter_type != 0:
                raise SystemExit(f"unknown PNG filter type {filter_type}")
        out += line
        prev = line
    return out


def png_to_rgba(blob: bytes) -> bytes:
    """Re-encode an 8-bit non-interlaced PNG as colour type 6, alpha 0xFF.

    Accepts colour type 2 (what the kit writes) and 6 (already RGBA, so the
    pixels round-trip). Anything else raises rather than being silently
    accepted -- a guard that quietly skips is the failure this file exists to
    prevent.
    """
    if blob[:8] != PNG_MAGIC:
        raise SystemExit("ICO entry is not PNG-encoded")
    width, height, depth, colour = struct.unpack(">IIBB", blob[16:26])
    interlace = blob[28]
    if depth != 8 or colour not in (2, 6) or interlace != 0:
        raise SystemExit(
            f"expected an 8-bit non-interlaced RGB/RGBA PNG, got depth {depth}, "
            f"colour type {colour}, interlace {interlace}"
        )

    idat = bytearray()
    off = 8
    while off + 8 <= len(blob):
        (length,) = struct.unpack(">I", blob[off : off + 4])
        if blob[off + 4 : off + 8] == b"IDAT":
            idat += blob[off + 8 : off + 8 + length]
        off += 12 + length

    src_bpp = 4 if colour == 6 else 3
    pixels = _unfilter(zlib.decompress(bytes(idat)), width, height, src_bpp)

    raw = bytearray()
    for y in range(height):
        raw.append(0)  # filter type 0 — the art is tiny, so store it plainly
        row = y * width * src_bpp
        for x in range(width):
            at = row + x * src_bpp
            raw += pixels[at : at + 3]
            raw.append(pixels[at + 3] if src_bpp == 4 else 0xFF)

    return (
        PNG_MAGIC
        + _chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
        + _chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + _chunk(b"IEND", b"")
    )


def rgba_ico(ico: bytes) -> bytes:
    """The same ICO, every embedded PNG re-encoded to RGBA."""
    reserved, kind, count = struct.unpack("<HHH", ico[:6])
    if reserved != 0 or kind != 1:
        raise SystemExit("not an ICO file")

    entries, payloads = [], []
    offset = 6 + count * 16
    for i in range(count):
        head = 6 + i * 16
        meta = ico[head : head + 8]  # width, height, colours, reserved, planes, bpp
        size, at = struct.unpack("<II", ico[head + 8 : head + 16])
        payload = png_to_rgba(ico[at : at + size])
        entries.append(meta + struct.pack("<II", len(payload), offset))
        payloads.append(payload)
        offset += len(payload)

    return struct.pack("<HHH", 0, 1, count) + b"".join(entries) + b"".join(payloads)


def main() -> int:
    check = "--check" in sys.argv[1:]
    stale: list[str] = []

    wanted: list[tuple[Path, bytes]] = [
        (SITE_ICONS / site, (KIT_PWA / kit).read_bytes()) for site, kit in PAIRS
    ]
    wanted.append((SITE_FAVICON, rgba_ico((KIT_PWA / "favicon.ico").read_bytes())))

    for path, content in wanted:
        rel = path.relative_to(REPO).as_posix()
        if path.exists() and path.read_bytes() == content:
            continue
        stale.append(rel)
        if not check:
            path.write_bytes(content)

    if check:
        if stale:
            print("website icons have drifted from docs/brand/:", file=sys.stderr)
            for rel in stale:
                print(f"  {rel}", file=sys.stderr)
            print(
                "\nrun scripts/mirror-brand-icons.py to re-copy them.",
                file=sys.stderr,
            )
            return 1
        print(f"brand icons: {len(wanted)} site copies match docs/brand/")
        return 0

    print(f"brand icons: {len(stale)} rewritten, {len(wanted) - len(stale)} already current")
    return 0


if __name__ == "__main__":
    sys.exit(main())
