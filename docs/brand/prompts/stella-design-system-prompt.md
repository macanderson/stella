Build a production-grade design system for the website of **stella** — a terminal-based agentic coding tool that is faster, cheaper, and more accurate than any other agent harness, verified #1 on Terminal-Bench 2.1. The brand already exists and is non-negotiable; your job is to express it as a website design system (tokens → components → pages), not to invent a new one. Follow this spec exactly. Deliver: design tokens as CSS variables, core components in light AND dark, and a landing page composition that uses them. Dark is the default theme; light is fully supported and switchable.

## brand essence

stella is Latin for star. The logomark is a comet — a four-point star with three speed trails, always flying left→right: the star is the benchmark result, the trail is the speed. Personality: quietly confident, precise, fast — a gold star that doesn't need to shout. Brand name is lowercase always ("stella", never "Stella" or "STELLA"). In marketing copy the name may be written stella\* — the footnote asterisk carries the claim (\*faster, cheaper, more accurate) and doubles as the shell wildcard. Four principles govern everything: one shape · one color · assemble, don't spin · terminal-native.

## logomark — use this exact svg, never redraw it

```svg
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 96" fill="none"><line x1="10" y1="48" x2="30" y2="48" stroke="#FFB000" stroke-width="7" stroke-linecap="round"/><line x1="18" y1="34" x2="30" y2="34" stroke="#FFB000" stroke-width="7" stroke-linecap="round"/><line x1="18" y1="62" x2="30" y2="62" stroke="#FFB000" stroke-width="7" stroke-linecap="round"/><path d="M64 26 C65.65 39.2 72.8 46.35 86 48 C72.8 49.65 65.65 56.8 64 70 C62.35 56.8 55.2 49.65 42 48 C55.2 46.35 62.35 39.2 64 26 Z" fill="#FFB000"/></svg>
```

Lockup = this mark + "stella" in JetBrains Mono ExtraBold, lowercase, tracking −2%, text optically centered on the star. Clearspace = half a star-height on all sides. Minimum sizes: 16px favicon, 24px UI. Below 16px use the star alone.

## color — hard tokens

- **Phosphor Gold `#FFB000`** — THE brand color (CRT amber phosphor + the gold star). Primary buttons, focus rings, key accents, the mark. Never body text on light surfaces.
- **gold-deep `#A37200`** — gold for small text on light surfaces (3.79:1, AA-large only).
- **Ink `#0B0B0C`** — dark surface. Never pure black. **Paper `#F4F1EA`** — text on dark. **Paper-bg `#F6F2E9`** — light surface.
- Gold ramp 50→950: `#FFF8E4 #FDECC3 #FCDFA1 #FCD07D #FDC154 #FFB000 #D09100 #A37200 #795500 #523900 #2E1F00`
- Warm neutral ramp 50→950: `#FBF9F4 #E4E2DC #CECBC5 #B8B5AE #A29F98 #8D8A82 #736C68 #575150 #3C3839 #222022 #0B0B0C`
- Semantic (shadcn-style): primary = gold with ink foreground; ring = gold. Dark: background ink, card `#131315`, border `#232327`, muted-foreground `#9B9890`, destructive `#E4573F`. Light: background `#F6F2E9`, card `#FCFAF4`, border `#E4DECF`, muted-foreground `#6E6A5F`, destructive `#D4432F`.
- Budget per view: surfaces + text ≈86%, secondary ≈10%, gold ≤4%. Gold is the signal, never the surface.
- Contrast facts to respect: gold on ink 10.7:1 (AAA); gold on paper-bg 1.64:1 (decorative only — swap to gold-deep for text).

## typography

One family everywhere, including headings and marketing copy: **JetBrains Mono** (the product lives in a terminal, so the brand speaks in monospace). Weights: 400 body & code, 500 UI labels & overlines, 700 headings, 800 display & wordmark. Fallback: ui-monospace, SF Mono, Menlo, Consolas, monospace. Scale: display 48/800/−2%, h1 38/800/−2%, h2 30/700/−1%, h3 24/700, lg 20/500, body 16/400, terminal 14/400, small 13/400, overline 12/500/+14% tracking uppercase — overlines are the ONLY uppercase in the entire system; headings are lowercase. Line-height 1.15 display, 1.65 body. Keep code ligatures on.

## shape & space

Radius scale 4/6/10/14/18 (base token 10px; cards 16–18px; pills 999). Borders 1px everywhere (`#232327` dark / `#E4DECF` light). Cards sit slightly lighter than the background in dark mode. 8px spacing grid, generous rhythm, section padding ≥84px.

## texture — subtle, classy, nearly subliminal

Four sanctioned layers: (1) terminal grid — 96px cells, 1px lines at 5% opacity, every 8th line 2×; (2) starfield — seeded scatter, ~70% dots + 30% four-point sparkles in gold/paper at 5–16% opacity; (3) gold glow — radial at 10–13%, only behind the star or a hero focal point; (4) film grain — fractal noise at 4–5%, hero and banner surfaces only. Three laws: nothing over 6% opacity (starfield highlights may reach 16%), never behind body text, never inside the logo's clearspace.

## motion — "assemble, don't spin"

Nothing rotates, pulses, or bounces idly. Elements arrive fast and settle precisely. Easings: arrive `cubic-bezier(.22,.9,.35,1)` for entrances; pop `cubic-bezier(.34,1.56,.64,1)` for confirmations and the star landing; exit `cubic-bezier(.55,0,.85,.25)` for dismissals. Durations 120/180/280ms. The loading state IS the brand: the comet assembles — three trails streak in left→right, the star pops in with a small overshoot, and on completion the whole comet flies off to the right. Honor `prefers-reduced-motion` by showing the finished mark statically.

## components to design

Buttons (primary: gold fill, ink text; secondary: 1px outline; ghost), inputs with gold focus ring, terminal window (ink bg, three dots top-left, `$` prompt, blinking block cursor), code block with copy button, sticky nav with backdrop blur and the lockup at left, hero with a one-line install command inside a terminal window, benchmark section for Terminal-Bench 2.1 results (gold bar/line = stella, warm neutrals = competitors — gold never used for anything but stella), feature cards, comparison table, pricing cards, badge/chip in overline style, stat tiles with gold numerals ("2.1× faster"), toast/callout, tabs, docs layout (sidebar + prose column), footer with faint starfield texture.

## pages

Landing (hero → benchmark proof → how it works → features → pricing → final CTA), docs, pricing, changelog.

## voice

Short, declarative, lowercase. Numbers do the talking. Hero pattern: "the fastest agent in the terminal." / "faster · cheaper · more accurate — verified on Terminal-Bench 2.1" / CTA "install stella". Benchmark chip: "verified on Terminal-Bench 2.1" in gold (gold-deep on light).

## never

Rotate or flip the comet, or fly it right→left. Gradients, outlines, or shadows on the mark. Gold body text on light surfaces. A capital S in stella. Any typeface other than JetBrains Mono. Texture behind body text. Pure black backgrounds. More than one gold accent competing in a view. Spinners that spin.
