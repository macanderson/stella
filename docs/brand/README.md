---
id: brand
title: "stella\* — brand kit v1.0"
status: living
---

# stella\* — brand kit v1.0

The comet: a four-point star moving fast enough to leave a trail.
One shape, one color — Phosphor Gold `#FFB000` on Ink `#0B0B0C`.

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
```

Quick rules: lowercase always. Comet flies left→right. Gold is the signal,
never the surface. On light backgrounds use gold-deep `#A37200` for small
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
