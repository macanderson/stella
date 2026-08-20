#!/usr/bin/env python3
"""Draw the stella wallpapers — the comet on its trajectory.

The kit's one sentence about its own mark is "a four-point star moving fast
enough to leave a trail". The wallpaper it replaced did not say that: it was a
flat ground, a faint starfield and the mark parked in the middle. This one is
the sentence — a tapered streak entering off-canvas, thinning debris strung
along it, and the comet at the head with the light pooling around it.

Four pictures, each at the three sizes in the ladder:

    desktop  the trail crosses left to right and climbs, so the mass sits low
             and right of centre and the top-right corner — where macOS keeps
             its icons — stays quiet
    phone    the trail falls steeply; the comet rides the upper third, clear of
             the lock-screen clock, and the bottom half stays calm enough to
             read notifications over

Two rules shape every choice here, and both are about a wallpaper being a
*background*:

- **Nothing is filtered.** librsvg evaluates `feGaussianBlur` per pixel, and at
  6016x3384 the cost is minutes per file. Every glow is a `radialGradient`
  instead, and every soft edge is a handful of gradient strokes at falling
  width — cheaper than a blur and, at this scale, indistinguishable from one.
- **Every gradient is dithered.** A wide, soft ramp across near-black is the
  one thing 8 bits per channel cannot hold: it posterises into visible steps,
  and a 6K canvas makes each step centimetres wide. `cometkit.grain_rect` lays
  the kit's seeded noise over the whole canvas to break them up. Remove it and
  the sky stripes.

The starfield is seeded per canvas and ground, so a re-run reproduces the
committed PNGs.

    python3 docs/brand/wallpapers/build_wallpapers.py           # write PNGs
    python3 docs/brand/wallpapers/build_wallpapers.py --check   # validate only
    python3 docs/brand/wallpapers/build_wallpapers.py --preview # one small pair

Requires rsvg-convert (`brew install librsvg`).
"""

from __future__ import annotations

import argparse
import math
import random
import sys
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))

import cometkit as ck  # noqa: E402  (path shim must precede the import)


# ---------------------------------------------------------------------------
# canvases
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Canvas:
    form: str  # "desktop" | "phone"
    tier: str  # "4k" | "5k" | "6k"
    w: int
    h: int

    @property
    def out_name(self) -> str:
        return f"stella-wallpaper-{self.form}-{self.tier}"

    @property
    def diag(self) -> float:
        return math.hypot(self.w, self.h)


# The ladder the kit has always shipped. Phone tiers are the desktop tiers
# turned on their side, which is why 6k phone is 3384 wide rather than 6016.
CANVASES = [
    Canvas("desktop", "4k", 3840, 2160),
    Canvas("desktop", "5k", 5120, 2880),
    Canvas("desktop", "6k", 6016, 3384),
    Canvas("phone", "4k", 2160, 3840),
    Canvas("phone", "5k", 2880, 5120),
    Canvas("phone", "6k", 3384, 6016),
]


# ---------------------------------------------------------------------------
# grounds
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Ground:
    """One sky and everything that has to change with it.

    The comet stays full-strength gold on both, per the kit's rule that the mark is a
    shape and never drops to gold-deep. What inverts is the *field*: on dark the
    specks are light on a night sky, on light they are ink on warm paper, which
    reads as a fine texture rather than a washed-out photograph of stars.
    """

    name: str
    sky_top: str
    sky_bottom: str
    haze: str  # the broad atmospheric wash behind everything
    haze_op: float
    bloom: str  # the light pooling around the comet head
    bloom_op: float
    speck: str
    speck_op: float
    warm_speck: str
    streak_core: str  # the hot centre of the trail at its head
    grid: str
    grid_op: float
    vignette_op: float


DARK = Ground(
    name="dark",
    sky_top="#04070B",
    sky_bottom="#101821",
    haze="#EFC53F",
    haze_op=0.11,
    bloom="#F0C070",
    bloom_op=0.30,
    speck="#E8E8EC",
    speck_op=0.55,
    warm_speck="#EFC53F",
    streak_core="#FFE9C2",
    grid="#E8E8EC",
    grid_op=0.022,
    vignette_op=0.55,
)

