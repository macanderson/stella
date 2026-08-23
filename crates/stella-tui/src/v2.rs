//! The TUI v2 command deck — the black-and-gold redesign.
//!
//! Built beside the v1 deck rather than on top of it, and landing one whole
//! surface at a time: a v1 module stays normative until the thing that replaces
//! it draws.
//!
//! It began as two palettes. v1 was warm-neutral by design and v2's hue clamp
//! rejects a warm gray outright, so a gradual recolour was not available — the
//! first migrated widget would have sat beside eleven unmigrated ones in the
//! opposite metal. That is no longer the split: v5.0 put both on one gold, and
//! [`crate::palette`]'s shared constants are re-exports of
//! [`stella_tui_theme::token`] rather than copies of it. What still separates
//! the two is *layout and event vocabulary*, not colour — which is why a v2
//! surface can now replace a v1 one a band at a time, as the status bar did.
//!
//! Every colour in this module tree comes from [`stella_tui_theme::token`] and
//! every state glyph from [`stella_tui_theme::glyph`]. A hex literal below
//! this line is a defect, not a shortcut — `no_hex_literals_in_v2_render_code`
//! in `tests/v2_status_bar.rs` is what says so.

pub mod fields;
pub mod frame;
pub mod graph;
pub mod sessions;
pub mod status_bar;
pub mod status_source;
pub mod transcript;
pub mod transcript_source;
