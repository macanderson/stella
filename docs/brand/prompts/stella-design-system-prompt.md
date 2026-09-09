Build a production-grade design system for the website of **stella** — a terminal-based agentic coding tool that is faster, cheaper, and more accurate than any other agent harness, verified #1 on Terminal-Bench 2.1. The brand already exists and is non-negotiable; your job is to express it as a website design system (tokens → components → pages), not to invent a new one. Follow this spec exactly. Deliver: design tokens as CSS variables, core components in light AND dark, and a landing page composition that uses them. Dark is the default theme; light is fully supported and switchable.

This brief is written against **brand kit v5.0 — black and gold**. Every hex below is a token in `design/tokens/stella-tokens.json`, which is upstream of this document; if the two disagree, the JSON wins and this file is the bug. Every contrast ratio below is a measured number printed by `scripts/check-contrast.py`, including the two that do not flatter the palette.

## brand essence

stella is Latin for star. The logomark is a comet — a four-point star with three speed trails, always flying left→right: the star is the benchmark result, the trail is the speed. Personality: quietly confident, precise, fast — a gold star that doesn't need to shout. Brand name is lowercase always ("stella", never "Stella" or "STELLA"). In marketing copy the name may be written stella\* — the footnote asterisk carries the claim (\*faster, cheaper, more accurate) and doubles as the shell wildcard. Four principles govern everything: one shape · one color · assemble, don't spin · terminal-native.

## logomark — use this exact svg, never redraw it

```svg
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 96" fill="none"><line x1="10" y1="48" x2="30" y2="48" stroke="#D6962C" stroke-width="7" stroke-linecap="round"/><line x1="18" y1="34" x2="30" y2="34" stroke="#D6962C" stroke-width="7" stroke-linecap="round"/><line x1="18" y1="62" x2="30" y2="62" stroke="#D6962C" stroke-width="7" stroke-linecap="round"/><path d="M64 26 C65.65 39.2 72.8 46.35 86 48 C72.8 49.65 65.65 56.8 64 70 C62.35 56.8 55.2 49.65 42 48 C55.2 46.35 62.35 39.2 64 26 Z" fill="#D6962C"/></svg>
```

Lockup = this mark + "stella" in JetBrains Mono ExtraBold, lowercase, tracking −2%, text optically centered on the star. Clearspace = half a star-height on all sides. Minimum sizes: 16px favicon, 24px UI. Below 16px use the star alone.

**The mark is the same gold on both grounds.** v3.0 and v4.0 each cut a darker stop so the mark could clear the 3:1 graphical floor on paper; v5.0 retires that stop. Two reasons, and both are binding instructions rather than trivia: gold, `text` and `ink` are the only three mark colours the system has, so a fourth is out of the palette by construction; and WCAG 1.4.3 and 1.4.11 both exempt logotypes by name, which is what lets a gold mark sit on paper while gold body text there stays forbidden. `check-contrast.py` records that pairing at **1.65:1** as an exemption rather than hiding it. Do not invent a darker gold for light surfaces.

## color — hard tokens

The system is **flat**. There are no 50→950 ramps: v5.0 deletes both the brand ramp and the cool neutral ramp and replaces them with the named stops below, each with one role. Do not generate intermediate steps, tints, or shades — a stop that is not on this list is not in the system.

**Dark (the default theme).** CSS variable names are normative.

<!-- BEGIN palette -->

