#!/usr/bin/env python3
"""Render a deck film (see `crates/stella-tui/examples/deck_film.rs`) to mp4.

    cargo run -q --release -p stella-tui --example deck_film > film.jsonl
    scripts/render-deck-film.py film.jsonl -o docs/demo/stella-deck.mp4

The film carries styled character grids plus a camera track; this turns them
into pixels. Output is 1920x1080 (`--size` for a 4K/6K master), 60 fps, H.264
High / yuv420p, `faststart`,
**no audio track at all** — the shape a hero `<video autoplay loop muted>`
wants.

The film's grid is picked to fill 16:9 (see `deck_film.rs::COLS`), so at zoom
1.0 the deck fills the frame. Anything left over is centred on the deck's own
ground rather than on black — `framing` carries that argument, and handles a
grid of any proportion, not just the one this film uses.

## The zoom, and why it is sharp

The obvious way to zoom a terminal recording is to scale up the frame, which
turns 12-pixel text into 18 blurry pixels — the reason most terminal demos look
soft the moment the camera moves. Here the grid is rasterised **once per
distinct grid** at `--supersample` times the width the grid occupies at zoom 1.0
(2.5x by default: a 160x30 grid becomes a 4800x1500 raster of 30x50 px cells),
and each output frame is a sub-pixel crop of that raster resized down to
1920x1080. Every frame is therefore a *downscale* of a raster with more detail
than it needs, at every zoom level in the track: text is supersampled rather
than magnified, and the motion is continuous instead of snapping between integer
font sizes.

Note what the raster is **not** shaped like: the output frame. One cell is one
character and a character's shape belongs to the font, so cell height comes from
the advance ratio (`FONT_ASPECT`), never from 1080/rows. The grid is a 3.2:1
strip and the frame is 16:9; deriving the cell from the frame makes the font
taller than the cell is wide and every run of text overlaps its neighbour.

That holds only while the film's peak zoom stays at or below `--supersample`;
past it a crop would have to be enlarged. The peak is checked up front and the
run refuses rather than quietly rendering the one blurry shot.

## Fonts

Terminals fall back per glyph and so does this: the deck's frames reach for box
drawing, geometric shapes, and a handful of symbols that no single monospace
face carries. The chain is JetBrains Mono (the brand face, `docs/brand/fonts/`)
→ DejaVu Sans Mono → DejaVu Sans → Noto Sans Symbols 2 → Noto Sans Symbols →
FreeMono (`FONT_CHAIN`, with the Debian and Homebrew paths of each), and a
glyph that survives the whole chain is a hard error naming the character — a
silently-rendered tofu box in a hero video is worse than a failed build. On a
Mac: `brew install --cask font-dejavu font-noto-sans-symbols
font-noto-sans-symbols-2`.

Bold is a real face, not a synthesised smear. Dim is applied as an alpha blend
toward the cell's background, italic is drawn upright (the deck uses it for
emphasis, and a faux-oblique monospace shears the box drawing beside it), and
underline is drawn as a rule at the baseline.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
import unicodedata
from collections import OrderedDict
from dataclasses import dataclass
from pathlib import Path

try:
    from PIL import Image, ImageDraw, ImageFont
except ImportError:  # pragma: no cover - environment, not logic
    sys.exit("render-deck-film: needs Pillow (pip install pillow)")

REPO = Path(__file__).resolve().parent.parent

# The font chain, in fallback order. Each entry is (regular, bold); a glyph is
# drawn by the first face that has it.
#
# A fallback face is looked for at every path a platform installs it to — the
# Debian/Ubuntu package path first, then the Homebrew cask (`brew install
# --cask font-dejavu`, which drops into `~/Library/Fonts`). Before the macOS
# paths were listed the chain on a Mac was JetBrains Mono alone, and the first
# v2 glyph outside its coverage (U+25AE `▮`) refused the whole render.
HOME_FONTS = Path.home() / "Library/Fonts"
FONT_CHAIN = [
    (
        [REPO / "docs/brand/fonts/JetBrainsMono-Regular.ttf"],
        [REPO / "docs/brand/fonts/JetBrainsMono-Bold.ttf"],
    ),
    (
        [
            Path("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"),
            HOME_FONTS / "DejaVuSansMono.ttf",
            Path("/Library/Fonts/DejaVuSansMono.ttf"),
        ],
        [
            Path("/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf"),
            HOME_FONTS / "DejaVuSansMono-Bold.ttf",
            Path("/Library/Fonts/DejaVuSansMono-Bold.ttf"),
        ],
    ),
    # Proportional faces from here down. A fallback glyph is drawn centred in
    # its cell (`_draw_centred`), so a face's advance never reaches the layout
    # and a symbol face is as safe as a monospace one. The v2 deck reaches for
    # U+2630 `☰`, U+23F8 `⏸`, U+23BF `⎿` and U+2315 `⌕`, which no monospace
    # face on either platform carries.
    (
        [
            Path("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"),
            HOME_FONTS / "DejaVuSans.ttf",
            Path("/Library/Fonts/DejaVuSans.ttf"),
        ],
        [
            Path("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"),
            HOME_FONTS / "DejaVuSans-Bold.ttf",
            Path("/Library/Fonts/DejaVuSans-Bold.ttf"),
        ],
    ),
    (
        [
            Path("/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf"),
            HOME_FONTS / "NotoSansSymbols2-Regular.ttf",
            Path("/Library/Fonts/NotoSansSymbols2-Regular.ttf"),
        ],
        [],
    ),
    (
        [
            Path("/usr/share/fonts/truetype/noto/NotoSansSymbols-Regular.ttf"),
            HOME_FONTS / "NotoSansSymbols[wght].ttf",
            Path("/Library/Fonts/NotoSansSymbols[wght].ttf"),
        ],
        [],
    ),
    # A Halfwidth and Fullwidth Forms tier (GNU FreeFont / Arial Unicode) used
    # to sit here for the deck's `WRITE` glyph alone — U+FF0B, absent from
    # every face above. #4482 retired that character for the native `+`
    # (U+002B, `stella_tui_theme::glyph::WRITE`), which JetBrains Mono itself
    # carries, so nothing in this vocabulary reaches this deep any more.
]

# The output frame. 1080p by default; `--size` overrides it for a 4K or 6K
# master, and `--supersample` is read relative to whatever this is.
OUT_W, OUT_H = 1920, 1080

# How many grid rasters to keep. Small on purpose — see the loop in `main`.
GRID_CACHE = 6

# The cell is sized from the font, and the font is sized from the cell's
# **height** — never from its width. A monospace face has two metrics that both
# have to fit: an advance (JetBrains Mono: 0.600 em) and a line box
# (ascent + descent: 1.32 em). Sizing to the width picks S = cell_w / 0.6, whose
# line box is then 2.2x the advance and overflows a cell that is not at least
# that tall — the rows collide and the baseline can land below the cell floor.
# That shipped once; `Rasteriser` now measures both and refuses either failure.

# Modifier bits, as `deck_film.rs::mod_bits` writes them.
BOLD, ITALIC, DIM, UNDERLINE = 1, 2, 4, 8

# How far a DIM cell is blended toward its background. The deck uses DIM for
# de-emphasised chrome that must stay legible, so this is gentler than the 50%
# most terminals apply.
DIM_ALPHA = 0.45


def hexrgb(value: str | None, default: tuple[int, int, int]) -> tuple[int, int, int]:
    """`"RRGGBB"` to a tuple; `None` means the terminal default."""
    if value is None:
        return default
    return (int(value[0:2], 16), int(value[2:4], 16), int(value[4:6], 16))


def blend(
    fg: tuple[int, int, int], bg: tuple[int, int, int], alpha: float
) -> tuple[int, int, int]:
    return tuple(round(f + (b - f) * alpha) for f, b in zip(fg, bg))  # type: ignore[return-value]


@dataclass
class Face:
    """One font in the chain, at one size, with its coverage set."""

    regular: ImageFont.FreeTypeFont
    bold: ImageFont.FreeTypeFont
    covers: frozenset[int]

    def font(self, bold: bool) -> ImageFont.FreeTypeFont:
        return self.bold if bold else self.regular


def fit_faces(cell_h: int) -> tuple[list[Face], int]:
    """The largest font size whose line box fits `cell_h`, and the chain at it.

    Steps down rather than solving in closed form: the em-to-line-box ratio is a
    property of whatever face is installed, and a face swapped into
    `FONT_CHAIN[0]` should re-fit rather than silently overflow. The advance is
    checked here too — a size whose advance is fractional is skipped, because a
    run drawn in one call needs whole-pixel columns.
    """
    for size in range(cell_h, 7, -1):
        faces = load_faces(size)
        ascent, descent = faces[0].regular.getmetrics()
        advance = faces[0].regular.getlength("M")
        if ascent + descent <= cell_h and abs(advance - round(advance)) < 0.01:
            return faces, size
    sys.exit(f"render-deck-film: no font size fits a {cell_h}px cell")


def load_faces(px: int) -> list[Face]:
    """Load the font chain at `px`, skipping entries that are not installed."""
    from fontTools.ttLib import TTFont

    faces: list[Face] = []
    for regular_paths, bold_paths in FONT_CHAIN:
        regular = next((p for p in regular_paths if p.exists()), None)
        if regular is None:
            continue
        covers: set[int] = set()
        tt = TTFont(str(regular), lazy=True)
        for table in tt["cmap"].tables:
            covers |= set(table.cmap.keys())
        tt.close()
        bold_path = next((p for p in bold_paths if p.exists()), regular)
        faces.append(
            Face(
                regular=ImageFont.truetype(str(regular), px),
                bold=ImageFont.truetype(str(bold_path), px),
                covers=frozenset(covers),
            )
        )
    if not faces:
        sys.exit(
            "render-deck-film: no usable font. Expected the brand face at "
            f"{FONT_CHAIN[0][0][0].relative_to(REPO)}"
        )
    return faces


class Rasteriser:
    """Draws a styled character grid at a fixed cell size."""

    def __init__(self, cols: int, rows: int, cell_h: int) -> None:
        """Size the type to a cell `cell_h` tall, then take the width it implies.

        The order matters and is the whole correctness argument. A monospace
        face has to satisfy two constraints at once — its **line box** must fit
        the cell's height, or consecutive rows draw into each other, and its
        **advance** must be exactly the cell's width, or a run drawn with one
        `draw.text` call walks out of its columns. Only one of them can be
        chosen freely. Choosing the height (rows are what a terminal fixes) and
        deriving the width from the measured advance satisfies both; choosing
        the width and deriving the height satisfies neither, because 1.32 em of
        line box does not fit in 0.6 em / 0.6 = 1.0 em of cell.
        """
        self.cols, self.rows = cols, rows
        self.cell_h = cell_h
        self.faces, self.size = fit_faces(cell_h)
        self.primary = self.faces[0]

        advance = self.primary.regular.getlength("M")
        self.cell_w = round(advance)
        if abs(advance - self.cell_w) > 0.01:
            sys.exit(
                f"render-deck-film: the primary face advances {advance:.3f}px at "
                f"size {self.size}, which is not a whole pixel. A run is drawn in "
                "one call, so a fractional advance walks the run out of its "
                "columns; pick a cell height whose fitted size lands on an "
                "integer advance."
            )

        ascent, descent = self.primary.regular.getmetrics()
        if ascent + descent > cell_h:
            sys.exit(
                f"render-deck-film: the primary face's line box is "
                f"{ascent + descent}px at size {self.size} but the cell is only "
                f"{cell_h}px tall — consecutive rows would overlap. This is the "
                "failure that ships as 'the text looks scrunched'."
            )
        # Centre the line box in the cell, so the leftover leading is split
        # above and below rather than all landing under the baseline (which is
        # what pushed the last row off the bottom of the frame).
        self.baseline = ascent + (cell_h - (ascent + descent)) // 2

        self.width, self.height = cols * self.cell_w, rows * cell_h
        self._face_cache: dict[int, Face | None] = {}

    def face_for(self, ch: str) -> Face:
        """The first face in the chain that covers `ch`."""
        cp = ord(ch)
        cached = self._face_cache.get(cp)
        if cached is not None:
            return cached
        for face in self.faces:
            if cp in face.covers:
                self._face_cache[cp] = face
                return face
        raise SystemExit(
            f"render-deck-film: no font in the chain draws U+{cp:04X} ({ch!r}). "
            "Add a face that covers it to FONT_CHAIN — a missing glyph would "
            "render as an empty box in the video."
        )

    def render(self, rows: list, ground: tuple[int, int, int]) -> Image.Image:
        img = Image.new("RGB", (self.width, self.height), ground)
        draw = ImageDraw.Draw(img)

        for y, row in enumerate(rows):
            top = y * self.cell_h
            x = 0
            for fg_hex, bg_hex, mods, text in row:
                width = len(text)
                fg = hexrgb(fg_hex, (0xF4, 0xF1, 0xEA))
                bg = hexrgb(bg_hex, ground)
                left = x * self.cell_w
                span = width * self.cell_w

                if bg != ground:
                    draw.rectangle(
                        [left, top, left + span - 1, top + self.cell_h - 1], fill=bg
                    )
                if mods & DIM:
                    fg = blend(fg, bg, DIM_ALPHA)

                if text.strip():
                    self._draw_run(draw, text, left, top, fg, bool(mods & BOLD))
                if mods & UNDERLINE:
                    rule = top + self.baseline + max(1, self.cell_h // 16)
                    draw.rectangle([left, rule, left + span - 1, rule], fill=fg)
                x += width
        return img

    def _draw_run(
        self,
        draw: ImageDraw.ImageDraw,
        text: str,
        left: int,
        top: int,
        fg: tuple[int, int, int],
        bold: bool,
    ) -> None:
        """Draw one style run.

        The common case is the whole run in the primary face, which is one
        `draw.text` call for the run rather than one per cell — the difference
        between a raster in milliseconds and one in seconds. A run containing a
        fallback glyph is split at that glyph and the pieces drawn around it.
        """
        baseline = top + self.baseline
        start = 0
        i = 0
        while i <= len(text):
            at_end = i == len(text)
            face = None if at_end else self.face_for(text[i])
            if at_end or face is not self.primary:
                if i > start:
                    draw.text(
                        (left + start * self.cell_w, baseline),
                        text[start:i],
                        font=self.primary.font(bold),
                        fill=fg,
                        anchor="ls",
                    )
                if not at_end:
                    assert face is not None
                    # Centre a fallback glyph in its cell: its advance is not
                    # ours, so trusting it would drift the rest of the run.
                    self._draw_centred(
                        draw, text[i], left + i * self.cell_w, baseline, face, fg, bold
                    )
                start = i + 1
            i += 1

    def _draw_centred(
        self,
        draw: ImageDraw.ImageDraw,
        ch: str,
        left: int,
        baseline: int,
        face: Face,
        fg: tuple[int, int, int],
        bold: bool,
    ) -> None:
        font = face.font(bold)
        advance = font.getlength(ch) or self.cell_w
        # A wide (East Asian W/F) glyph owns two cells: the deck's buffer
        # leaves a continuation space after it, so centring over both keeps
        # the glyph where a terminal would draw it rather than spilling it
        # half a cell to the left of its column.
        span = self.cell_w * (2 if unicodedata.east_asian_width(ch) in "WF" else 1)
        draw.text(
            (left + (span - advance) / 2, baseline),
            ch,
            font=font,
            fill=fg,
            anchor="ls",
        )


def framing(
    cam: list[float], cols: int, rows: int, cell_w: int, cell_h: int
) -> tuple[tuple[float, float, float, float], tuple[int, int]]:
    """Where the camera looks, and how big that lands on the output frame.

    Returns `(crop_box_in_raster_px, (dest_w, dest_h))`.

    Zoom is defined against **fit-to-width**: at 1.0 the grid spans the frame
    horizontally, at 2.0 half of it does. The deck's grid is a wide strip
    (160x26 is 3.7:1), so at the wide end it does not fill a 16:9 frame and is
    centred on the deck's own ground — and every step of the push-in closes
    those bands, until at roughly 2.08x the strip is taller than the frame and
    starts cropping vertically as well. That is the whole reason zoom is
    anchored to width: the camera move doubles as the composition, rather than
    the frame being padded around a grid nothing filled.

    Both axes clamp inside the grid, so a pan near an edge cannot show ground
    the grid never drew.
    """
    zoom, cx, cy = cam
    zoom = max(1.0, zoom)

    # Grid cells visible across the frame, and how many rows that many columns
    # implies at the output's aspect ratio.
    view_w = cols / zoom
    view_h = min(view_w * OUT_H / OUT_W * cell_w / cell_h, float(rows))

    x0 = min(max(cx * cols - view_w / 2, 0.0), max(0.0, cols - view_w))
    y0 = min(max(cy * rows - view_h / 2, 0.0), max(0.0, rows - view_h))

    box = (x0 * cell_w, y0 * cell_h, (x0 + view_w) * cell_w, (y0 + view_h) * cell_h)
    # Height on the output frame: full when the strip is at least as tall as the
    # frame, proportionally short (and centred) when it is not.
    dest_h = round(OUT_H * view_h / (view_w * OUT_H / OUT_W * cell_w / cell_h))
    return box, (OUT_W, min(OUT_H, max(1, dest_h)))


def ffmpeg_bin() -> str:
    found = shutil.which("ffmpeg")
    if found:
        return found
    try:
        import imageio_ffmpeg

        return imageio_ffmpeg.get_ffmpeg_exe()
    except ImportError:
        sys.exit(
            "render-deck-film: needs ffmpeg on PATH (or `pip install imageio-ffmpeg`)"
        )


def main() -> None:
    global OUT_W, OUT_H
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("film", type=Path, help="the .jsonl written by deck_film")
    ap.add_argument("-o", "--out", type=Path, required=True, help="output .mp4")
    ap.add_argument(
        "--supersample",
        type=float,
        default=2.5,
        help="raster scale relative to 1080p; must be >= the film's peak zoom",
    )
    # 24, above x264's default. Terminal text is high-frequency detail on a flat
    # ground and this film's camera pans across it almost continuously, which is
    # close to the worst case for inter-frame prediction — so the usual
    # "visually lossless 18" costs far more here than it does on camera footage.
    # Measured on this film: CRF 18 encodes to 31.2 MB (5.0 Mbps), more than a
    # hero video should ask a visitor to download. Glyph edges are what to
    # protect if this is raised further — check it on a zoomed shot, not a wide
    # one, since the wide shots have the least detail per pixel.
    ap.add_argument("--crf", type=int, default=24, help="x264 quality (lower is better)")
    ap.add_argument(
        "--size",
        default=f"{OUT_W}x{OUT_H}",
        help="output frame, WxH (default 1920x1080; 3840x2160 and 6144x3456 are "
        "the 4K and 6K 16:9 masters). The raster scales with it, so a 6K render "
        "at --supersample 2.5 is a 15K-wide raster per grid — use ~1.6 there.",
    )
    ap.add_argument(
        "--preset",
        default="slow",
        help="x264 preset; `medium` roughly halves a 6K encode's wall clock",
    )
    ap.add_argument(
        "--poster",
        type=Path,
        help="also write this frame as a still (a `poster` for the <video>)",
    )
    ap.add_argument(
        "--poster-frame", type=int, default=0, help="which frame the poster is cut from"
    )
    ap.add_argument(
        "--stills",
        help="comma-separated frame indices to also write as PNGs (for review "
        "and for cutting social stills), named <out-stem>-<index>.png",
    )
    args = ap.parse_args()

    try:
        w, h = (int(n) for n in args.size.lower().split("x"))
    except ValueError:
        sys.exit(f"render-deck-film: --size wants WxH, got {args.size!r}")
    if w % 2 or h % 2 or w < 2 or h < 2:
        sys.exit("render-deck-film: --size must be even on both axes (yuv420p)")
    OUT_W, OUT_H = w, h

    stills = (
        {int(n) for n in args.stills.split(",") if n.strip()} if args.stills else set()
    )

    lines = args.film.read_text().splitlines()
    if not lines:
        sys.exit(f"render-deck-film: {args.film} is empty")
    header = json.loads(lines[0])
    if header.get("kind") != "header":
        sys.exit("render-deck-film: first line is not a header")

    cols, rows, fps = header["cols"], header["rows"], header["fps"]
    frames = [json.loads(line) for line in lines[1:]]

    peak = max(f["cam"][0] for f in frames)
    if peak > args.supersample + 1e-9:
        sys.exit(
            f"render-deck-film: peak zoom {peak:.2f}x exceeds --supersample "
            f"{args.supersample:.2f}x, so the closest shot would be enlarged "
            "rather than downscaled. Raise --supersample."
        )

    # The raster has the *grid's* aspect, not the frame's: one cell is one
    # character, and a character's shape is the font's business. The row count
    # fixes the cell height; the face's advance at the size that fits then fixes
    # the width, and the grid's aspect falls out of the two. `framing` pads
    # whatever is left over — which is nothing when the film's cols/rows are
    # picked for it (see `deck_film.rs::COLS`).
    cell_h = round(OUT_H * args.supersample / rows)
    raster = Rasteriser(cols, rows, cell_h)
    cell_w = raster.cell_w
    ground = (0x0B, 0x0B, 0x0C)  # palette::GROUND — the deck's own ink

    print(
        f"render-deck-film: {len(frames)} frames · {cols}x{rows} grid · "
        f"{raster.width}x{raster.height} raster ({cell_w}x{cell_h} cells) · "
        f"peak zoom {peak:.2f}x",
        file=sys.stderr,
    )

    args.out.parent.mkdir(parents=True, exist_ok=True)
    proc = subprocess.Popen(
        [
            ffmpeg_bin(), "-hide_banner", "-loglevel", "error", "-y",
            "-f", "rawvideo", "-pix_fmt", "rgb24",
            "-s", f"{OUT_W}x{OUT_H}", "-r", str(fps), "-i", "-",
            "-an",                       # no audio track at all
            "-c:v", "libx264", "-profile:v", "high", "-preset", args.preset,
            "-crf", str(args.crf), "-pix_fmt", "yuv420p",
            "-movflags", "+faststart",
            str(args.out),
        ],
        stdin=subprocess.PIPE,
    )
    assert proc.stdin is not None

    # One raster per distinct grid, kept in a small LRU. Each is tens of
    # megabytes at 2.5x supersample and this film has ninety-odd of them, so
    # holding all of them is gigabytes for no gain: grids are cited in
    # contiguous runs, and the few frames that cut back to an earlier state
    # re-render in milliseconds.
    cache: OrderedDict[int, Image.Image] = OrderedDict()
    black = Image.new("RGB", (OUT_W, OUT_H), (0, 0, 0))
    grids: dict[int, list] = {}
    poster_written = False

    for n, frame in enumerate(frames):
        grid_id = frame["grid"]
        if "rows" in frame:
            grids[grid_id] = frame["rows"]
        if grid_id not in grids:
            sys.exit(f"render-deck-film: frame {n} cites unseen grid {grid_id}")
        if grid_id in cache:
            cache.move_to_end(grid_id)
        else:
            cache[grid_id] = raster.render(grids[grid_id], ground)
            while len(cache) > GRID_CACHE:
                cache.popitem(last=False)
        source = cache[grid_id]

        box, dest = framing(frame["cam"], cols, rows, cell_w, cell_h)
        strip = source.resize(dest, resample=Image.LANCZOS, box=box)
        if dest[1] == OUT_H:
            out = strip
        else:
            # The strip is shorter than the frame: centre it on the deck's own
            # ground, so the bands read as the terminal's backdrop rather than
            # as letterboxing bolted on afterwards.
            out = Image.new("RGB", (OUT_W, OUT_H), ground)
            out.paste(strip, (0, (OUT_H - dest[1]) // 2))
        fade = frame.get("fade", 0.0)
        if fade > 0.0:
            out = Image.blend(out, black, min(1.0, fade))

        if args.poster and not poster_written and n == args.poster_frame:
            args.poster.parent.mkdir(parents=True, exist_ok=True)
            out.save(args.poster)
            poster_written = True
        if n in stills:
            out.save(args.out.with_name(f"{args.out.stem}-{n:05d}.png"))

        proc.stdin.write(out.tobytes())
        if n % 300 == 0:
            print(
                f"  {n}/{len(frames)} ({n / max(1, len(frames)):.0%})", file=sys.stderr
            )

    proc.stdin.close()
    code = proc.wait()
    if code != 0:
        sys.exit(f"render-deck-film: ffmpeg exited {code}")

    size = args.out.stat().st_size
    seconds = len(frames) / fps
    print(
        f"render-deck-film: wrote {args.out} — {seconds:.1f}s, "
        f"{size / 1e6:.1f} MB, {OUT_W}x{OUT_H}@{fps}",
        file=sys.stderr,
    )
    if args.poster and poster_written:
        print(f"render-deck-film: poster {args.poster}", file=sys.stderr)


if __name__ == "__main__":
    main()
