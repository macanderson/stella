// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The SESSIONS overlay's keys and its row list.
//!
//! Split out of [`super`] (a god file, closed to growth). The list is the
//! part with a decision in it: the registry holds every session this machine
//! ever ran, and an overlay that listed all of them let forty finished
//! sessions from other repositories bury the two that are live here. The
//! overlay opens on **this workspace's recent work and anything live
//! anywhere**; `h` widens it to the whole registry.

use super::{DeckAction, DeckUi, KeyCode, KeyEvent, WorkspaceInput};
use crate::envelope::{SessionInfo, SessionPhase};

/// How long a finished session stays on the default view.
pub const RECENT_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// Whether a phase is work in flight or parked — always listed, wherever it
/// runs.
pub fn is_live(phase: SessionPhase) -> bool {
    matches!(
        phase,
        SessionPhase::InProgress | SessionPhase::NeedsInput | SessionPhase::Paused
    )
}

/// The rows the overlay lists, in order: live first, then by recency.
///
/// With `show_all` off, a finished session is listed only when it belongs to
/// this deck's workspace (the workspace of the snapshot's `mine` row) and was
/// touched inside [`RECENT_MS`]; archived sessions are never on the default
/// view. `now_ms` is the model's clock, so
/// the list is a pure function of the snapshot.
pub fn visible_session_rows(ui: &DeckUi, now_ms: u64) -> Vec<&SessionInfo> {
    let here = ui
        .sessions
        .iter()
        .find(|s| s.mine)
        .map(|s| s.workspace.as_str());
    let mut rows: Vec<&SessionInfo> = ui
        .sessions
        .iter()
        .filter(|s| {
            if ui.sessions_show_all || s.mine || is_live(s.phase) {
                return true;
            }
            // No row of our own in the snapshot (a demo, a test) — then
            // every workspace is "here" rather than none of them.
            s.phase != SessionPhase::Archived
                && here.is_none_or(|w| w == s.workspace)
                && now_ms.saturating_sub(s.updated_ms) <= RECENT_MS
        })
        .collect();
    rows.sort_by(|a, b| {
        is_live(b.phase)
            .cmp(&is_live(a.phase))
            .then_with(|| b.updated_ms.cmp(&a.updated_ms))
    });
    rows
}

/// How many registry rows the default view hides — the number `h` offers.
pub fn hidden_session_rows(ui: &DeckUi, now_ms: u64) -> usize {
    if ui.sessions_show_all {
        return 0;
    }
    ui.sessions
        .len()
        .saturating_sub(visible_session_rows(ui, now_ms).len())
}

