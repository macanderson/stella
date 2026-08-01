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
