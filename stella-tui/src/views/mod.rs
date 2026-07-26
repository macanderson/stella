//! The deck's tab views. Each exposes
//! `render(model: &WorkspaceModel, ui: &mut DeckUi, area: Rect, buf: &mut Buffer)`
//! — a deterministic draw of the (model, ui) into a sub-area, recording any
//! viewport metrics it needs for scroll clamping back onto `ui.metrics`.
//! (`engine` and `tools` are the exceptions: they are the two config editors
//! the SETTINGS tab ([`settings`]) hosts side by side, not tab renderers of
//! their own — each exposes `render_panel(ui, area, buf)` plus its own key
//! handler, modal while that panel is focused.)

pub mod agents;
pub mod engine;
pub mod files;
pub mod graph;
pub mod installed;
pub mod issues;
pub mod mcp;
pub mod session;
pub mod settings;
pub mod skills;
pub mod tools;
pub mod traces;
