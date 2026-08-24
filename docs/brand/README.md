---
id: brand
title: "stella\* — brand kit v5.0"
status: living
---

# stella\* — brand kit v5.0

The comet: a four-point star moving fast enough to leave a trail.
One shape, one color — Gold `#EFC53F` on `#0A0A0C`.

**Start with `brand-guidelines.html`** — it explains everything below,
adapts to light/dark, and works offline.

```
logo/svg/          logomark · lockup · wordmark — color/mono × dark/light,
                   plus *-adaptive.svg (auto light/dark via media query).
                   All text outlined; no fonts needed.
logo/png/          transparent exports of every fixed variant (256–2048px)
spinners/          build-up spinners — animated SVG (CSS, reduced-motion
                   safe) + GIF in dark/light, the GIF rendered from the SVG
wallpapers/        the comet on its trajectory — desktop + phone · 4K/5K/6K ·
                   dark + light, all generated
social/            avatar 1024 · LinkedIn 1584×396 · X 1500×500 ·
                   YouTube 2560×1440 (safe-area aware) · OG 1200×630
                   dark + light, all generated — see "social art" below
pwa/               favicon.ico + png favicons · apple-touch · 192/512 +
                   maskable · safari-pinned-tab.svg · manifest.webmanifest ·
                   head-snippet.html
css/globals.css    shadcn/ui base for Tailwind v4 (oklch, light + .dark)
css/tokens.css     framework-free brand tokens: ramps, type, motion, texture
fonts/             JetBrains Mono woff2 (400/500/700/800) + OFL license
cometkit.py        the one copy of the geometry, palette, and rasteriser
```

## every pixel here is generated

**No raster file in this kit may be edited by hand.** Each one has a builder,
each builder takes `--check`, and the SVGs and `cometkit.py` are the only
sources:

```
python3 docs/brand/build_marks.py              # logo/png/ + pwa/ + favicon.ico
python3 docs/brand/wallpapers/build_wallpapers.py
python3 docs/brand/spinners/build_spinners.py  # GIFs, from the animated SVGs
python3 docs/brand/social/build_social.py      # see "social art" below
```

The website carries byte-copies of the logo SVGs and PWA icons (plus an RGBA
re-encode of `favicon.ico`), and that mirror is generated too: `make
brand-sync` (or `build_marks.py --sync-site`) produces it, and
`website/src/lib/brand-parity.test.ts` fails `pnpm test` on any drift. A
recolour therefore ends with the sync, never with hand-run `cp` (#3983).

All four need `rsvg-convert` (`brew install librsvg`); the spinners also need
`ffmpeg`, and the social art needs JetBrains Mono installed as a desktop font.

This exists because of what happened without it. `logo/png/`, `pwa/` and
`wallpapers/` had no builder at all, so regenerating them meant reaching for
whatever renderer was to hand — and commit 10781aa31 reached for a broken one,
committing 52 PNGs of torn scanlines and channel-separated garbage to the
remote. Nothing caught it, because nothing was watching: there was no `--check`
to fail and no reviewer opens fifty binaries. The same commit recoloured the
four spinner *SVGs* to the then-current gold and left their *GIFs* on the
previous one,
which is the quieter half of the same failure — an asset with no builder does
not get rebuilt when its source moves.

`--check` deliberately does **not** compare bytes. librsvg's output shifts
between releases, so a byte comparison would go red on a Homebrew upgrade while
the art was still correct, and a guard that cries wolf gets ignored. Each one
asserts what a broken build actually violates instead: that every file exists,
decodes, inflates, and has the size its name promises; that `cometkit`'s
geometry still matches the committed SVGs; that the three wallpaper tiers draw
the same picture; and that the spinner GIFs carry the current brand hue.

Quick rules: lowercase always. Comet flies left→right. Gold is the signal,
never the surface. On light backgrounds small brand text is **ink** `#141413`,
never gold — gold on paper measures 1.65:1. The *mark* stays full-strength gold
on both grounds: v3.0 and v4.0 each stepped it down a darker stop to clear a
3:1 graphical floor, and v5.0
keeps it: gold is better at 2.63:1 but still under the 3:1 graphical floor,
which brand-700 clears at 4.99:1. Assemble, don't spin.

```
site/              one-page desktop mock + mobile mock with the star-fan
                   thumb nav (both self-contained, adaptive light/dark)
prompts/           paste-ready prompts: website design system (Claude design
                   feature) + TUI restyle (coding agent)
```

## wallpapers

The kit's one sentence about its mark is "a four-point star moving fast enough
to leave a trail". The wallpapers are that sentence: a tapered plume entering
off-canvas, debris thinning along it, the comet at the head with the light
pooling around it, and a second comet far off for scale. Desktop runs the trail
left to right and climbing, so the top-right stays quiet for icons; phone drops
it steeply and parks the comet in the upper third, clear of the lock clock and
above a calm bottom half.

Three things about them are deliberate:

- **Nothing is filtered.** librsvg evaluates `feGaussianBlur` per pixel and the
  6K canvas makes that minutes per file, so every glow is a `radialGradient`
  and every soft edge is nested tapered plumes at falling opacity. A stroke
  cannot do this — one stroke has one width for its whole length, so stacking
  strokes stacks their edges and the trail reads as concentric plastic tubing.
- **Every gradient is dithered.** A wide soft ramp across near-black is the one
  thing 8 bits cannot hold; at 6K each posterised step is centimetres wide.
  The kit's seeded grain goes over the whole canvas to break them up.
- **The ladder is one picture at three sizes.** Star density is per canvas, not
  per megapixel — it was per megapixel once, which put 3,800 specks on the 6K
  file and 900 on the 4K, so the ladder shipped three different skies under one
  name. `--check` counts drawn elements per tier and fails if they diverge.

## social art

Every banner carries the same four things: the lockup, the tagline, the
Homebrew line in a terminal, and a row naming `macanderson/stella` and asking
for a star. Edit them by editing their source, not the pixels:

```
python3 docs/brand/social/build_social.py            # rewrite every PNG
python3 docs/brand/social/build_social.py --check    # validate, write nothing
python3 docs/brand/social/build_social.py --keep-svg # leave the SVG too
```

Needs `rsvg-convert` (`brew install librsvg`) and JetBrains Mono installed as a
desktop font — librsvg reads fontconfig, so the woff2s in `fonts/` are for the
web only. The starfield is seeded, so a re-run reproduces the committed PNGs.

Three things about that art are deliberate:

- **The terminal is a picture.** Nothing on a profile banner copies to a
  clipboard, so the install line is drawn to be *read and retyped*. It is the
  widest element on every canvas, and the box is sized from the string rather
  than the string trusted to fit. `--check` fails if it stops matching the
  command in the repo's own `README.md`.
- **The avatar carries no words.** It renders about 48px wide in a timeline,
  where a repo slug is mud. It stays the comet alone; the slug does its work on
  the banner beside it.
- **The website renders its own OG card.** `website/src/app/opengraph-image.tsx`
  is the same composition through next/og, and it ships no binary. Changing the
  card here usually means changing it there too.
