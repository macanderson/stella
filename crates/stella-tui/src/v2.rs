//! The TUI v2 command deck — the black-and-gold redesign.
//!
//! Built beside the v1 deck rather than on top of it. The two disagree about
//! the palette at the root (v1 is warm-neutral by design; v2's hue clamp
//! rejects a warm gray outright — see [`stella_tui_theme`]), so a gradual
//! recolour of the existing widgets was never available: the first migrated
//! widget would have had to sit next to eleven unmigrated ones in the opposite
//! metal. Phases land whole surfaces here, and the v1 modules stay normative
//! until the surface that replaces them ships.
//!
//! Every colour in this module tree comes from [`stella_tui_theme::token`] and
//! every state glyph from [`stella_tui_theme::glyph`]. A hex literal below
//! this line is a defect, not a shortcut — `no_hex_literals_in_v2_render_code`
//! in `tests/v2_status_bar.rs` is what says so.

pub mod status_bar;