LIGHT = Ground(
    name="light",
    sky_top="#F7FAFD",
    sky_bottom="#E3EAF2",
    haze="#EFC53F",
    haze_op=0.12,
    bloom="#E0A44A",
    bloom_op=0.20,
    speck="#7A8492",
    speck_op=0.34,
    warm_speck="#141416",
    # On paper the head cannot be the *lightest* point — white on warm white is
    # nothing, and the trail loses its direction of travel. It deepens instead:
    # the same ramp, run toward saturation rather than toward light.
    streak_core="#141416",
    grid="#0A0A0C",
    grid_op=0.030,
    vignette_op=0.16,
)

GROUNDS = [DARK, LIGHT]


# ---------------------------------------------------------------------------
# layout — one composition per form factor, in canvas fractions
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Plot:
    """Where the comet sits and where it came from, as fractions of the canvas.

    `tail` and `bend` are outside 0..1 on purpose: the trail has to enter from
    off-canvas or it reads as a stroke that starts nowhere, and the control
    point sits beyond the tail to keep the curve's camber shallow.
    """

    head: tuple[float, float]
    tail: tuple[float, float]
    bend: tuple[float, float]
    mark_frac: float  # comet ink width, as a fraction of the short edge
    streak_frac: float  # plume half-width at the tail, as a fraction of the short edge
    wordmark_frac: float  # lockup width, as a fraction of the short edge
    wordmark_at: tuple[float, float]
    # A second comet, far off and much fainter, running the same way. It is
    # here for scale: with one subject the canvas has no depth cue and the
    # empty half of the frame reads as unfinished rather than as distance.
    far_head: tuple[float, float]
    far_tail: tuple[float, float]
    far_scale: float


PLOTS = {
    # Left to right and climbing. The head sits on the right third and low of
    # centre, which leaves the top-right quiet for icons and keeps the trail
    # from running under the dock.
    "desktop": Plot(
        head=(0.655, 0.470),
        tail=(-0.22, 0.880),
        bend=(0.16, 0.760),
        mark_frac=0.185,
        streak_frac=0.042,
        wordmark_frac=0.150,
        wordmark_at=(0.500, 0.905),
        far_head=(0.880, 0.180),
        far_tail=(0.545, 0.330),
        far_scale=0.20,
    ),
    # Falling steeply, head in the upper third: above it is clear for the lock
    # clock, below it is calm enough to stack notifications on.
    "phone": Plot(
        head=(0.560, 0.335),
        tail=(-0.30, -0.060),
        bend=(0.02, 0.230),
        mark_frac=0.290,
        streak_frac=0.058,
        wordmark_frac=0.280,
        wordmark_at=(0.500, 0.892),
        # Further along the same trajectory, low and right: it fills the corner
        # the main comet leaves empty without landing in the middle of the
        # notification stack.
        far_head=(0.845, 0.660),
        far_tail=(0.590, 0.545),
        far_scale=0.20,
    ),
}


# ---------------------------------------------------------------------------
# primitives
# ---------------------------------------------------------------------------


def radial(gid: str, cx: float, cy: float, r: float, color: str, op: float, falloff: float = 2.2) -> str:
    """A soft round glow as a gradient rather than a blurred shape.

    The stops follow `(1 - t) ** falloff`, which is the shape a Gaussian's
    tail actually has, and it costs a gradient lookup instead of a convolution.

    Ten stops rather than a handful: a renderer interpolates *linearly* between
    stops, so a sparse set of samples off a curved ramp leaves a visible crease
    at every one of them. On a 6K canvas those creases are centimetres apart
    and read as concentric rings around the comet.
    """
    stops = "".join(
        f'<stop offset="{i / 9:.3f}" stop-color="{color}" '
        f'stop-opacity="{op * (1 - i / 9) ** falloff:.4f}"/>'
        for i in range(10)
    )
    return (
        f'<radialGradient id="{gid}" gradientUnits="userSpaceOnUse" '
        f'cx="{cx:.1f}" cy="{cy:.1f}" r="{r:.1f}">{stops}</radialGradient>'
    )


def glow_rect(gid: str, w: float, h: float) -> str:
    return f'<rect width="{w:.0f}" height="{h:.0f}" fill="url(#{gid})"/>'


