#!/usr/bin/env python3
"""Render the spinner GIFs from the spinner SVGs — one motion, two formats.

The animated SVGs beside this script are the source of truth for the build-up:
which bar arrives when, how the star overshoots, where the cursor stops. The
GIFs exist only for surfaces that cannot run CSS (GitHub README tables, chat
clients, slide decks), and they have to be the *same* animation or the kit is
telling two stories.

That is why this reads the keyframes out of the SVG instead of restating them.
librsvg does not evaluate CSS animation — it renders the initial state — so a
GIF has to be built frame by frame, and the tempting shortcut is to reimplement
the timeline in Python. The kit already paid for a version of that mistake:
commit 10781aa31 recoloured the four spinner SVGs to Bronze Gold and left the
four GIFs untouched, so the kit once shipped `#EFC53F` vector art beside `#EFC53F`
animations for as long as nobody opened them side by side. Anything restated is
something that can drift; here the SVG is parsed, evaluated, and re-emitted per
frame with the geometry untouched.

    python3 docs/brand/spinners/build_spinners.py          # write the GIFs
    python3 docs/brand/spinners/build_spinners.py --check   # validate only

Requires rsvg-convert (`brew install librsvg`) and ffmpeg (`brew install
ffmpeg`) for palette generation and GIF encoding.
"""

from __future__ import annotations

import argparse
import re
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))

import cometkit as ck  # noqa: E402  (path shim must precede the import)

FPS = 25  # 4 centiseconds per frame — GIF delays are integral hundredths


# ---------------------------------------------------------------------------
# the CSS subset
# ---------------------------------------------------------------------------
#
# Deliberately narrow: `animation` shorthand with a name, a duration in
# seconds, `linear` or `steps(1, end)`, and `infinite`; `@keyframes` whose
# selectors are comma-separated percentages; and the three properties the kit's
# spinners animate — `opacity`, `transform` (translateX / scale), and a
# per-keyframe `animation-timing-function: cubic-bezier(...)`. Anything outside
# that raises rather than being silently ignored, because a silently dropped
# rule is a GIF that no longer matches its SVG.

_ANIM = re.compile(
    r"\.([\w-]+)\s*\{[^}]*animation:\s*([\w-]+)\s+([\d.]+)s\s+"
    r"(linear|steps\(1\s*,\s*end\))\s+infinite"
)
_KEYFRAMES_HEAD = re.compile(r"@keyframes\s+([\w-]+)\s*\{")
_BLOCK = re.compile(r"([\d.%,\s]+?)\{([^}]*)\}")
_BEZIER = re.compile(r"cubic-bezier\(\s*([-\d.]+)\s*,\s*([-\d.]+)\s*,\s*([-\d.]+)\s*,\s*([-\d.]+)\s*\)")
# The unit is optional: CSS lets a zero length drop it, and the kit's keyframes
# use both `translateX(0)` and `translateX(0px)`.
_TRANSLATE_X = re.compile(r"translateX\(\s*([-\d.]+)(?:px)?\s*\)")
_SCALE = re.compile(r"scale\(\s*([-\d.]+)\s*\)")


def bezier(x1: float, y1: float, x2: float, y2: float):
    """A CSS cubic-bezier easing as a callable, solved by bisection.

    Bisection rather than Newton on purpose: `cubic-bezier(.34,1.56,.64,1)` —
    the star's overshoot — has a derivative that changes sign, and Newton walks
    off it. Forty iterations is exact to well under a frame.
    """

    def curve(a: float, b: float, t: float) -> float:
        u = 1 - t
        return 3 * u * u * t * a + 3 * u * t * t * b + t * t * t

    def ease(x: float) -> float:
        if x <= 0:
            return 0.0
        if x >= 1:
            return 1.0
        lo, hi = 0.0, 1.0
        for _ in range(40):
            mid = (lo + hi) / 2
            if curve(x1, x2, mid) < x:
                lo = mid
            else:
                hi = mid
        return curve(y1, y2, (lo + hi) / 2)

    return ease


