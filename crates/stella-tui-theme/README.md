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
  `prompt.md` rule 3 makes them unweakenable: a change that turns one green by
  loosening a bound has broken the design, not fixed a test.
- **Role totality.** `token::ALL` pairs every token with a `token::Role`, and
  the role picks the clamp. Adding a token means declaring what kind of colour
  it is — and a warm hex has no honest declaration to pick. That is the whole
  anti-drift mechanism; the individual assertions only cover today's set.
- **Fallback totality.** `fallback::ansi16` answers for every token, proven by
  walking the table, so a token added without a 16-color stand-in fails rather
  than quietly shipping a 24-bit value to a terminal that cannot show it.

One documented exception exists and is worth reading before touching the
clamp: SPEC 3.1's own `gold_bright` `#F7D96B` does not satisfy SPEC 3.2's blue
ceiling (0.433 against a stated 0.35). `prompt.md` rule 4 forbids inventing a
replacement colour, so the value stands and the exception is recorded as
`clamp::GOLD_LIFT_BLUE_PCT`, tight to the hundredth against the one token it
was measured from. `gold_bright_is_a_recorded_lift_not_an_unclamped_colour`
holds it there. Re-cutting the value so SPEC 3.2 holds everywhere is the
better fix; the number to beat is in that constant's doc.

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

Note also that `stella-tui::palette` is already held apart from the web brand
kit by its own module doc (`#FFB81A` against `docs/brand/css/tokens.css`'s
`#C58A32`), and this crate changes nothing about the kit, the website, or the
observatory. The v2 spec's scope is the TUI.

## Consumers

- `stella-tui`: `src/v2/` — the v2 widgets, starting with the single-line
  status bar (SPEC 5).
