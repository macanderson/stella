Build a production-grade design system for the website of **stella** — a terminal-based agentic coding tool that is faster, cheaper, and more accurate than any other agent harness, verified #1 on Terminal-Bench 2.1. The brand already exists and is non-negotiable; your job is to express it as a website design system (tokens → components → pages), not to invent a new one. Follow this spec exactly. Deliver: design tokens as CSS variables, core components in light AND dark, and a landing page composition that uses them. Dark is the default theme; light is fully supported and switchable.

## brand essence

stella is Latin for star. The logomark is a comet — a four-point star with three speed trails, always flying left→right: the star is the benchmark result, the trail is the speed. Personality: quietly confident, precise, fast — a gold star that doesn't need to shout. Brand name is lowercase always ("stella", never "Stella" or "STELLA"). In marketing copy the name may be written stella\* — the footnote asterisk carries the claim (\*faster, cheaper, more accurate) and doubles as the shell wildcard. Four principles govern everything: one shape · one color · assemble, don't spin · terminal-native.

## logomark — use this exact svg, never redraw it

```svg
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 96" fill="none"><line x1="10" y1="48" x2="30" y2="48" stroke="#C58A32" stroke-width="7" stroke-linecap="round"/><line x1="18" y1="34" x2="30" y2="34" stroke="#C58A32" stroke-width="7" stroke-linecap="round"/><line x1="18" y1="62" x2="30" y2="62" stroke="#C58A32" stroke-width="7" stroke-linecap="round"/><path d="M64 26 C65.65 39.2 72.8 46.35 86 48 C72.8 49.65 65.65 56.8 64 70 C62.35 56.8 55.2 49.65 42 48 C55.2 46.35 62.35 39.2 64 26 Z" fill="#C58A32"/></svg>
```

Lockup = this mark + "stella" in JetBrains Mono ExtraBold, lowercase, tracking −2%, text optically centered on the star. Clearspace = half a star-height on all sides. Minimum sizes: 16px favicon, 24px UI. Below 16px use the star alone.

## color — hard tokens

- **Bronze Gold `#C58A32`** — THE brand color (the comet's tail + the gold star). Primary buttons, focus rings, key accents, the mark. Never body text on light surfaces.
- **brand-deep `#8B5E1A`** — gold for small text on light surfaces (4.99:1, AA). **brand-700 `#8B5E1A`** — gold for *shapes* on light surfaces; the mark takes this on paper, because full-strength gold measures 2.63:1 there, under the 3:1 graphical floor. In v4.0 these two stops are the same value: the text rule and the mark rule converge on brand-700, where under v3.0's Ion they did not.
- **Obsidian `#070B10`** — dark surface. Never pure black. **Paper `#E9EDF2`** — text on dark. **Paper-bg `#EEF1F5`** — light surface.
- Brand ramp 50→950: `#FBF3E3 #F3DEC0 #EBCB9D #DFB473 #D39F50 #C58A32 #A97227 #8B5E1A #674415 #462E10 #291B0D`
- Cool neutral ramp 50→950: `#F1F8FF #DBE2EA #C0C8D0 #A7AEB6 #90979E #7A8088 #60666D #474D54 #2F353B #1A1F25 #070B10`
- Semantic (shadcn-style): primary = gold with obsidian foreground; ring = gold. Dark: background obsidian, card `#12181D`, border `#2A3036`, muted-foreground `#9299A1`, destructive `#FF749B`. Light: background `#EEF1F5`, card `#F7F9FC`, border `#CFD6DD`, muted-foreground `#61676F`, destructive `#C21F3A`.
- Budget per view: surfaces + text ≈86%, secondary ≈10%, gold ≤4%. Gold is the signal, never the surface.
- Contrast facts to respect: gold on obsidian 6.63:1 (AA); gold on paper-bg 2.63:1 (unusable — under the 3:1 graphical floor; swap to brand-deep for text and for marks, which are the same stop in v4.0).

## typography

One family everywhere, including headings and marketing copy: **JetBrains Mono** (the product lives in a terminal, so the brand speaks in monospace). Weights: 400 body & code, 500 UI labels & overlines, 700 headings, 800 display & wordmark. Fallback: ui-monospace, SF Mono, Menlo, Consolas, monospace. Scale: display 48/800/−2%, h1 38/800/−2%, h2 30/700/−1%, h3 24/700, lg 20/500, body 16/400, terminal 14/400, small 13/400, overline 12/500/+14% tracking uppercase — overlines are the ONLY uppercase in the entire system; headings are lowercase. Line-height 1.15 display, 1.65 body. Keep code ligatures on.

## shape & space

Radius scale 4/6/10/14/18 (base token 10px; cards 16–18px; pills 999). Borders 1px everywhere (`#2A3036` dark / `#CFD6DD` light). Cards sit slightly lighter than the background in dark mode. 8px spacing grid, generous rhythm, section padding ≥84px.

## texture — subtle, classy, nearly subliminal

Four sanctioned layers: (1) terminal grid — 96px cells, 1px lines at 5% opacity, every 8th line 2×; (2) starfield — seeded scatter, ~70% dots + 30% four-point sparkles in gold/paper at 5–16% opacity; (3) gold glow — radial at 10–13%, only behind the star or a hero focal point; (4) film grain — fractal noise at 4–5%, hero and banner surfaces only. Three laws: nothing over 6% opacity (starfield highlights may reach 16%), never behind body text, never inside the logo's clearspace.

## motion — "assemble, don't spin"

Nothing rotates, pulses, or bounces idly. Elements arrive fast and settle precisely. Easings: arrive `cubic-bezier(.22,.9,.35,1)` for entrances; pop `cubic-bezier(.34,1.56,.64,1)` for confirmations and the star landing; exit `cubic-bezier(.55,0,.85,.25)` for dismissals. Durations 120/180/280ms. The loading state IS the brand: the comet assembles — three trails streak in left→right, the star pops in with a small overshoot, and on completion the whole comet flies off to the right. Honor `prefers-reduced-motion` by showing the finished mark statically.

## components to design

Buttons (primary: gold fill, obsidian text; secondary: 1px outline; ghost), inputs with gold focus ring, terminal window (obsidian bg, three dots top-left, `$` prompt, blinking block cursor), code block with copy button, sticky nav with backdrop blur and the lockup at left, hero with a one-line install command inside a terminal window, benchmark section for Terminal-Bench 2.1 results (gold bar/line = stella, cool neutrals = competitors — gold never used for anything but stella), feature cards, comparison table, pricing cards, badge/chip in overline style, stat tiles with gold numerals ("2.1× faster"), toast/callout, tabs, docs layout (sidebar + prose column), footer with faint starfield texture.

## pages

Landing (hero → benchmark proof → how it works → features → pricing → final CTA), docs, pricing, changelog.

## voice

Short, declarative, lowercase. Numbers do the talking. Hero pattern: "the fastest agent in the terminal." / "faster · cheaper · more accurate — verified on Terminal-Bench 2.1" / CTA "install stella". Benchmark chip: "verified on Terminal-Bench 2.1" in gold (brand-deep on light).

## never

Rotate or flip the comet, or fly it right→left. Gradients, outlines, or shadows on the mark. Full-strength gold body text on light surfaces. A capital S in stella. Any typeface other than JetBrains Mono. Texture behind body text. Pure black backgrounds. More than one gold accent competing in a view. Spinners that spin.