/// The SESSIONS overlay key map: ↑/↓ select, `⏎` resume the selected session
/// when its row is resumable (the durable-state sessions of THIS workspace
/// with no live owner) or open it read-only (replay) otherwise, `n` start a
/// new session, `h` show every session on the machine, `a` archive, `x`
/// delete (another session's record only — never this session's own), `r`
/// refresh, Esc/`←`/`q` close. Modal: everything else is swallowed.
pub(super) fn handle_sessions_key(key: KeyEvent, now_ms: u64, ui: &mut DeckUi) -> DeckAction {
    let selected = |ui: &DeckUi| {
        visible_session_rows(ui, now_ms)
            .get(ui.sessions_sel)
            .copied()
            .cloned()
    };
    let count = visible_session_rows(ui, now_ms).len();
    ui.sessions_sel = ui.sessions_sel.min(count.saturating_sub(1));
    match key.code {
        KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => {
            ui.sessions_open = false;
            DeckAction::Handled
        }
        KeyCode::Up => {
            ui.sessions_sel = ui.sessions_sel.saturating_sub(1);
            DeckAction::Handled
        }
        KeyCode::Down => {
            if count > 0 {
                ui.sessions_sel = (ui.sessions_sel + 1).min(count - 1);
            }
            DeckAction::Handled
        }
        KeyCode::Enter => match selected(ui) {
            // Navigate INTO the chosen session live: close the overlay and
            // hand over to the driver, which adopts the durable state and
            // continues the session in this deck
            // ([`WorkspaceInput::SessionResume`]).
            Some(row) if row.resumable && !row.mine => {
                ui.sessions_open = false;
                DeckAction::Send(WorkspaceInput::SessionResume { id: row.id })
            }
            // Every other row opens read-only: the driver registers a
            // `replay:<id>` lane and streams the persisted events
            // ([`WorkspaceInput::SessionOpen`] — replay IS the fold).
            Some(row) => {
                ui.sessions_open = false;
                DeckAction::Send(WorkspaceInput::SessionOpen { id: row.id })
            }
            None => DeckAction::Handled,
        },
        // A fresh session: the driver parks this one and opens an empty
        // record, exactly as a resume does with a stored one.
        KeyCode::Char('n') => {
            ui.sessions_open = false;
            DeckAction::Send(WorkspaceInput::SessionNew)
        }
        KeyCode::Char('h') => {
            ui.sessions_show_all = !ui.sessions_show_all;
            ui.sessions_sel = 0;
            DeckAction::Handled
        }
        KeyCode::Char('r') => DeckAction::Send(WorkspaceInput::SessionsRefresh),
        KeyCode::Char('a') => match selected(ui) {
            Some(row) => DeckAction::Send(WorkspaceInput::SessionArchive { id: row.id }),
            None => DeckAction::Handled,
        },
        KeyCode::Char('x') => match selected(ui) {
            // This deck's own record is written by this process — deleting
            // it out from under the writer would just resurrect on the next
            // transition, so the key refuses.
            Some(row) if !row.mine => {
                DeckAction::Send(WorkspaceInput::SessionDelete { id: row.id })
            }
            _ => DeckAction::Handled,
        },
        _ => DeckAction::Handled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        id: &str,
        phase: SessionPhase,
        workspace: &str,
        updated_ms: u64,
        mine: bool,
    ) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            title: id.into(),
            summary: String::new(),
            description: None,
            workspace: workspace.into(),
            phase,
            started_ms: 0,
            updated_ms,
            mine,
            resumable: false,
            turns: 0,
            spend_micros: 0,
            model: None,
        }
    }

    const DAY: u64 = 24 * 60 * 60 * 1000;

    /// **The witness.** The default view is this workspace's recent sessions
    /// and anything live; the registry's history stays behind `h`.
    #[test]
    fn the_default_view_hides_old_and_foreign_history() {
        let now = 30 * DAY;
        let mut ui = DeckUi {
            sessions: vec![
                row("mine", SessionPhase::InProgress, "/w", now, true),
                row(
                    "recent-here",
                    SessionPhase::Complete,
                    "/w",
                    now - DAY,
                    false,
                ),
                row(
                    "old-here",
                    SessionPhase::Complete,
                    "/w",
                    now - 20 * DAY,
                    false,
                ),
                row(
                    "recent-elsewhere",
                    SessionPhase::Complete,
                    "/other",
                    now - DAY,
                    false,
                ),
                row(
                    "live-elsewhere",
                    SessionPhase::Paused,
                    "/other",
                    now - 10 * DAY,
                    false,
                ),
                row("archived-here", SessionPhase::Archived, "/w", now, false),
            ],
            ..Default::default()
        };
        let ids: Vec<&str> = visible_session_rows(&ui, now)
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(ids, ["mine", "live-elsewhere", "recent-here"]);
        assert_eq!(hidden_session_rows(&ui, now), 3);

        ui.sessions_show_all = true;
        assert_eq!(visible_session_rows(&ui, now).len(), 6);
        assert_eq!(hidden_session_rows(&ui, now), 0);
    }

    /// Live first, then newest first — the order a reader looks for work in.
    #[test]
    fn live_rows_lead_and_the_rest_sort_by_recency() {
        let now = 30 * DAY;
        let ui = DeckUi {
            sessions: vec![
                row("mine", SessionPhase::InProgress, "/w", now - 3 * DAY, true),
                row("a", SessionPhase::Complete, "/w", now - 2 * DAY, false),
                row("b", SessionPhase::Complete, "/w", now - DAY, false),
                row("p", SessionPhase::Paused, "/w", now - 5 * DAY, false),
            ],
            ..Default::default()
        };
        let ids: Vec<&str> = visible_session_rows(&ui, now)
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(ids, ["mine", "p", "b", "a"]);
    }
}
