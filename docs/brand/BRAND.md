# Stella brand system

The normative source for Stella's visual identity.

Values are mirrored in four places, which must move together:

| Surface | File |
| --- | --- |
| Terminal | `stella-tui/src/palette.rs` |
| Brand assets | `docs/brand/tokens/stella-colors.css`, `docs/brand/tokens/stella-tokens.json` |
| Docs site | `website/src/app/tokens.css` |
| Observatory | `stella-observatory/src/assets/index.html` (inlined `:root` block) |

The marketing site (`macanderson/stella-website`) is the origin of these values;
it is the reference rendering.

## Identity

Stella is an **observatory**: an instrument you point at a codebase to see what
is actually there. The identity is **electric blue and gold on deep space**.
Blue is the interactive signal; gold is the mark. It is quiet, dense, and
legible — closer to a star chart than to a product launch.

The retired vermilion (`#FF3D1F`, "ember") and the terminal-green dark theme are
gone. No surface uses an orange, rust, or phosphor-green hue.

## Colour

### Ground — deep space

| Token | Value | Role |
| --- | --- | --- |
| `void` | `#05070C` | Deepest ground — full-bleed backdrops, splash, OG art |
| `ground` | `#080A0F` | App background |
| `surface` | `#0B0F17` | Cards, panels |
| `raised` | `#101623` | Popovers, selected rows |
| `hairline` | `#182031` | Seam — decorative only, 1.2:1, never the sole carrier of structure |
| `hairline-strong` | `#232D42` | Heavier decorative seam — 1.4:1, still decorative |
| `hairline-contrast` | `#525F80` | 3.0:1 on `surface` — the only seam that may carry structure alone |

`hairline` and `hairline-strong` are both far below the 3:1 floor that WCAG
1.4.11 sets for a graphical element, which is fine for what they are: they
separate things that are *also* separated by spacing, weight, or a heading. The
mistake is reaching for them when the border is the only thing saying "these two
regions are different" — a card edge on a page of equal-weight cards, or the rule
under a table header. That is what `hairline-contrast` is for.

An interactive control's boundary is never decorative. Use `hairline-contrast`
or a text tone; never `hairline`.

| Token | Value | Role |
| --- | --- | --- |
| `paper` | `#FFFFFF` | Light background |
| `snow` | `#F5F7FA` | Light surface |
| `paper-raised` | `#E7EBF2` | Light popovers, selected rows |
| `paper-hairline` | `#D3DAE6` | Light seam |

### Brand — electric blue

The interactive signal: active/running state, focus, links, primary action.

| Token | Value | Ground | Contrast |
| --- | --- | --- | --- |
| `brand` | `#2E7BFF` | dark | 5.1:1 on `ground` — fills and large marks |
| `brand-bright` | `#5AA0FF` | dark | 7.4:1 on `ground` — small text and thin strokes |
| `brand-deep` | `#1A5FE0` | dark | 3.5:1 on `ground` — pressed state, gradient-deep stop |
| `brand-ink` | `#1550C8` | light | 7.0:1 on `paper` |
| `brand-ink-deep` | `#0F3A94` | light | 10.2:1 on `paper` — pressed stop |

`brand-deep` was `#1550C8` in an earlier draft, carried over from the marketing
site where it only ever backed a button fill. It measures 2.84:1 on `ground`,
under the 3:1 floor for interactive and graphical elements — which matters
because it is the leading stop of the determinate progress fill, so the leftmost
cells of a running bar sat below the floor against the canvas. `#1A5FE0` clears
it at 3.5:1. `#1550C8` survives unchanged as `brand-ink`, where it sits on paper
at 7.0:1 and is entirely correct.

### Gold — the mark

The identity accent: the logo's block cursor, progress fill, splash rules,
section markers.

**Gold never carries status.** It sits close enough to `warning` in hue that a
reader must never have to tell the two apart in the same row. Status is amber
and always glyph-paired; gold is identity and appears only on brand chrome.

| Token | Value | Ground | Contrast |
| --- | --- | --- | --- |
| `gold` | `#F5C145` | dark | 11.8:1 on `ground` |
| `gold-bright` | `#FFD873` | dark | headline gradient stop |
| `gold-deep` | `#C99420` | dark | trailing stop of the progress fill |
| `gold-ink` | `#8A6118` | light | 5.5:1 on `paper` |

