//! The deck's tab views. Each exposes
//! `render(model: &WorkspaceModel, ui: &mut DeckUi, area: Rect, buf: &mut Buffer)`
//! — a deterministic draw of the (model, ui) into a sub-area, recording any
//! viewport metrics it needs for scroll clamping back onto `ui.metrics`.
//! (`engine` and `tools` are the exceptions: they are the two config editors
//! the SETTINGS tab ([`settings`]) hosts as the two panes of its ←/→ nav, not
//! tab renderers of their own — each exposes `render_panel(ui, area, buf)`
//! plus its own key handler, modal while that panel is focused. `installed`
//! is a third: it
//! is the AGENTS tab's INSTALLED AGENTS pane, dispatched from
//! [`agents::render`] rather than from the deck's tab match, and its
//! `render(ui, now_ms, area, buf)` takes only the deck clock off the model rather than
//! carry a dead one — it has no model-derived state and no key handler of
//! its own, deck_ui.rs routes its keys directly.)

/// The braille spinner's frames — the classic 10-frame dot cycle.
const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// One spinner frame advance per ~80ms.
const SPINNER_PERIOD_MS: u64 = 80;

/// The animated in-flight spinner glyph, a pure function of the deck clock
/// (`model.now_ms`, advanced by the shell's ~30 fps tick) — no timer state.
/// `no_anim` (`--no-anim` / `NO_COLOR`) pins it to a static glyph so
/// recordings stay byte-stable.
pub(crate) fn spinner_glyph(now_ms: u64, no_anim: bool) -> &'static str {
    if no_anim {
        return SPINNER_FRAMES[0];
    }
    SPINNER_FRAMES[((now_ms / SPINNER_PERIOD_MS) as usize) % SPINNER_FRAMES.len()]
}

pub mod agents;
pub mod budget_card;
pub(crate) mod cards;
pub mod dispatch_card;
pub mod engine;
pub mod files;
pub mod graph;
pub mod installed;
pub mod issues;
pub(crate) mod linear;
pub mod mcp;
pub mod models_card;
pub mod plan_card;
pub mod plan_rail;
pub mod proof;
pub mod session;
pub mod settings;
pub mod skills;
pub(crate) mod subagents;
pub mod tools;
pub mod traces;
