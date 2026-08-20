//! What survives of the deck's bottom band after SPEC 5.
//!
//! This module used to own a **two-row** statline: a dim micro-label row
//! stacked over its value row, ten cells wide, carrying MODEL, the stage box,
//! CPU, CONTEXT, SPEND, CACHE, SAVED, WARMTH, ENGINE and INBOX, with an ethos
//! chip pinned right and a whole-cell drop rule for narrow rows.
//!
//! Its argument was sound and did not survive changing the set of cells.
//! Stacking a label above a value spends free vertical space instead of scarce
//! horizontal space, which is the right trade when the values need labels —
//! `▮▮▮▯▯▯ 91k/200k` does not explain itself. SPEC 5 cut the set to six values
//! that do: `$0.45` is money, `✉ 21` is an inbox, and the two that want a word
//! carry it inline in four cells. CPU, MEM, WARMTH and ENGINE moved behind `?`
//! and the AGENTS tab because none is a fact about the *work*; CACHE collapsed
//! into the one number a reader acts on, `saved`. A label row above that set is
//! a row of chrome explaining values that explain themselves, and permanent
//! chrome has to earn its row on every frame.
//!
//! [`crate::v2::status_bar`] is the row now, projected by
//! [`crate::v2::project`]. Three things stayed behind, because the new bar does
//! not want them and something still does:
//!
//! - [`CTX_WINDOW`], the divisor the context meter reads.
//! - `vendor_slug`, which answers "whose model is this" for the bar's worker
//!   cell — the model's own vendor, not whoever proxied the call.
//! - [`render_diagnosis`], the low-hit-rate sentence, which is prose and so has
//!   nowhere to sit on a row of glanceable values.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};

use crate::cache_panel;
use crate::deck::WorkspaceModel;
use crate::deck_ui::DeckUi;

/// The context window the CONTEXT meter divides by.
pub const CTX_WINDOW: u64 = 200_000;

/// A pin's slug with the API gateway stripped: the *model's own* vendor, not
/// whoever proxied the call.
///
/// `RolePin::slug` answers "where did this call go", which is the right
/// question for routing and the wrong one for a status bar — a reader wants to
/// know which model is thinking, not which reseller billed it.
pub(crate) fn vendor_slug(pin: &crate::deck::RolePin) -> String {
    if pin.model.contains('/') || pin.provider.is_empty() {
        pin.model.clone()
    } else {
        format!("{}/{}", pin.provider, pin.model)
    }
}

/// The low-hit-rate diagnosis, on a row of its own under the status bar.
///
/// Deliberately not a cell on that bar: a full sentence explaining *why* the
/// cache is cold is prose, and a row of six glanceable values is the one place
/// prose cannot go. Byte-identical to what `stella stats` prints for the same
/// cause, which is the property that earns it a row at all — two surfaces
/// explaining one fact differently is how a user learns to trust neither.
///
/// Renders nothing when the focused agent has not earned one, so a caller may
/// hand it a row unconditionally; `render_deck` only reserves that row when the
/// diagnosis exists.
pub fn render_diagnosis(model: &WorkspaceModel, ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    if let Some(cause) = model
        .agents
        .get(ui.focused)
        .and_then(|a| a.cache_diagnosis(cache_panel::LOW_HIT_RATE_THRESHOLD))
    {
        Paragraph::new(Line::from(cache_panel::diagnosis_spans(cause))).render(area, buf);
    }
}
