# stella-tui-theme

The stella TUI v2 palette, glyph vocabulary, wordmark, and degradation map —
one crate so there is one answer.

```rust
use stella_tui_theme::{glyph, token, wordmark};

Span::styled(format!("{} gate types", glyph::GATE), Style::new().fg(token::GOLD));
```

Seventeen tokens (SPEC 3.1), the hue clamp that holds them (SPEC 3.2), sixteen
state glyphs (SPEC 4), the `stella*` wordmark (SPEC 3.3), and the 16-color
fallback for terminals without truecolor (SPEC 3.5).

## What is enforced, and why it is a crate

The palette is not a convention here; it is a set of assertions.

- **The hue clamp.** Gold must satisfy `r > g > b`, `g >= 0.78 r`,
  `b <= 0.35 r`. Below the green ratio the hue is orange, and orange on a
  near-black ground reads brown on a cheap panel — which is how nearly every
  black-and-gold terminal scheme dies. Grays must be neutral or blue-tipped
  (`r == g`, `b >= g`); one point warm is how the same scheme becomes sepia.
  Both ship as unit tests on the shipped table, exactly as SPEC 3.2 asks, and
  they are unweakenable: a change that turns one green by loosening a bound has
  broken the design, not fixed a test.
- **Role totality.** `token::ALL` pairs every token with a `token::Clamp`, and
  the tag picks the predicate through `clamp::satisfies`. Adding a token means
  declaring what kind of colour it is — and a warm hex has no honest
  declaration to pick. That is the whole anti-drift mechanism; the individual
  assertions only cover today's set.
- **Fallback totality.** `fallback::ansi16` answers for every token, proven by
  walking the table, so a token added without a 16-color stand-in fails rather
  than quietly shipping a 24-bit value to a terminal that cannot show it.

## One authored gold, and a lift that is proven, not asserted

Worth reading before touching the clamp, because it is the one place the
implementation departs from SPEC 3.2's literal wording — and it does so to make
the spec's own argument satisfiable.

SPEC 3.2 states one rule over "every color in the gold role", including
`b <= 0.35 r`. SPEC 3.1's own `gold_bright` `#F7D96B` measures `0.433`. That is
not a bad colour; it is a geometry problem, and the test
`the_resting_blue_ceiling_is_unsatisfiable_above_this_lightness` pins it: in a
gold, lightness is `(r + b) / 510`, so `b <= 0.35 r` caps lightness at
`(255 + 89) / 510 = 0.6745` — for **any** colour, gold or not. `gold_bright`
sits at `0.6941`. Above that line the ceiling is a rule nothing can satisfy, so
a lift could never have been held to it.

The two clauses were never doing the same job, so they are stated where each is
coherent:

| Clause | Applies to | Why |
|---|---|---|
| `r > g > b`, `g >= 0.78 r` | **every** gold | the hue rule — gold versus orange, true at every lightness |
| `b <= 0.35 r` | the resting gold | the saturation rule — a hue that is not lightened holds full saturation |
| same hue within 3°, strictly lighter | a lift | anchored to `GOLD` itself |

The anchor is the durability win. A ceiling admits every colour beneath it and
keeps passing after the gold it was cut for is gone; an anchor admits only *the
authored gold, brighter*. Recolour `GOLD` and leave `GOLD_BRIGHT` behind and
`gold_bright_is_a_lift_of_gold` fails immediately, naming the hue distance. The
palette has exactly one authored gold, and the second is a proven consequence
of it.

The 3° tolerance is derived, not chosen: `stella-tui`'s v1 theme records that
its warning amber sits 4.0° from gold "so an outcome may never be told from
chrome by hue alone" — 4° is the distance this repository already treats as
indistinguishable, and a lift must be nearer than that to be the *same* hue.

## Boundary — does this change belong here?

