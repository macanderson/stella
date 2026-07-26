# stella — brand kit

The prompt identity: `›stella▮` set in Geist Mono Bold with the vermilion block cursor.
Same system as the hailo.dev original — lockups: lockup / wordmark / glyph.
(No TLD in this identity, so `wordmark` doubles as the compact form.)

Each lockup ships as `-adaptive` (recolors via prefers-color-scheme), `-light`, `-dark`,
`-mono` (currentColor), `-mono-black`, `-mono-white`. Transparent @3x PNGs in logos/png.

Colors — ink #0B0B0C / #F5F6F7 · sub #9AA0A6 / #7D838A · cursor #FF3D1F / #FF4B2A.
Tokens in tokens/. PWA icons + manifest.webmanifest ready to drop in:

```html
<link rel="icon" href="/icons/favicon.svg" type="image/svg+xml">
<link rel="apple-touch-icon" href="/icons/apple-touch-180.png">
<link rel="manifest" href="/manifest.webmanifest">
<meta name="theme-color" content="#FFFFFF" media="(prefers-color-scheme: light)">
<meta name="theme-color" content="#0B0B0C" media="(prefers-color-scheme: dark)">
```

Spinner: `spinner/stella-spinner-*.svg` — SMIL loop typing `stella`, cursor blinks at rest (3.6s). Works as <img>, no JS.
Wallpapers: desktop + phone × 4K/5K/6K × light/dark. Extras: OG cards 1200×630.
