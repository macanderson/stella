---
id: brand
title: "the house brand kit, mirrored"
status: living
---

# docs/brand — the house kit, mirrored

**This directory is a copy. The source is
[oxageninc/brand](https://github.com/oxageninc/brand),
and nothing here is edited by hand.**

```sh
make brand-sync                              # pull the kit into this repo
node scripts/sync-brand-assets.mjs --check   # fail if a copy has drifted
```

Both read the kit from `$OXAGEN_BRAND_KIT`, else from `../oxagen-brand` or
`~/Projects/oxagen-brand`, whichever exists first. Pass `--brand <dir>` to the
script to name another checkout.
`.github/workflows/brand-drift.yml` runs the check against the kit's `main`.

```sh
node scripts/sync-brand-assets.mjs --brand ~/Projects/oxagen-brand
```

## Why a copy exists at all

`website/src/lib/brand-parity.test.ts` holds the site's assets to the kit's,
and it runs in CI, where the house kit is not checked out. So the kit lands
here first — offline, in the repository — and the site is held to this copy.
Deleting it would not remove a duplicate; it would remove the only thing the
parity test can compare against, which is how the site drifted a whole brand
version behind in the first place.

## What changed

Stella's own kit stood here until the house system, with its own generator:
`build_marks.py` drew a four-point comet through `cometkit.py`, `sync_site.py`
copied the results into the website, and `social/build_social.py` composed the
banners. All of it is retired.

**The comet is gone.** Stella's mark is the **asterisk**, and it already lives
inside the word: `stella*`, set in Space Grotesk at the kit's logo weight with
the asterisk in gold. That combined form *is* the Stella lockup and it is the
only one — nothing is ever placed to the left of the word. The kit emits no
separate Stella lockup, which is why there is none here.

**The palette is the house palette.** One table for Stella and Oxagen, so a
reader crossing between the two sites does not watch the brand change hue. The
normative copy for this repo is `design/tokens/stella-tokens.json`, and
`css/tokens.css` mirrors it. The sync writes the kit's value into each Stella
token the house palette owns, in both files. `css/house-tokens.css`,
`css/house-tokens.json`, and `css/house-tailwind.css` are the kit's own
files, verbatim. `css/tokens.css` imports `css/house-tokens.css` for its
corner scale, so keep the two files together.

**The faces are the house's three.** Space Grotesk sets the wordmarks and h1
to h3, Aeonik sets the text and h4 to h6, and Monaspace Neon sets code and
terminal transcripts.

## What is here

```
logo/svg/     the wordmark and the asterisk: adaptive · dark · light · mono · tile
pwa/          favicons, app icons, maskables, the ICO, the manifest snippet
spinners/     the house motion — animated SVG, no script
social/       avatar · x · linkedin · youtube · open graph
fonts/        the three house faces, with licences
css/          tokens.css (mirrors the JSON) · house-tokens.css · house-tokens.json · house-tailwind.css
prompts/      the design-system prompts
site/         static page mocks
brand-guidelines.html
```

## Rules worth knowing before you use it

- **One glyph is gold**: the asterisk. Never a second one, never the whole word.
- **The gold is identity and at most one action per screen.** It is never a
  surface and it never encodes a state — that is what `--st-green`, `--st-amber`
  and `--st-red` are for, and `scripts/check-hue-separation.py` keeps them 30°
  clear of it.
- **Gold as text on warm paper becomes `#8A7223`.** The mark keeps the metal;
  words do not.
- **Nothing sits to the left of `stella*`.** The asterisk is the only mark.
- **Minimum 88 px** for the wordmark, **24 px** for the icon. Below that, use
  the favicon.
- **The icon is the only picture we own.** No stock illustration, no gradient
  mesh, no 3D render. A surface that needs a picture builds one out of the icon
  — its outline, a field of it, or simply a much bigger one.