LINEAR = bezier(0.0, 0.0, 1.0, 1.0)


@dataclass
class Keyframe:
    offset: float  # 0..1
    props: dict[str, float] = field(default_factory=dict)
    easing: object = None  # timing function for the interval starting here


@dataclass
class Animation:
    name: str
    duration: float
    stepped: bool  # steps(1, end): hold the from-value for the whole interval
    frames: list[Keyframe]

    def value(self, prop: str, t: float, default: float) -> float:
        """`prop` at cycle fraction `t`, interpolated the way CSS would.

        Each property brackets against only the keyframes that declare it —
        a keyframe that sets `opacity` and nothing else must not pin
        `transform` to its neighbour's value.
        """
        pts = [k for k in self.frames if prop in k.props]
        if not pts:
            return default
        if t <= pts[0].offset:
            return pts[0].props[prop]
        for lo, hi in zip(pts, pts[1:]):
            if lo.offset <= t <= hi.offset:
                if self.stepped or hi.offset == lo.offset:
                    return lo.props[prop]
                span = (t - lo.offset) / (hi.offset - lo.offset)
                e = (lo.easing or LINEAR)(span)
                return lo.props[prop] + (hi.props[prop] - lo.props[prop]) * e
        return pts[-1].props[prop]


def iter_keyframes(css: str):
    """Yield `(name, body)` for each `@keyframes`, matching braces properly.

    A regex cannot do this: a keyframes body is itself a sequence of braced
    blocks, so a non-greedy `\\{(.*?)\\}` stops at the end of the *first*
    selector rather than the rule. That failure is silent — the rule parses,
    just short — which is why this counts depth instead.
    """
    for head in _KEYFRAMES_HEAD.finditer(css):
        depth, i = 1, head.end()
        while i < len(css) and depth:
            depth += (css[i] == "{") - (css[i] == "}")
            i += 1
        if depth:
            raise SystemExit(f"@keyframes {head.group(1)}: unbalanced braces")
        yield head.group(1), css[head.end() : i - 1]


def parse_style(css: str) -> dict[str, Animation]:
    """Map each animated class name to its timeline."""
    # The reduced-motion block turns everything off; it is a rendering hint for
    # a browser and says nothing about the GIF, so it never reaches the parser.
    css = re.sub(r"@media[^{]*\{(?:[^{}]*\{[^}]*\})*[^}]*\}", "", css)

    keyframes: dict[str, list[Keyframe]] = {}
    for name, body in iter_keyframes(css):
        frames: dict[float, Keyframe] = {}
        for selectors, decls in _BLOCK.findall(body):
            props: dict[str, float] = {}
            easing = None
            for decl in decls.split(";"):
                decl = decl.strip()
                if not decl:
                    continue
                key, _, raw = (p.strip() for p in decl.partition(":"))
                if key == "opacity":
                    props["opacity"] = float(raw)
                elif key == "transform":
                    if m := _TRANSLATE_X.search(raw):
                        props["translateX"] = float(m.group(1))
                    if m := _SCALE.search(raw):
                        props["scale"] = float(m.group(1))
                    if not _TRANSLATE_X.search(raw) and not _SCALE.search(raw):
                        raise SystemExit(f"unsupported transform in @keyframes {name}: {raw!r}")
                elif key == "animation-timing-function":
                    m = _BEZIER.search(raw)
                    if not m:
                        raise SystemExit(f"unsupported timing function in {name}: {raw!r}")
                    easing = bezier(*(float(g) for g in m.groups()))
                elif key in ("transform-box", "transform-origin"):
                    pass
                else:
                    raise SystemExit(f"unsupported property in @keyframes {name}: {key!r}")
            for sel in selectors.split(","):
                sel = sel.strip()
                if not sel.endswith("%"):
                    continue
                off = float(sel[:-1]) / 100.0
                # A repeated offset (the cursor blinks through 58% twice) merges
                # rather than replaces, so the later declaration wins per key.
                slot = frames.setdefault(off, Keyframe(off))
                slot.props.update(props)
                if easing is not None:
                    slot.easing = easing
        keyframes[name] = sorted(frames.values(), key=lambda k: k.offset)

    out: dict[str, Animation] = {}
    for cls, name, dur, timing in _ANIM.findall(css):
        if name not in keyframes:
            raise SystemExit(f".{cls} names @keyframes {name}, which does not exist")
        out[cls] = Animation(name, float(dur), timing.startswith("steps"), keyframes[name])
    return out


