# Stella Brand Guidelines — the Laika system

Stella's identity is **Laika, the astronaut pup**: a floppy-eared dog in a
space helmet, drawn in warm cosmic sunset color on deep space plum. The name
nods to the first dog in orbit; the star (stella) she's chasing sits in the
wordmark and on her helmet glass.

Every asset in this directory is **generated** — edit
[`build.mjs`](build.mjs) and re-run it; never hand-edit the SVGs or PNGs:

```sh
node docs/brand/build.mjs            # full build (SVG + PNG, needs rsvg-convert)
node docs/brand/build.mjs --svg-only # fast iteration
```

## The mark

Laika's head inside her helmet: a gradient ring (the helmet), cream face with
a caramel eye patch and floppy ears, a gold antenna bead, and a four-point
gold star drifting across the glass. On light grounds the helmet interior
stays deep-space plum — a porthole into space. In the mono variants the star
is punched *through* the helmet ring as a knockout at 1:30.

## Variants — every family ships six

| Suffix | What it is |
| --- | --- |
| `-adaptive.svg` | Full-color; flips light↔dark automatically via `prefers-color-scheme`. Default for docs/web embeds. |
| `-light.svg` / `-dark.svg` | Non-adaptive color variants for grounds you control. |
| `-mono-light.svg` / `-mono-dark.svg` | Single-color line art (ink / milk) for engraving, embossing, single-color print, disabled states. |
| `-mono-adaptive.svg` | Mono line art that flips ink↔milk with the OS theme. |

Adaptive SVGs carry both concrete presentation attributes (the light state)
and a `<style>` block whose `@media (prefers-color-scheme: dark)` rules
override them. Rasterizers that ignore CSS (librsvg) therefore render the
light state — **PNGs are only cut from the non-adaptive variants**, into
`png/` mirroring the SVG tree, always with transparent backgrounds (except
app icons, splash screens, wallpapers and the nebula wash, which own their
grounds and ship light/dark only).

## Directory map

| Path | Contents |
| --- | --- |
| `marks/` | The Laika mark, 6 variants + PNGs at 1024/512/256. |
| `wordmarks/` | Monoline rounded `stella` + gold star, 6 variants. |
| `lockups/` | Horizontal (mark + wordmark) and stacked lockups, 6 variants. |
| `icons/` | `favicon-*` (simplified mark), `appicon-*` (rounded tile), `maskable-*` (full-bleed PWA icon, safe-zone compliant), `glyphs/` (24×24 UI icon set: star, sparkle, orbit, rocket, planet, paw, bone, helmet). |
| `loader/` | Animated SVG loader — Laika assembles herself: helmet ring draws on, ears/head/patch/eyes/nose/mouth pop in, star sweeps in; loops. |
| `splash/` | PWA loading screens, portrait 1320×2868 and landscape 2880×1800. |
| `wallpapers/` | Desktop (16:9, PNG at 5120×2880) and phone (2880×6240), light + dark. |
| `textures/` | 512-box overlay patterns: `starfield`, `constellation`, `paws`, `grain` (transparent, 6 variants) and the opaque `nebula` wash. |
| `tokens.css` / `tokens.json` | The palette as CSS custom properties / JSON design tokens. |
| `legacy/` | Retired systems: ember-gold (2025) and `aurora/` (cyan-on-navy, retired 2026-07). |

## Palette — warm cosmic sunset

No ice blue anywhere. The signature **nebula gradient** runs
`corona → flare → orchid` at 135°.

| Token | Hex | Role |
| --- | --- | --- |
| `corona` | `#FF6D4D` | Coral orange — gradient start, energy accents. |
| `flare` | `#F5487F` | Warm pink — gradient middle. |
| `orchid` | `#A24BEA` | Violet — gradient end, interactive accents. |
| `starlight` | `#FFC24D` | Gold — stars, the antenna bead, highlights. |
| `caramel` | `#DE8F55` | Laika's ears and eye patch. |
| `cream` | `#F9EDDC` | Laika's face. |
| `ink` | `#2A1A35` | Warm plum-black — text/line art on light grounds. |
| `milk` | `#FFF6E9` | Warm white — text/line art on dark grounds. |

### Grounds

| Token | Hex | Role |
| --- | --- | --- |
| `void900` | `#0E0916` | Dark mode — deepest ground. |
| `void800` | `#171021` | Dark mode — base ground. |
| `void700` | `#241833` | Dark mode — raised surfaces, helmet interior. |
| `cream50` | `#FFFAF0` | Light mode — highest ground. |
| `cream100` | `#F9F0E1` | Light mode — base ground. |
| `cream200` | `#EFE2CC` | Light mode — sunken surfaces. |

## Typography

No webfonts required for product surfaces (system-native stacks stay), but
the wordmark itself is drawn geometry — monoline rounded strokes, not a font.

- **Sans:** `-apple-system, BlinkMacSystemFont, "Segoe UI", Inter, Arial, sans-serif`
- **Mono (code, terminal):** `ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace`

## Usage rules

- Clear space around the mark: at least the helmet-ring stroke width (7/120
  of mark height) on all sides.
- Don't recolor Laika, tilt her more than the wallpapers' −6°, or separate
  the gold star from the wordmark.
- Mono variants are for single-color contexts only — never use them where
  color is available.
- The nebula gradient is for the helmet ring, progress fills, and hero
  washes — not body text.

## Scope note

This directory is the brand source of truth. Product surfaces (TUI theme,
docs site CSS, Observatory, `stella-docs/public/brand/` copies) still carry
the aurora palette and migrate in follow-up changes; until then
`stella-tui/src/theme.rs` remains authoritative for the TUI, including its
regression test pinning the *2025 ember* hexes (none of which this palette
reuses).