### Text

| Role | Dark | Contrast | Light | Contrast |
| --- | --- | --- | --- | --- |
| primary | `#F2F5FA` | 18.1:1 | `#0A0F1A` | 19.2:1 |
| secondary | `#8E97A8` | 6.7:1 | `#525C6E` | 6.7:1 |
| tertiary | `#737D92` | 4.8:1 | `#6B7488` | 4.7:1 |

Every ratio above is computed, and every one clears AA for body text. An earlier
draft of this table specified `#7B8598` as the paper tertiary and claimed 4.6:1;
it actually measures 3.7:1, which is a large-text-only value. It was corrected
rather than demoted, because a three-tier text scale in which the third tier
cannot be read is a two-tier scale with a trap in it.

### Status

Always paired with a glyph. Hue alone never carries meaning.

| Token | Dark | Light |
| --- | --- | --- |
| `success` | `#4ADE80` | `#16744F` |
| `warning` | `#EAB308` | `#A16207` |
| `danger` | `#FF5C7A` | `#C81E3E` |

### Data marks

Categorical series in the Observatory are deliberately *not* the brand hue — a
data mark must not read as "active".

| Slot | Value | Hue | On `surface` |
| --- | --- | --- | --- |
| `data-1` | `#E3B341` | 42° | 10.2:1 |
| `data-2` | `#8F70E8` | 256° | 5.2:1 |
| `data-3` | `#E4408F` | 327° | 5.0:1 |
| `data-4` | `#2FD3C6` | 175° | 10.3:1 |

**The ramp is four slots, and that is a constraint on the charts, not a
placeholder.** Gold sits at hue 42° and `data-1` sits at hue 42°; measured
against each other they are 1.17:1, so nothing but size tells them apart. Any
view that shows the gold mark may not also use `data-1` — in practice that
retires `data-1` from every chart on a page with brand chrome, leaving three.

The temptation is to add a fifth and sixth value. Resist it: the usable hue
circle is already crowded by `success` (142°), `warning` (48°), `danger` (349°),
the brand blue (218°), and gold (42°), and every further slot has to clear all
of them plus each other by ~40°. Colours added under that pressure end up
distinguishable in a swatch and identical in an 8-pixel chart cell.

**Beyond three categories, encode with something other than hue.** Order the
series and label them directly; use shape or dash pattern for line series; or
choose an encoding with fewer categories — "which crate" is a weak question
compared with churn, symbol count, or last-touched-by-a-run, and those have
natural rankings that a sequential ramp serves better than a categorical one.

## Logo

The mark is the lowercase `s` followed by a filled block cursor — a prompt
mid-thought. The lockup prefixes a chevron. The geometry is fixed; only hue
changes between variants.

| Part | Light | Dark |
| --- | --- | --- |
| `ink` (glyph, wordmark) | `#0A0F1A` | `#F2F5FA` |
| `sub` | `#525C6E` | `#8E97A8` |
| `chev` (chevron) | `#1550C8` | `#5AA0FF` |
| `cur` (cursor block) | `#B5831F` | `#F5C145` |

The cursor block carries two values because one gold cannot hold its edge
against both white and deep space — `#B5831F` clears 3:1 on paper, `#F5C145`
clears 11.8:1 on ground. Same hue family, different value; the retired mark
shipped two vermilions for the same reason.

Monochrome variants exist for single-colour reproduction and must stay single
colour. Never recolour the cursor block outside the gold family, and never place
the mark on a ground between `#404050` and `#9090A0`, where neither variant
separates.

## Type

| Role | Face |
| --- | --- |
| Brand / wordmark | Geist Mono Bold |
| Product UI, prose | Geist Sans |
| Annotation, code, terminal | Geist Mono / DM Mono |

## Voice

Declarative and specific. State what the software does and what it costs you.

- No superlatives, no "revolutionary", "blazingly fast", "game-changing".
- No exclamation marks. No emoji in headings or nav.
- One idea per sentence; one claim per paragraph.
- Prefer a number to an adjective. "Aborts between steps, never mid-tool" beats
  "robust budget enforcement".
- A sentence that would survive in a man page survives on the home page.
