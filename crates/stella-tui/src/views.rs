//! The deck's tab views. Each exposes
//! `render(model: &WorkspaceModel, ui: &mut DeckUi, area: Rect, buf: &mut Buffer)`
//! — a deterministic draw of the (model, ui) into a sub-area, recording any
//! viewport metrics it needs for scroll clamping back onto `ui.metrics`.
//! No SETTINGS pane is left in this module: AGENTS is
//! [`crate::v2::engine_panel`], TOOLS is [`crate::v2::tools`] and SEATS is
//! [`crate::v2::seats`], each with its own key handler, modal while that pane
//! is focused. The AGENTS tab and its INSTALLED AGENTS pane are
//! [`crate::v2::agents_page`] and [`crate::v2::installed`].

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

pub mod approval;
pub(crate) mod cards;
pub mod dispatch_card;
pub mod files;
pub mod graph;
pub mod issues;
pub(crate) mod linear;
pub mod picker;
pub mod question;
pub(crate) mod queue_popup;
pub mod session;
pub mod settings;
pub mod skills;
pub mod traces;
