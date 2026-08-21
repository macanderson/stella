//! `WorkspaceModel` → the v2 widgets' inputs.
//!
//! The v2 widgets are pure projections of explicit input structs
//! ([`super::status_bar::Status`] and its siblings), which is what makes their
//! golden frames fixture data all the way down. This module is the one place
//! that turns the live fold into those inputs, so a widget never learns about
//! `WorkspaceModel` and a test never has to build one.
//!
//! Everything here reads state the deck already holds. No clock, no
//! environment, no process table — the same contract `render_deck` keeps, and
//! the reason a v2 frame is reproducible from a scenario.

use stella_tui_theme::token;

use crate::deck::{PipelineRole, WorkspaceModel};
use crate::deck_ui::DeckUi;

use super::status_bar::{CTX_WINDOW, Status};

/// A pin's slug with the API gateway stripped: the *model's own* vendor, not
/// whoever proxied the call.
///
/// [`crate::deck::RolePin::slug`] answers "where did this call go", which is the
/// right question for routing and the wrong one for a status bar — a reader
/// wants to know which model is thinking, not which reseller billed it.
pub(super) fn vendor_slug(pin: &crate::deck::RolePin) -> String {
    if pin.model.contains('/') || pin.provider.is_empty() {
        pin.model.clone()
    } else {
        format!("{}/{}", pin.provider, pin.model)
    }
}

/// The model actually answering, as the bar names it.
///
/// The *active* role when one is pinned, else the worker, else whatever pin
/// exists — the precedence the v1 status line used, because "which pin is
/// answering" is one question and two answers to it would be a bug.
/// Unlike the v1 cell this drops the `worker: ` prefix: SPEC 5 gives the slot
/// to the slug alone, and the role is already named by the stage beside it.
fn worker_slug(model: &WorkspaceModel) -> String {
    let role = model
        .active_role
        .filter(|r| model.role_pins.contains_key(r))
        .or_else(|| {
            PipelineRole::ORDER
                .iter()
                .copied()
                .find(|r| *r == PipelineRole::Worker && model.role_pins.contains_key(r))
        })
        .or_else(|| {
            PipelineRole::ORDER
                .iter()
                .copied()
                .find(|r| model.role_pins.contains_key(r))
        });
    role.and_then(|role| model.role_pins.get(&role))
        .map(vendor_slug)
        .unwrap_or_else(|| "—".into())
}

/// Everything SPEC 5's status bar says, read off the live fold.
///
/// Borrowed rather than owned where it can be: `Status` holds `&str` for the
/// two text cells, so this returns a value tied to `model`'s lifetime and the
/// draw path allocates one `String` (the slug) per frame rather than seven.
#[must_use]
pub fn status<'a>(model: &'a WorkspaceModel, ui: &DeckUi, worker: &'a str) -> Status<'a> {
    let focused = model.agents.get(ui.focused);
    Status {
        worker,
        // The stage word the stepper already uses. `—` when no stage has been
        // announced, never an invented one: a bar that guesses where the run
        // is has spent the credibility the whole row exists to hold.
        stage: focused
            .and_then(|a| a.model.hud.stage.as_ref())
            .map_or("—", |s| s.as_str()),
        // Window occupancy is the latest call's prompt size, not the session's
        // cumulative input — which dwarfs the window after a few turns and
        // would peg the meter at full from turn three onward.
        ctx_used: focused.map_or(0, |a| a.context_tokens) as f64 / CTX_WINDOW as f64,
        spend_usd: model.total_cost(),
        saved_usd: model.total_cache_savings_usd(),
        inbox: ui.notifications.iter().filter(|n| !n.read).count() as u32,
    }
}

/// The slug the bar borrows, computed once per frame.
///
/// Split from [`status`] because `Status` borrows it: the caller owns the
/// `String` for the length of the draw, which is what lets the struct stay
/// `Copy` and allocation-free everywhere else.
#[must_use]
pub fn worker(model: &WorkspaceModel) -> String {
    worker_slug(model)
}

/// The ground the v2 bar paints, so a caller can fill its band before drawing.
///
/// Exposed rather than assumed: every contrast figure in the palette is
/// measured against this value, and a terminal's own background is not it.
pub const GROUND: ratatui::style::Color = token::BG;
