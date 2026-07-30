# stella — brand kit

The prompt identity: `›stella▮` set in Geist Mono Bold, chevron in electric
blue, block cursor in gold.

**[`BRAND.md`](BRAND.md) is normative.** It holds the full colour system —
ground, brand, gold, text, status, data marks — plus the type and voice rules.
Everything in this directory is a rendering of those values. If a value here
disagrees with BRAND.md, BRAND.md wins and the asset is a bug.

## Logo

Three lockups — `lockup` / `wordmark` / `glyph`. There is no TLD in this
identity, so `wordmark` doubles as the compact form.

Each ships as `-adaptive` (recolours via `prefers-color-scheme`), `-light`,
`-dark`, `-mono` (`currentColor`), `-mono-black`, `-mono-white`. The three mono
variants are single-colour by definition and must stay that way.

| Part | Light | Dark |
| --- | --- | --- |
| `ink` (glyph, wordmark) | `#0A0F1A` | `#F2F5FA` |
| `sub` | `#525C6E` | `#8E97A8` |
| `chev` (chevron) | `#1550C8` | `#5AA0FF` |
| `cur` (cursor block) | `#B5831F` | `#F5C145` |

The cursor block carries two golds because one cannot hold its edge against
both white and deep space. Never recolour it outside the gold family, and never
place the mark on a ground between `#404050` and `#9090A0`, where neither
variant separates.

## Files

| Path | What |
| --- | --- |
| `logos/svg/` | The 18 lockup × variant SVGs. Hand-written; the `-adaptive` ones carry an inline `<style>` block with a `prefers-color-scheme: dark` override. |
| `logos/png/` | Transparent @3x rasters of the `-light` / `-dark` SVGs. |
| `icons/` | `favicon.svg` plus the PWA / apple-touch PNG sizes. |
| `spinner/` | SMIL loop typing `stella`, cursor blinking at rest over 3.6s. Works as an `<img>`, no JS. `demo.html` renders all three on both grounds. |
| `tokens/` | `stella-colors.css` (`--stella-*` custom properties) and `stella-tokens.json` — the **logo** tokens only (ink / sub / chev / cur / paper, light and dark) plus the cursor geometry. The full colour system is in BRAND.md, not here. |
| `wallpapers/` | Desktop + phone × 4K/5K/6K × light/dark. |
| `extras/` | OG cards, 1200×630. |
| `manifest.webmanifest` | Drop-in PWA manifest pointing at `icons/`. |

There is no build step. Every file in this directory is committed source or a
committed export; nothing here is generated at install or CI time.

## Drop-in

```html
<link rel="icon" href="/icons/favicon.svg" type="image/svg+xml">
<link rel="apple-touch-icon" href="/icons/apple-touch-180.png">
<link rel="manifest" href="/manifest.webmanifest">
<meta name="theme-color" content="#FFFFFF" media="(prefers-color-scheme: light)">
<meta name="theme-color" content="#05070C" media="(prefers-color-scheme: dark)">
```

## Stale rasters

The PNGs under `logos/png/` are regenerated from their SVG sources. The PNGs
under `icons/`, `extras/` and `wallpapers/` are composed artwork with no vector
source in this repository, so they still carry the retired vermilion identity
and must be re-exported from the design source before release. Regenerating the
logo rasters:

```sh
pip install cairosvg
python3 - <<'PY'
import cairosvg
from PIL import Image
for kind in ("glyph", "lockup", "wordmark"):
    for mode in ("light", "dark"):
        svg = f"logos/svg/stella-{kind}-{mode}.svg"
        png = f"logos/png/stella-{kind}-{mode}@3x.png"
        w, h = Image.open(png).size
        cairosvg.svg2png(url=svg, output_width=w, output_height=h, write_to=png)
PY
```