| token | hex | role |
|---|---|---|
| `--st-bg` | `#10100F` | canvas — the near-black ground |
| `--st-panel` | `#181715` | panels, cards, code blocks |
| `--st-hl` | `#201F1C` | selected and hover rows |
| `--st-border` | `#292722` | hairlines, dividers, unfilled meter track |
| `--st-rule` | `#34322D` | section rules, boundaries |
| `--st-gold` | `#D6962C` | THE brand metal: actions, active states, money, the mark |
| `--st-gold-bright` | `#F1C364` | tiny live indicators only — spinner, hot marker |
| `--st-silver` | `#9B958A` | secondary emphasis, syntax strings |
| `--st-silver-type` | `#DDD8CD` | syntax types, tertiary labels |
| `--st-text` | `#F2EEE5` | primary text on dark, in the deck |
| `--st-text` | `#F2EEE5` | primary text on dark, off the deck — the web surfaces and the cut assets, so this is the one you want for a page |
| `--st-muted` | `#8C877C` | secondary text |
| `--st-dim` | `#6E6A62` | hints, captions, line numbers |
| `--st-green` | `#74C991` | pass, additive diff sign |
| `--st-red` | `#E0687D` | fail, destructive, removal diff sign |
| `--st-amber` | `#EB8960` | warning — the one status the core palette does not otherwise name |
| `--st-diff-add` | `#10201A` | added diff row background |
| `--st-diff-del` | `#241019` | removed diff row background |
| `--st-void` | `#0A0A09` | below the canvas: full-bleed backdrops |

**Light.** Not a recolour of the dark set — its own stops, and the only theme where `ink` and the paper tints appear.

| token | hex | role |
|---|---|---|
| `--st-paper` | `#FBFAF6` | light canvas |
| `--st-paper-ground` | `#F2EEE5` | the warm paper page ground, one step under the panel |
| `--st-paper-panel` | `#F5F2EA` | light panel |
| `--st-paper-raised` | `#F8F5EE` | surface raised above the page ground |
| `--st-paper-row` | `#E5DED1` | light hover and selected rows |
| `--st-paper-border` | `#DED5C7` | light border |
| `--st-paper-seam` | `#D8CDBD` | the hairline under a border |
| `--st-ink` | `#10100F` | primary text on light |
| `--st-ink-muted` | `#6B665C` | secondary text on light |
| `--st-gold-ink` | `#8B5E1A` | gold as *text* on the light ground, where the metal cannot clear AA |
| `--st-green-ink` | `#006933` | pass, as text on light |
| `--st-amber-ink` | `#8D3B19` | warning, as text on light |
| `--st-red-ink` | `#952141` | fail, as text on light |

<!-- END palette -->

Two clamps govern any colour work you do on top of this: gold must satisfy `g >= 0.78 r` or it is orange, and orange on a near-black ground reads brown on uncalibrated panels; grays must be neutral or blue-tipped (`r == g`, `b >= g`) or the scheme reads sepia. Both are enforced against the shipped table, so a hand-mixed value that misses them is a build failure, not a taste argument.

- Semantic (shadcn-style): primary = `--st-gold` with `--st-ink` foreground; ring = `--st-gold`. Dark: background `--st-bg`, card `--st-panel`, border `--st-border`, muted-foreground `--st-muted`, destructive `--st-red`. Light: background `--st-paper-ground`, card `--st-paper-panel`, border `--st-paper-border`, muted-foreground `--st-ink-muted`, destructive `--st-red-ink`.
- Budget per view: surfaces + text ≈86%, secondary ≈10%, gold ≤4%. Gold is the signal, never the surface.
- Contrast facts to respect, all measured by `scripts/check-contrast.py`:
  - `text` on `bg` **16.19:1**; `gold` on `bg` **11.99:1**; `gold` on `panel` **11.60:1** — gold is a first-class text colour on dark.
  - `ink` on `gold` **11.15:1** — this is why a filled gold button takes ink, not white.
  - `ink` on `paper` **18.40:1**; `dim` on `paper` **6.15:1**.
  - `muted` on `bg` **4.79:1**; `muted` on `panel` **4.64:1**; `dim` on `bg` **3.14:1** — all three clear the AA and large-text floors their roles are held to. `scripts/contrast-baseline.txt` holds no pairing today.
  - `gold` on `paper` **1.65:1** — unusable for text or graphics, and licensed for the **mark alone** under the WCAG logotype exemption. Gold *text* on light is `--st-gold-ink`.

