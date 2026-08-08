---
id: brand
title: "stella\* — brand kit v2.0"
status: living
---

# stella\* — brand kit v2.0

The comet: a four-point star moving fast enough to leave a trail.
One shape, one color — Nebula Violet `#7C5CFF` on Ink `#080B1C`.

**Start with `brand-guidelines.html`** — it explains everything below,
adapts to light/dark, and works offline.

```
logo/svg/          logomark · lockup · wordmark — color/mono × dark/light,
                   plus *-adaptive.svg (auto light/dark via media query).
                   All text outlined; no fonts needed.
logo/png/          transparent exports of every fixed variant (256–2048px)
spinners/          build-up spinners — animated SVG (CSS, reduced-motion
                   safe) + GIF in dark/light
wallpapers/        desktop + phone · 4K/5K/6K · dark + light
social/            avatar 1024 · LinkedIn 1584×396 · X 1500×500 ·
                   YouTube 2560×1440 (safe-area aware) · OG 1200×630
                   dark + light, all generated — see "social art" below
pwa/               favicon.ico + png favicons · apple-touch · 192/512 +
                   maskable · safari-pinned-tab.svg · manifest.webmanifest ·
                   head-snippet.html
css/globals.css    shadcn/ui base for Tailwind v4 (oklch, light + .dark)
css/tokens.css     framework-free brand tokens: ramps, type, motion, texture
fonts/             JetBrains Mono woff2 (400/500/700/800) + OFL license
brand-tokens.toml  color source for `pnpm generate-assets` — recolors
                   pwa/, wallpapers/, and spinners/*.gif — see below
```

Quick rules: lowercase always. Comet flies left→right. Gold is the signal,
never the surface. On light backgrounds use gold-deep `#5133BE` for small
gold text. Assemble, don't spin.

```
site/              one-page desktop mock + mobile mock with the star-fan
                   thumb nav (both self-contained, adaptive light/dark)
prompts/           paste-ready prompts: website design system (Claude design
                   feature) + TUI restyle (coding agent)
```

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

## generated assets — pwa/, wallpapers/, spinners/\*.gif

Unlike `social/`, these three families had no committed generator (#2224):
a palette change meant hand-editing PNG/GIF pixels. `brand-tokens.toml` +
`pnpm generate-assets` fixes that:

```
brew install oxipng           # lossless PNG re-encoder (MIT) — see below
cd docs/brand
pnpm install
pnpm generate-assets          # recolor pwa/, wallpapers/, spinners/*.gif
                               # + their website/ mirrors, in place
pnpm check-assets             # verify committed files match the tokens;
                               # exits 1 (no writes) if anything's stale
```

Needs `oxipng` (`brew install oxipng`) the same way `social/`'s generator
needs `rsvg-convert`: pngjs (the npm PNG codec this pipeline decodes and
recolors pixels with) has no optimizing encoder of its own — its default
writer runs ~3x larger than the committed files on this kit's art — and the
usual npm fix, `sharp`, bundles a `libvips` binary built `LGPL-3.0-or-later`,
which fails this repo's (AGPL/commercial dual-licensed) dependency-review
gate. `oxipng` is MIT-licensed and re-encodes losslessly over stdin/stdout,
so it's an external tool dependency instead — see `scripts/lib/png.mjs`'s
module doc.

Edit `brand-tokens.toml` — the nebula gradient (violet→cyan) plus a dark and
a light `{bg, fg}` — and re-run. Everything else about these files is
untouched: **this recolors, it does not recompose.** None of the three
families has a committed vector/composition source (glow, starfield, grid,
animation timing were never checked in — only their rendered pixels were),
so `scripts/generate-assets.mjs` decomposes each committed pixel into a
brand-token basis (`scripts/lib/recolor.mjs` has the how and why) and
re-renders it under the new tokens at the exact same position, opacity, and
frame. Geometry, padding, the maskable safe-zone inset, and every spinner's
animation timing are never read from `brand-tokens.toml` and never change —
only fills do. `spinners/*.svg` are real committed sources, so those recolor
by exact hex substitution instead of decomposition.

Two things this does **not** cover, both inherited from the same root cause
(no committed source for these families) rather than fixed by it — see
#2224 for the open half:

- **The comet mark itself is fixed.** `logo/svg/*.svg` (and the
  `website/public/brand/*.svg` mirror `website/src/components/brand.tsx`
  depends on) are never touched — per this file's second line, Nebula Violet
  on Void *is* the identity, not a themeable default.
- **Geometry still can't change.** A new safe-zone inset, a different
  starfield density, a longer spinner reveal — none of that is possible
  without first authoring real composition sources for wallpapers/PWA icons,
  which `pnpm generate-assets` deliberately doesn't attempt: a from-scratch
  recomposition could silently drift from the committed art with no source
  to diff it against, which is worse than leaving the gap open.