# ---------------------------------------------------------------------------
# frame emission
# ---------------------------------------------------------------------------

_STYLE = re.compile(r"<style>.*?</style>", re.S)
_TAG_WITH_CLASS = re.compile(r"<(\w+)\b([^>]*?)\bclass=\"([^\"]+)\"([^>]*?)(/?)>")

# The only element in the kit that scales about its own box is the star, and
# its box is a known constant — `cometkit.STAR_CX/CY`. Resolving `fill-box`
# generally would mean computing a path bounding box, which is a renderer's job
# and not worth reimplementing for one shape.
FILL_BOX_CENTER = (ck.STAR_CX, ck.STAR_CY)


def frame_svg(src: str, anims: dict[str, Animation], t: float, ground: str) -> str:
    """The source SVG with its animation evaluated at cycle fraction `t`.

    Geometry is never touched: the original tags are re-emitted with `opacity`
    and `transform` attributes computed for this instant. That is what keeps the
    GIF honest — the shapes in it are literally the shapes in the SVG.
    """

    def sub(m: re.Match) -> str:
        tag, before, classes, after, close = m.groups()
        opacity, tx, scale = 1.0, 0.0, 1.0
        animated = False
        for cls in classes.split():
            if (a := anims.get(cls)) is None:
                continue
            animated = True
            opacity *= a.value("opacity", t, 1.0)
            tx += a.value("translateX", t, 0.0)
            scale *= a.value("scale", t, 1.0)
        if not animated:
            return m.group(0)
        # The star's easing overshoots past 1 on purpose — that is the pop in
        # the build-up — and the same curve drives its opacity, which must not
        # overshoot. A browser clamps `opacity`; librsvg is stricter, so it is
        # clamped here rather than relied upon.
        opacity = min(1.0, max(0.0, opacity))
        attrs = f' opacity="{opacity:.4f}"'
        parts = []
        if abs(tx) > 1e-6:
            parts.append(f"translate({tx:.3f} 0)")
        if abs(scale - 1.0) > 1e-6:
            cx, cy = FILL_BOX_CENTER
            parts.append(f"translate({cx} {cy}) scale({scale:.4f}) translate({-cx} {-cy})")
        if parts:
            attrs += f' transform="{" ".join(parts)}"'
        return f"<{tag}{before}{after}{attrs}{close}>"

    body = _TAG_WITH_CLASS.sub(sub, _STYLE.sub("", src))
    # GIF alpha is one bit, so the ground is painted in rather than left clear.
    return body.replace(">", f'><rect width="100%" height="100%" fill="{ground}"/>', 1)


# ---------------------------------------------------------------------------
# the spinners
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Spinner:
    out_stem: str
    src_name: str
    width: int
    height: int
    ground: str

    @property
    def src(self) -> Path:
        return HERE / self.src_name

    @property
    def out(self) -> Path:
        return HERE / f"{self.out_stem}.gif"


# Sizes match what the kit already shipped, so a README embedding one of these
# by width does not reflow when it is replaced.
SPINNERS = [
    Spinner("spinner-logomark-dark", "spinner-logomark.svg", 480, 480, ck.INK),
    Spinner("spinner-logomark-light", "spinner-logomark.svg", 480, 480, ck.PAPER_BG),
    Spinner("spinner-lockup-dark", "spinner-lockup-dark.svg", 704, 192, ck.INK),
    Spinner("spinner-lockup-light", "spinner-lockup-light.svg", 704, 192, ck.PAPER_BG),
]


