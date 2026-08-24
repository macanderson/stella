#!/usr/bin/env python3
"""Mirror the brand kit's outputs into `website/` — the producer the parity
test checks.

`website/src/lib/brand-parity.test.ts` asserts that the site's logo SVGs, PWA
icons, `src/app/icon.svg` and the pixels inside `src/app/favicon.ico` are the
kit's. Until this script existed nothing *performed* that copy: every recolour
ended with a human running a dozen `cp` commands, and the guard's only job was
to catch the ones they missed — which it did twice, after the miss had already
shipped (#3983). This is the single way the mirrored files are produced; the
parity test is the check on it rather than the process.

    python3 docs/brand/sync_site.py          # or: make brand-sync
    python3 docs/brand/build_marks.py --sync-site   # regenerate + mirror

The mapping below is the same one the test's `PAIRS` table encodes, renames
included (`icon-maskable-{192,512}.png` → `maskable-{192,512}.png`). One file
is deliberately NOT a byte-copy: `src/app/favicon.ico` is the kit's pixels
re-encoded to RGBA (`cometkit.ico_as_rgba`), because Next's `ico` decoder
rejects the kit's opaque RGB PNGs and fails the production build on them. The
re-encode is compared by *pixels* before writing, so a compressor that emits
different bytes for the same art does not churn the working tree.

Verify with:

    cd website && node --test src/lib/brand-parity.test.ts
"""

from __future__ import annotations

import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import cometkit as ck  # noqa: E402  (path shim must precede the import)

REPO = HERE.parents[1]
SITE = REPO / "website"

SVG_SRC = HERE / "logo" / "svg"
PWA_SRC = HERE / "pwa"
BRAND_DEST = SITE / "public" / "brand"
ICONS_DEST = SITE / "public" / "icons"

# site name -> kit name, exactly the parity test's PAIRS table. The site
# renames the kit's two maskables; every other file keeps its name.
PWA_PAIRS: list[tuple[str, str]] = [
    ("favicon-16.png", "favicon-16.png"),
    ("favicon-32.png", "favicon-32.png"),
    ("favicon-48.png", "favicon-48.png"),
    ("icon-192.png", "icon-192.png"),
    ("icon-512.png", "icon-512.png"),
    ("maskable-192.png", "icon-maskable-192.png"),
    ("maskable-512.png", "icon-maskable-512.png"),
    ("safari-pinned-tab.svg", "safari-pinned-tab.svg"),
]


def _copy(src: Path, dest: Path) -> bool:
    """Byte-copy `src` over `dest`, skipping an already-identical file."""
    if not src.exists():
        raise SystemExit(f"missing kit file: {src.relative_to(REPO)}")
    data = src.read_bytes()
    if dest.exists() and dest.read_bytes() == data:
        return False
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_bytes(data)
    print(f"  wrote {dest.relative_to(REPO)}")
    return True


def _ico_pixels(ico: bytes) -> list[tuple[int, int, int, bytes]]:
    """Every entry's art as raw RGBA, the identity the parity test compares."""
    out: list[tuple[int, int, int, bytes]] = []
    for size, png in sorted(ck.ico_entries(ico).items()):
        width, height, pixels = ck.png_rgba_pixels(png)
        out.append((size, width, height, pixels))
    return out


def sync() -> int:
    """Mirror everything; the count of files written."""
    wrote = 0

    # The site carries exactly the kit's SVG set, so a cut the kit retires
    # leaves the mirror too — the parity test asserts set equality, and a
    # stray file it fails on would otherwise need a hand delete.
    kit_svgs = sorted(p.name for p in SVG_SRC.glob("*.svg"))
    if not kit_svgs:
        raise SystemExit(f"no SVGs under {SVG_SRC.relative_to(REPO)}")
    for name in kit_svgs:
        wrote += _copy(SVG_SRC / name, BRAND_DEST / name)
    for stray in BRAND_DEST.glob("*.svg"):
        if stray.name not in kit_svgs:
            stray.unlink()
            print(f"  removed {stray.relative_to(REPO)} (not in the kit)")
            wrote += 1

    for site_name, kit_name in PWA_PAIRS:
        wrote += _copy(PWA_SRC / kit_name, ICONS_DEST / site_name)

    wrote += _copy(SVG_SRC / "logomark-color.svg", SITE / "src" / "app" / "icon.svg")

    # favicon.ico: the one non-byte-copy. Written only when the *pixels*
    # differ, so re-running the sync under a different zlib is a no-op.
    kit_ico = (PWA_SRC / "favicon.ico").read_bytes()
    dest = SITE / "src" / "app" / "favicon.ico"
    fresh = ck.ico_as_rgba(kit_ico)
    if not dest.exists() or _ico_pixels(dest.read_bytes()) != _ico_pixels(fresh):
        dest.write_bytes(fresh)
        print(f"  wrote {dest.relative_to(REPO)}")
        wrote += 1

    return wrote


def main() -> int:
    wrote = sync()
    if wrote:
        print(f"{wrote} file(s) updated — commit them with the kit change")
    else:
        print("mirror already current; nothing written")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
