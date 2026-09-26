---
id: brand-hue
title: "What gold may mean"
status: living
---

# What gold may mean

**Status:** living. This page holds the rule for gold on every product
surface. Each file in the table below cites it as `doc:brand-hue`, and
`crates/stella-tui/tests/spec_brand_hue.rs` fails when one stops.

## The rule

Gold is stella's own colour. It marks the brand, and it never marks a verdict.
Pass, fail, and needs attention take green, red, and amber. Each of those sits
at least 30° from gold in hue, and `scripts/check-hue-separation.py` checks the
gap.

The house brand kit (`doc:brand`) goes one step further: gold never marks a
state at all. The web pages keep that rule. Interactive mode does not, and the
sections below say why.

## Surfaces

<!-- BEGIN brand-hue-surfaces -->
| Surface | Source | Gold marks | Statuses that take gold |
|---|---|---|---|
| Interactive mode. | `crates/stella-tui/src/theme.rs`, `crates/stella-tui/src/palette.rs` | The mark, the prompt, focus, selection, and progress. | `Running` |
| Interactive mode v2 spec. | `design/tui-v2/SPEC.md` | The mark, and stella acting: edit, write, gate, and money. | `running` |
| Web pages. | `crates/stella-observatory/src/assets/index.html`, `crates/stella-cli/src/export.rs` | The mark, and at most one main action per page. | none |
<!-- END brand-hue-surfaces -->

## Interactive mode

Gold marks one status: `Running`, the one that says stella is at work. The
terminal has three reasons the web does not.

- A cell has no opacity and no subpixel control. It may fall back to 256 or 16
  colours.
- The web marks "active" by swapping ink and paper. In a cell, that swap costs
  the reverse attribute, and the row may already use it.
- Every status carries a glyph: `▶` for running, `✓` for done. The hue never
  carries the state alone, so the screen still reads under `NO_COLOR`.

`gold_never_carries_a_verdict` in `crates/stella-tui/src/theme/tests.rs`
proves no verdict maps to gold. `spec_brand_hue.rs` holds the table row above
to `status_color`.

## The v2 spec

Section 2 of the spec gives gold to stella acting on the world: edit, write,
gate, brand, and money. Silver is the world coming in. The running glyph `◐`
takes the bright gold. Red and green keep pass and fail, so gold still marks
no verdict.

## Web pages

The Observatory and the `/export` page give gold to the brand alone: the
asterisk in `stella*`, the favicon mark, and at most one main action per page.
No state takes it, and that includes `Running`.

- A page can swap ink and paper for "active" at no cost, so it needs no hue for
  that job.
- These pages are all data. A hue in the chrome pulls the eye away from the
  data.
- Gold and amber are the pair that clash. The page keeps `--warn` far from the
  gold in hue, and keeps gold off every state as well.

`crates/stella-cli/tests/design_token_parity.rs` checks the web palette. That
includes the hue gap between the brand mark and each state.

## Cost

Run stella in one pane and open the Observatory in another. Gold marks a
running agent in the first and only the brand in the second. The glyph keeps
that from misleading you: a running row in the terminal shows `▶` whatever its
colour.

## Changing a rule

Edit this page and the files its row names in the same change. If a surface
moves to the other rule, change its test too: `gold_never_carries_a_verdict`
for interactive mode, and `design_token_parity.rs` for the web pages.