This crate owns *what a colour is* and *what a state looks like*. It does not
own where either one goes: which metal an event rail takes belongs to the
widget that draws rails, and how much of a meter is filled belongs to whatever
computes the fraction. The test is whether the answer changes when the layout
does — if it does, it is not a palette fact and it is not welcome here.

## Dependencies — one, and it stays one

`ratatui`, for `Color`, `Style` and `Span`. This is a leaf by contract, the
[`stella-tty`](../stella-tty) / [`stella-diff`](../stella-diff) shape: every v2
surface depends on this crate, so it may depend on nothing that could ever
paint a cell. A palette that pulls in the widgets it colours is a cycle
waiting for its second consumer.

## God files — do not add lines

This crate has no god files: no file exceeds the gate's 1500-line ratchet
(`scripts/check-file-size.sh`), and none may appear — a new file crossing 1500
lines fails the gate outright, and `scripts/file-size-baseline.txt` accepts no
new entries. When a file here approaches the limit, split it before it
crosses.

## Relationship to `stella-tui::palette`

The v1 deck's palette (`crates/stella-tui/src/palette.rs`, "Phosphor Gold on
Ink") is a deliberately **warm-neutral** system: gold `#FFB81A`, warm paper
text, and a stated rule of "no cool grays anywhere on the dark side". The v2
spec inverts exactly that — a cooler gold and a blue-tipped neutral ramp — so
the two cannot be one table, and this crate does not try to be a superset of
the old one. The v1 palette stays normative for every surface still drawing v1
until that surface migrates; the sweep for warm hex in v2 render code is
scoped to v2 render code for the same reason.

That paragraph described a tree in which `stella-tui::palette`, the brand kit
and this crate each carried their own gold, and the sentence held them apart by
naming all three. They agree now: v5.0 put the kit, the site, the Observatory
and both TUI palettes on one gold, generated from
`design/tokens/stella-tokens.json` by `scripts/gen-tokens.py` and checked by
`make tokens`. So there is no longer a divergence to name — the values are one
value. The v2 spec's scope is still the TUI; what changed is that the palette
above it is shared rather than parallel.

## What is generated and what is not

`src/token.rs` is **generated** — values and role tags, emitted from the JSON.
Edit the JSON and run `make tokens-update`; never edit that file.

Everything else here is hand-written, and the split is where it is on purpose.
`src/clamp.rs` holds the predicates, because the gold clamp is an argument with
a geometric proof behind it rather than a templatable expression — a generator
that emitted it would put the algorithm somewhere its proof could not follow.
`clamp::satisfies` is the bridge between the two: the generator emits role
*tags* and never predicates, which is also the only shape that can express an
anchored clamp, since a lift is checked against another token's value and no
per-row template can reach one.

This was two files for a few hours. #4055 created this crate with a
hand-written table; #4066 added the generator and emitted `generated.rs`
beside it; the two landed 41 seconds apart with no textual conflict, leaving
one palette defined twice (#4083, #4058).

## Two hue functions, named for their spaces

`src/oklch.rs` is the OKLCH ruler the 30° separation law is measured with,
plus `SEPARATION_FLOOR_DEG` — the floor itself, stated once. Three suites read
it: `stella-tui`'s `theme::tests` over the terminal palette, `stella-cli`'s
`design_token_parity` over the eight instrument surfaces, and
`scripts/check-hue-separation.py`, which holds its own Python port to this
file's literals and refuses to run if they have moved. It lives here because
this crate is a near-leaf every surface can already link; it was two Rust
transcriptions of Ottosson's matrices held to two different floors until #4071.

`clamp::srgb_hue_degrees` is a different metric for a different job — the
gold-lift anchor's 3° tolerance was cut in sRGB against the gold this palette
ships, and it stays there. Both are named for the space they measure, because
one of them being named for the job is how a reader concluded the two were
copies of each other.

## Consumers

- `stella-tui`: `src/v2/` — the v2 widgets, starting with the single-line
  status bar (SPEC 5).