# Five plumes: the widest is nearly invisible and does the atmospheric spill,
# the narrowest is the hot line down the middle. Blurring one shape would give
# the same picture; this gives it without a per-pixel filter.
STREAK_PASSES = [
    (4.6, 0.030),
    (3.4, 0.042),
    (2.5, 0.058),
    (1.85, 0.080),
    (1.35, 0.115),
    (1.00, 0.175),
    (0.65, 0.300),
    (0.30, 0.850),
]

# How fast the plume closes as it approaches the head. A real comet is narrowest
# at its nucleus and flares away from it, so width runs the *opposite* way to
# brightness — which is the whole reason this is a tapered ribbon and not a
# stroke. A stroke has one width for its whole length, so stacking strokes
# stacks their edges, and the trail reads as concentric plastic tubing.
TAPER = 0.85

# Points sampled along the curve per ribbon edge. Enough that the outline is
# smooth at 6K; the cost is linear and the whole set renders in seconds.
RIBBON_STEPS = 96


def quad_tangent(t: float, p0, p1, p2) -> tuple[float, float]:
    return (
        2 * (1 - t) * (p1[0] - p0[0]) + 2 * t * (p2[0] - p1[0]),
        2 * (1 - t) * (p1[1] - p0[1]) + 2 * t * (p2[1] - p1[1]),
    )


def ribbon(pts: tuple, width: float, steps: int = RIBBON_STEPS) -> str:
    """A closed plume along the trail: widest at the tail, a point at the head.

    Walks the curve offsetting by the normal on one side, then walks back on
    the other. Because each layer already tapers, stacking them reads as a soft
    plume rather than as a set of nested outlines.
    """
    upper, lower = [], []
    for i in range(steps + 1):
        t = i / steps
        x, y = quad_point(t, *pts)
        tx, ty = quad_tangent(t, *pts)
        norm = math.hypot(tx, ty) or 1.0
        nx, ny = -ty / norm, tx / norm
        hw = width * (1.0 - t) ** TAPER
        upper.append(f"{x + nx * hw:.1f} {y + ny * hw:.1f}")
        lower.append(f"{x - nx * hw:.1f} {y - ny * hw:.1f}")
    return "M" + " L".join(upper) + " L" + " L".join(reversed(lower)) + " Z"


def streak(gid: str, pts: tuple, width: float, passes=STREAK_PASSES) -> str:
    """The trail: nested tapered plumes, wide and faint to thin and hot."""
    return "".join(
        f'<path d="{ribbon(pts, width * k)}" fill="url(#{gid})" opacity="{op:g}"/>'
        for k, op in passes
    )


def streak_gradient(gid: str, tail: tuple[float, float], head: tuple[float, float], g: Ground) -> str:
    """Transparent at the tail, hot at the head — the direction of travel.

    The gold stop sits at 62% rather than the midpoint because a linear ramp
    reads as a wedge, not as speed. Weighting the light toward the head is what
    makes the eye run along the curve in the right direction.
    """
    stops = (
        f'<stop offset="0" stop-color="{ck.BRAND}" stop-opacity="0"/>'
        f'<stop offset="0.30" stop-color="{ck.BRAND}" stop-opacity="0.18"/>'
        f'<stop offset="0.62" stop-color="{ck.BRAND}" stop-opacity="0.60"/>'
        f'<stop offset="0.88" stop-color="{g.streak_core}" stop-opacity="0.92"/>'
        f'<stop offset="1" stop-color="{g.streak_core}" stop-opacity="1"/>'
    )
    return (
        f'<linearGradient id="{gid}" gradientUnits="userSpaceOnUse" '
        f'x1="{tail[0]:.1f}" y1="{tail[1]:.1f}" '
        f'x2="{head[0]:.1f}" y2="{head[1]:.1f}">{stops}</linearGradient>'
    )


def quad_point(t: float, p0, p1, p2) -> tuple[float, float]:
    """A point on the trail, so debris can be placed *on* the curve."""
    u = 1 - t
    return (
        u * u * p0[0] + 2 * u * t * p1[0] + t * t * p2[0],
        u * u * p0[1] + 2 * u * t * p1[1] + t * t * p2[1],
    )


# ---------------------------------------------------------------------------
# the field
# ---------------------------------------------------------------------------

