//! The command deck's surfaces: every tab body, every overlay, every floating
//! card, and the chrome that wraps them — the tab row, the hint row, the
//! status bar and the pulse row.
//!
//! [`crate::deck_render`] is the dispatcher; each module here draws one
//! surface and owns the state only that surface reads. The two `*_source`
//! modules are the halves that decide *what* a surface shows, split from the
//! ratatui that draws it so the decision is testable without a terminal.
//!
//! Every colour comes from [`stella_tui_theme::token`] and every state glyph
//! from [`stella_tui_theme::glyph`]. A hex literal below this line is a
//! defect, not a shortcut — `no_hex_literals_in_render_code` in
//! `tests/status_bar_goldens.rs` is what says so.

pub mod agents_page;
pub mod approval;
pub mod budget_card;
pub(crate) mod cards;
pub mod dispatch_card;
pub mod engine_panel;
pub mod fields;
pub mod files_tab;
pub mod frame;
pub mod graph;
pub mod graph_tab;
pub mod installed;
pub mod issues_tab;
pub mod mcp_tab;
pub mod models_card;
pub mod picker;
pub mod plan_card;
pub mod pulse;
pub mod question;
pub mod queue;
pub(crate) mod record;
pub mod seats;
pub mod session;
pub mod sessions;
pub mod settings;
pub mod skills;
pub mod status_bar;
pub mod status_source;
pub mod subagents;
pub mod tools;
pub mod traces;
pub mod transcript;
pub mod transcript_source;
