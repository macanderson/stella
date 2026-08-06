//! The SESSIONS overlay's snapshot: session-registry records projected into
//! the TUI's own types (split out of `command_deck.rs` under the god-file
//! rule when [`session_phase`] grew its `Stopped` arm, #1653).

use super::Inbound;

/// The SESSIONS overlay snapshot: every registry record mapped to the deck's
/// [`stella_tui::SessionInfo`], flagging this process's own record and the
/// rows that can be reopened HERE (no live owner, this workspace, durable
/// state on disk — ⏎ navigates into those).
pub(super) fn sessions_inbound(
    registry: &stella_store::SessionRegistry,
    mine: &str,
    workspace: &str,
) -> Inbound {
    let sessions = registry
        .list()
        .into_iter()
        .map(|r| {
            // A session mid-mapping advertises its slices right in the
            // summary line, so a human sees "already being mapped" before
            // typing a prompt that would duplicate the exploration.
            let summary = if r.exploring.is_empty() {
                r.summary
            } else {
                format!("{} [mapping: {}]", r.summary, r.exploring.join(", "))
            };
            stella_tui::SessionInfo {
                mine: r.id == mine,
                resumable: r.id != mine && r.workspace == workspace && registry.resumable(&r.id),
                phase: session_phase(r.status),
                id: r.id,
                title: r.title,
                summary,
                workspace: r.workspace,
                started_ms: r.started_at_ms,
                updated_ms: r.updated_at_ms,
            }
        })
        .collect();
    Inbound::Sessions(sessions)
}

/// Store status → TUI phase (the TUI mirrors the enum so it never links the
/// store crate).
fn session_phase(status: stella_store::SessionStatus) -> stella_tui::SessionPhase {
    match status {
        stella_store::SessionStatus::InProgress => stella_tui::SessionPhase::InProgress,
        stella_store::SessionStatus::NeedsInput => stella_tui::SessionPhase::NeedsInput,
        stella_store::SessionStatus::Paused => stella_tui::SessionPhase::Paused,
        stella_store::SessionStatus::Cancelled => stella_tui::SessionPhase::Cancelled,
        stella_store::SessionStatus::Stopped => stella_tui::SessionPhase::Stopped,
        stella_store::SessionStatus::Complete => stella_tui::SessionPhase::Complete,
        stella_store::SessionStatus::Archived => stella_tui::SessionPhase::Archived,
        stella_store::SessionStatus::Error => stella_tui::SessionPhase::Error,
    }
}