# Three depth planes. Far specks are small, dim and dense; near ones are large,
# bright and rare, and a few of those are the kit's own four-point star rather
# than a dot — which is what stops the field reading as generic noise.
# Counts are per *canvas*, not per megapixel, and radii are fractions of the
# diagonal. Both halves matter: the ladder's promise is that 4k, 5k and 6k are
# one picture at three sizes, and a density that scales with pixel area breaks
# it — the 6k file would carry fourteen times the specks of the preview and
# read as confetti where the 4k read as sky. Resolution changes sharpness here
# and nothing else.
PLANES = [
    # (count, radius as a fraction of the diagonal, opacity, star odds)
    (820, 0.00040, 0.50, 0.00),
    (250, 0.00080, 0.78, 0.05),
    (52, 0.00145, 1.00, 0.30),
]


def starfield(c: Canvas, g: Ground, keep_out: tuple[float, float, float]) -> str:
    """A seeded three-plane field, thinned inside the comet's own glow.

    `keep_out` is the head and a radius: specks inside it are dropped, because a
    star sitting on top of the bloom reads as a dust spot on the lens rather
    than as depth behind the subject.
    """
    rng = random.Random(f"stella/wallpaper/{c.form}/{g.name}/v3")
    kx, ky, kr = keep_out
    out = []
    for count, r_frac, op, star_odds in PLANES:
        radius = c.diag * r_frac
        for _ in range(count):
            x, y = rng.random() * c.w, rng.random() * c.h
            if math.hypot(x - kx, y - ky) < kr:
                continue
            # Fade toward the edges so the field has a centre of gravity.
            edge = min(x, c.w - x, y, c.h - y) / (min(c.w, c.h) * 0.5)
            a = op * g.speck_op * min(1.0, 0.35 + edge * 1.4) * (0.45 + rng.random() * 0.55)
            warm = rng.random() < 0.30
            fill = g.warm_speck if warm else g.speck
            if rng.random() < star_odds:
                out.append(ck.star(x, y, radius * 5.2, fill, round(a * 0.75, 3)))
            else:
                out.append(
                    f'<circle cx="{x:.1f}" cy="{y:.1f}" r="{radius * (0.6 + rng.random()):.2f}" '
                    f'fill="{fill}" opacity="{a:.3f}"/>'
                )
    return "".join(out)


def debris(c: Canvas, g: Ground, p: Plot, pts: tuple) -> str:
    """Sparks shed along the trail, dense at the head and gone by the tail.

    Placed by evaluating the curve rather than scattered near it, so they read
    as having come off the comet instead of floating beside it.
    """
    rng = random.Random(f"stella/debris/{c.form}/{g.name}/v3")
    short = min(c.w, c.h)
    out = []
    for i in range(140):
        # Cubed so the density piles up toward the head.
        t = 1.0 - (rng.random() ** 3) * 0.92
        x, y = quad_point(t, *pts)
        spread = short * 0.055 * (1.15 - t)
        x += rng.gauss(0, spread)
        y += rng.gauss(0, spread * 0.55)
        size = short * (0.0016 + rng.random() * 0.0042) * (0.45 + t)
        a = (0.14 + rng.random() * 0.55) * t**1.6
        if rng.random() < 0.30:
            out.append(ck.star(x, y, size * 3.6, g.streak_core, round(a * 0.8, 3)))
        else:
            out.append(
                f'<circle cx="{x:.1f}" cy="{y:.1f}" r="{size:.2f}" '
                f'fill="{ck.BRAND}" opacity="{a:.3f}"/>'
            )
    return "".join(out)


def grid(c: Canvas, g: Ground) -> str:
    """A faint terminal rule, fading out downward. Structure, not decoration."""
    step = min(c.w, c.h) / 22.0
    lines = []
    x = step
    while x < c.w:
        lines.append(f'<line x1="{x:.1f}" y1="0" x2="{x:.1f}" y2="{c.h}"/>')
        x += step
    y = step
    while y < c.h:
        lines.append(f'<line x1="0" y1="{y:.1f}" x2="{c.w}" y2="{y:.1f}"/>')
        y += step
    return (
        f'<g stroke="{g.grid}" stroke-width="{max(1.0, c.diag * 0.00016):.2f}" '
        f'opacity="{g.grid_op:g}" mask="url(#gridfade)">{"".join(lines)}</g>'
    )