def require_ffmpeg() -> None:
    if shutil.which("ffmpeg") is None:
        raise SystemExit("ffmpeg not found — install it with `brew install ffmpeg`.")


def build(sp: Spinner) -> Path:
    src = sp.src.read_text(encoding="utf-8")
    css = _STYLE.search(src)
    if css is None:
        raise SystemExit(f"{sp.src.name}: no <style> block — nothing to animate")
    anims = parse_style(css.group(0))
    duration = max(a.duration for a in anims.values())
    if len({a.duration for a in anims.values()}) != 1:
        raise SystemExit(f"{sp.src.name}: animations disagree on duration — the loop will drift")

    count = round(duration * FPS)
    with tempfile.TemporaryDirectory() as tmp:
        tmpdir = Path(tmp)
        for i in range(count):
            markup = frame_svg(src, anims, i / count, sp.ground)
            ck.rasterize(markup, sp.width, sp.height, tmpdir / f"f{i:04d}.png")
        pattern = str(tmpdir / "f%04d.png")
        palette = tmpdir / "palette.png"
        subprocess.run(
            ["ffmpeg", "-v", "error", "-y", "-i", pattern,
             "-vf", "palettegen=stats_mode=diff", str(palette)],
            check=True,
        )
        subprocess.run(
            ["ffmpeg", "-v", "error", "-y", "-framerate", str(FPS), "-i", pattern,
             "-i", str(palette),
             "-lavfi", "paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle",
             "-loop", "0", str(sp.out)],
            check=True,
        )
    return sp.out


def check() -> int:
    bad = 0
    for sp in SPINNERS:
        if not sp.src.exists():
            print(f"missing source: {sp.src.name}")
            bad += 1
            continue
        if not sp.out.exists():
            print(f"missing: {sp.out.name}")
            bad += 1
            continue
        raw = sp.out.read_bytes()
        if raw[:6] not in (b"GIF89a", b"GIF87a"):
            print(f"{sp.out.name}: not a GIF")
            bad += 1
            continue
        w = int.from_bytes(raw[6:8], "little")
        h = int.from_bytes(raw[8:10], "little")
        if (w, h) != (sp.width, sp.height):
            print(f"{sp.out.name}: is {w}x{h}, want {sp.width}x{sp.height}")
            bad += 1
        # The defect this whole script exists for: a GIF whose gold is not the
        # SVG's gold. Any table in the file carrying the old amber fails here.
        if not gold_matches(raw):
            print(f"{sp.out.name}: carries no colour near {ck.BRAND} — stale palette?")
            bad += 1
    return bad


def gold_matches(raw: bytes, tol: int = 26) -> bool:
    """True if some colour table entry is close to Bronze Gold.

    Close rather than exact because the GIF is palettised and dithered, so the
    quantiser is free to move the swatch a little. The old amber `#EFC53F` is
    58 away in the red-blue difference alone, which no tolerance this size
    admits — which is precisely the drift this has to catch.
    """
    want = (0xC5, 0x8A, 0x32)
    for i in range(13, len(raw) - 3, 1):
        px = raw[i : i + 3]
        if len(px) == 3 and all(abs(px[k] - want[k]) <= tol for k in range(3)):
            return True
    return False


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--check", action="store_true", help="validate only, write nothing")
    args = ap.parse_args()

    if args.check:
        bad = check()
        if bad:
            print(f"\n{bad} spinner(s) wrong — run the builder")
            return 1
        print(f"ok: {len(SPINNERS)} spinners")
        return 0

    ck.require_renderer()
    require_ffmpeg()
    for sp in SPINNERS:
        print(f"  {build(sp).name}")
    if check():
        raise SystemExit("build wrote something the check rejects")
    print(f"\n{len(SPINNERS)} spinners")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