## typography

One family everywhere, including headings and marketing copy: **JetBrains Mono** (the product lives in a terminal, so the brand speaks in monospace). Weights: 400 body & code, 500 UI labels & overlines, 700 headings, 800 display & wordmark. Fallback: ui-monospace, SF Mono, Menlo, Consolas, monospace. Scale: display 48/800/−2%, h1 38/800/−2%, h2 30/700/−1%, h3 24/700, lg 20/500, body 16/400, terminal 14/400, small 13/400, overline 12/500/+14% tracking uppercase — overlines are the ONLY uppercase in the entire system; headings are lowercase. Line-height 1.15 display, 1.65 body. Keep code ligatures on.

## shape & space

Radius scale 4/6/10/14/18 (base token 10px; cards 16–18px; pills 999). Borders 1px everywhere (`--st-border` dark / `--st-paper-border` light). Cards sit slightly lighter than the background in dark mode — `--st-panel` on `--st-bg`, an 0F over an 0A, which is the whole separation the system allows. 8px spacing grid, generous rhythm, section padding ≥84px.

## texture — subtle, classy, nearly subliminal

Four sanctioned layers: (1) terminal grid — 96px cells, 1px lines at 5% opacity, every 8th line 2×; (2) starfield — seeded scatter, ~70% dots + 30% four-point sparkles in gold/text at 5–16% opacity; (3) gold glow — radial at 10–13%, only behind the star or a hero focal point; (4) film grain — fractal noise at 4–5%, hero and banner surfaces only. Three laws: nothing over 6% opacity (starfield highlights may reach 16%), never behind body text, never inside the logo's clearspace.

## motion — "assemble, don't spin"

Nothing rotates, pulses, or bounces idly. Elements arrive fast and settle precisely. Easings: arrive `cubic-bezier(.22,.9,.35,1)` for entrances; pop `cubic-bezier(.34,1.56,.64,1)` for confirmations and the star landing; exit `cubic-bezier(.55,0,.85,.25)` for dismissals. Durations 120/180/280ms. The loading state IS the brand: the comet assembles — three trails streak in left→right, the star pops in with a small overshoot, and on completion the whole comet flies off to the right. Honor `prefers-reduced-motion` by showing the finished mark statically.

## components to design

Buttons (primary: gold fill, ink text; secondary: 1px outline; ghost), inputs with gold focus ring, terminal window (bg canvas, three dots top-left, `$` prompt, blinking block cursor), code block with copy button, sticky nav with backdrop blur and the lockup at left, hero with a one-line install command inside a terminal window, benchmark section for Terminal-Bench 2.1 results (gold bar/line = stella, silver and the neutral stops = competitors — gold never used for anything but stella), feature cards, comparison table, pricing cards, badge/chip in overline style, stat tiles with gold numerals ("2.1× faster"), toast/callout, tabs, docs layout (sidebar + prose column), footer with faint starfield texture.

## pages

Landing (hero → benchmark proof → how it works → features → pricing → final CTA), docs, pricing, changelog.

## voice

Short, declarative, lowercase. Numbers do the talking. Hero pattern: "the fastest agent in the terminal." / "faster · cheaper · more accurate — verified on Terminal-Bench 2.1" / CTA "install stella". Benchmark chip: "verified on Terminal-Bench 2.1" in gold (`gold-ink` on light).

## never

Rotate or flip the comet, or fly it right→left. Gradients, outlines, or shadows on the mark. Gold body text on light surfaces — that is `gold-ink`, or `ink`. A darker gold cut for the mark on paper; the mark is the same metal on both grounds. Any stop not on the two token tables above — no ramp steps, no hand-mixed tints. A capital S in stella. Any typeface other than JetBrains Mono. Texture behind body text. Pure black backgrounds — the canvas is `#10100F`, not `#000000`. More than one gold accent competing in a view. Spinners that spin.