# ---------------------------------------------------------------------------
# composition
# ---------------------------------------------------------------------------


def compose(c: Canvas, g: Ground) -> str:
    p = PLOTS[c.form]
    short = min(c.w, c.h)
    head = (p.head[0] * c.w, p.head[1] * c.h)
    tail = (p.tail[0] * c.w, p.tail[1] * c.h)
    bend = (p.bend[0] * c.w, p.bend[1] * c.h)
    mark_w = short * p.mark_frac
    bloom_r = mark_w * 2.5

    far_head = (p.far_head[0] * c.w, p.far_head[1] * c.h)
    far_tail = (p.far_tail[0] * c.w, p.far_tail[1] * c.h)
    # Straight, because at this distance a curve is not resolvable — the bend
    # sits on the chord.
    far_bend = ((far_head[0] + far_tail[0]) / 2, (far_head[1] + far_tail[1]) / 2)

    defs = [
        ck.grain_defs(),
        # The sky itself: a shallow ramp, warmer toward the comet's end of it.
        f'<linearGradient id="sky" gradientUnits="userSpaceOnUse" '
        f'x1="0" y1="0" x2="{c.w * 0.35:.0f}" y2="{c.h}">'
        f'<stop offset="0" stop-color="{g.sky_top}"/>'
        f'<stop offset="1" stop-color="{g.sky_bottom}"/></linearGradient>',
        # A broad off-centre wash so the ground is never one dead value.
        radial("haze", c.w * 0.62, c.h * 0.60, c.diag * 0.78, g.haze, g.haze_op, 1.6),
        # The light pooling around the head, and a tighter core inside it.
        radial("bloom", head[0], head[1], bloom_r, g.bloom, g.bloom_op, 2.4),
        radial("core", head[0], head[1], mark_w * 0.85, g.streak_core, 0.42, 2.8),
        # Corners fall away so the eye stays on the subject.
        radial("vig", c.w * 0.5, c.h * 0.5, c.diag * 0.62, g.sky_top, 0.0, 1.0),
        f'<radialGradient id="vignette" gradientUnits="userSpaceOnUse" '
        f'cx="{c.w * 0.5:.0f}" cy="{c.h * 0.5:.0f}" r="{c.diag * 0.60:.0f}">'
        f'<stop offset="0.45" stop-color="#000000" stop-opacity="0"/>'
        f'<stop offset="1" stop-color="#000000" stop-opacity="{g.vignette_op:g}"/>'
        f"</radialGradient>",
        streak_gradient("trail", tail, head, g),
        streak_gradient("fartrail", far_tail, far_head, g),
        radial("farbloom", far_head[0], far_head[1], mark_w * 0.9, g.bloom, g.bloom_op * 0.55, 2.4),
        # The grid is strongest where the trail is and gone by the bottom edge.
        f'<linearGradient id="gridramp" gradientUnits="userSpaceOnUse" '
        f'x1="0" y1="0" x2="0" y2="{c.h}">'
        f'<stop offset="0" stop-color="#FFFFFF" stop-opacity="0.75"/>'
        f'<stop offset="0.55" stop-color="#FFFFFF" stop-opacity="0.30"/>'
        f'<stop offset="1" stop-color="#FFFFFF" stop-opacity="0"/></linearGradient>',
        f'<mask id="gridfade"><rect width="{c.w}" height="{c.h}" '
        f'fill="url(#gridramp)"/></mask>',
    ]

    body = [
        f'<rect width="{c.w}" height="{c.h}" fill="url(#sky)"/>',
        glow_rect("haze", c.w, c.h),
        grid(c, g),
        starfield(c, g, (head[0], head[1], bloom_r * 0.62)),
        glow_rect("bloom", c.w, c.h),
        # The far comet goes down before the near one, so the near plume passes
        # in front of it — the only cue that says which of the two is closer.
        f'<g opacity="{0.55 if g.name == "dark" else 0.70:g}">'
        + streak("fartrail", (far_tail, far_bend, far_head), short * p.streak_frac * p.far_scale)
        + glow_rect("farbloom", c.w, c.h)
        + ck.star(far_head[0], far_head[1], mark_w * p.far_scale * 0.85, ck.BRAND)
        + "</g>",
        streak("trail", (tail, bend, head), short * p.streak_frac),
        debris(c, g, p, (tail, bend, head)),
        glow_rect("core", c.w, c.h),
        # The star alone, not the full comet: the plume *is* the trail here, so
        # the mark's own three bars would be a second trail pointing the same
        # way — read once as duplication and once as debris stuck to the
        # nucleus. The bottom lockup is where the whole mark gets its say.
        ck.star(head[0], head[1], mark_w * 0.62, ck.BRAND),
        # The name, small and quiet, near the bottom edge. A wallpaper that
        # shouts its own brand is a poster; this is meant to be lived on.
        ck.lockup(
            p.wordmark_at[0] * c.w,
            p.wordmark_at[1] * c.h,
            short * p.wordmark_frac * (ck.LOCKUP_H / ck.LOCKUP_W),
            g.speck,
        ),
        f'<rect width="{c.w}" height="{c.h}" fill="url(#vignette)"/>',
        ck.grain_rect(c.w, c.h),
    ]
    return ck.svg_doc(c.w, c.h, f"<defs>{''.join(defs)}</defs>{''.join(body)}")


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------


