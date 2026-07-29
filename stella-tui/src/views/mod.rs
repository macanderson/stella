//! The deck's tab views. Each exposes
//! `render(model: &WorkspaceModel, ui: &mut DeckUi, area: Rect, buf: &mut Buffer)`
//! — a deterministic draw of the (model, ui) into a sub-area, recording any
//! viewport metrics it needs for scroll clamping back onto `ui.metrics`.
//! (`engine` and `tools` are the exceptions: they are the two config editors
//! the SETTINGS tab ([`settings`]) hosts side by side, not tab renderers of
//! their own — each exposes `render_panel(ui, area, buf)` plus its own key
//! handler, modal while that panel is focused. `installed` is a third: it
//! is the AGENTS tab's INSTALLED AGENTS pane, dispatched from
//! [`agents::render`] rather than from the deck's tab match, and its
//! `render(ui, area, buf)` drops the unused `model` parameter rather than
//! carry a dead one — it has no model-derived state and no key handler of
//! its own, deck_ui.rs routes its keys directly.)

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
