---
id: adr/0028-panel-cells-are-glyphs-in-the-contract
title: "ADR 0028: A panel cell is a glyph in the contract and a column in the host"
status: implemented
---

# ADR 0028: A panel cell is a glyph in the contract and a column in the host

- Status: accepted
- Date: 2026-09-05
- Decides: `#5185`

## Context

A plugin may draw a panel. The host leases it a box. The plugin sends back a
frame of text.

`PanelText::cells` counts `char`s. It lives in
`crates/stella-plugin/src/panel.rs`. `PanelFrame::fits` is built on that count.
A host calls `fits` to refuse a frame that runs past the lease.

A `char` is not a column. `あ` is one `char` and two columns. An `e` with a
combining acute is two `char`s and one column.

So the two counts differ. `fits` can admit a row that needs more columns than
the lease has. It can also refuse a row that would have fit.

When `#5185` was filed, nothing else measured columns. There was no panel
host at all. The type's docs said the host clipped every blit, and no host
did. The host landed with `#5223`. Its `write_run` advances by display width.
It places a glyph only when every column that glyph needs is inside the lease.

## Decision

`cells()` keeps counting `char`s. The host keeps the column count.

`stella-plugin` is a near-leaf. It argues each workspace edge in its own
`Cargo.toml`, and a width table would be a new one. The host already links
`unicode-width`.

A cell is also a wire term. Change what it means and you change which frames
are legal. That is a wire-contract change, with a corpus to remeasure and
`make wire-schema-update` to run.

And two exact counts would still not agree. A width table has a version. The
terminal has its own. Only the clip has to be right, so only the host should
try.

## Consequences

`fits` returning `Ok` is not a promise that a row is drawn whole. A row of
wide glyphs is cut at the lease's edge. Nothing paints outside the box.
`a_wide_glyph_cannot_reach_past_the_lease_into_the_border` proves that, and
`a_frame_the_contract_admits_by_glyph_count_is_cut_at_the_lease` pins the seam
itself. Both live in `crates/stella-tui/src/plugin_panel.rs`.

A mark that follows a glyph counts as a second cell here. So a frame of
decomposed text can be refused for room it does not use. A plugin that wants
its whole row drawn should send precomposed text, and should keep a glyph and
its marks in one span.

The host draws such a mark on the glyph before it, in one cell, as ratatui
does. A mark with no glyph to ride is dropped. One cell keeps at most thirty
marks, which is the cap Unicode's stream-safe format uses.

What would reopen this: a panel API that has to tell a plugin how much room a
string takes. A plugin cannot ask that question of a glyph count. If we add
one, the contract measures width and this record is amended.