def out_dir(c: Canvas) -> Path:
    return HERE / c.form


def render(c: Canvas, g: Ground, keep_svg: bool = False) -> Path:
    out = out_dir(c) / f"{c.out_name}-{g.name}.png"
    return ck.rasterize(compose(c, g), c.w, c.h, out, keep_svg=keep_svg)


def check_ladder_parity() -> None:
    """4k, 5k and 6k of one form must be the same picture, only larger.

    Counting the drawn elements is what makes that checkable without comparing
    pixels: every tier derives its geometry from the canvas, so if the tiers
    agree on *how many* things are drawn, the only remaining difference is
    scale. This fails loudly on the bug it was written for — star density was
    once per megapixel, which put 3,800 specks on the 6k canvas and 900 on the
    4k, so the ladder shipped three different skies under one name.
    """
    for form in PLOTS:
        tiers = [c for c in CANVASES if c.form == form]
        for g in GROUNDS:
            counts = {c.tier: compose(c, g).count("<") for c in tiers}
            if len(set(counts.values())) != 1:
                raise SystemExit(
                    f"ladder drift: {form}/{g.name} draws a different number of "
                    f"elements per tier — {counts}.\n"
                    "Something in the composition scales with pixel area rather "
                    "than with the canvas."
                )


def check() -> int:
    check_ladder_parity()
    bad = 0
    for c in CANVASES:
        for g in GROUNDS:
            out = out_dir(c) / f"{c.out_name}-{g.name}.png"
            if not out.exists():
                print(f"missing: {out.relative_to(HERE)}")
                bad += 1
                continue
            ck.verify_png(out)
            if ck.png_size(out) != (c.w, c.h):
                print(f"wrong size: {out.relative_to(HERE)} is {ck.png_size(out)}")
                bad += 1
    return bad


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--check", action="store_true", help="validate only, write nothing")
    ap.add_argument("--keep-svg", action="store_true", help="leave the SVG beside each PNG")
    ap.add_argument(
        "--preview",
        action="store_true",
        help="render one small desktop+phone pair to /tmp for a look",
    )
    args = ap.parse_args()

    if args.check:
        bad = check()
        if bad:
            print(f"\n{bad} wallpaper(s) wrong — run the builder")
            return 1
        print(f"ok: {len(CANVASES) * len(GROUNDS)} wallpapers")
        return 0

    ck.require_renderer()

    if args.preview:
        import tempfile

        tmp = Path(tempfile.gettempdir()) / "stella-wallpaper-preview"
        tmp.mkdir(exist_ok=True)
        for form, w, h in (("desktop", 1600, 900), ("phone", 720, 1280)):
            for g in GROUNDS:
                c = Canvas(form, "preview", w, h)
                out = tmp / f"{c.out_name}-{g.name}.png"
                ck.rasterize(compose(c, g), w, h, out)
                print(out)
        return 0

    for c in CANVASES:
        for g in GROUNDS:
            print(f"  {render(c, g, args.keep_svg).relative_to(HERE)}")
    if check():
        raise SystemExit("build wrote something the check rejects")
    print(f"\n{len(CANVASES) * len(GROUNDS)} wallpapers")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
