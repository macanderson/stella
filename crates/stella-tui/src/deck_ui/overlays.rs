// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Opening an overlay, and the help overlay's own keys.
//!
//! Split out of [`super`] (a god file, closed to growth), beside `local`,
//! `nav`, `gates` and `queue_editor`. Each opener sets the overlay's view
//! state and returns the one request its content needs, so the two callers
//! that can raise the same overlay — a key chord and a `/command` — set it
//! up identically rather than each remembering the fields to reset.

use super::{DeckAction, DeckUi, KeyCode, KeyEvent, SkillOp, WorkspaceInput, list_nav};

/// Open the SESSIONS overlay (`ctrl-e`, `/sessions`) and ask the driver for
/// a fresh registry snapshot.
pub(crate) fn open_sessions_overlay(ui: &mut DeckUi) -> DeckAction {
    ui.sessions_open = true;
    ui.sessions_sel = 0;
    DeckAction::Send(WorkspaceInput::SessionsRefresh)
}

/// Open the CONTEXT overlay (`ctrl-k`, `/context`) and freshen both
/// snapshots it renders — the second refresh rides `pending_inputs` since a
/// key returns only one action.
pub(crate) fn open_context_overlay(ui: &mut DeckUi) -> DeckAction {
    ui.context_open = true;
    ui.context_scroll = 0;
    ui.pending_inputs.push(WorkspaceInput::McpRefresh);
    DeckAction::Send(WorkspaceInput::Skill(SkillOp::List))
}

/// Open the INSPECT overlay (`⌃g`, `/inspect`) on its call list and ask the
/// driver for a fresh index. Opens on the list, never straight into a detail:
/// which call produced a given transcript line is not knowable from UI state
/// yet (transcript entries carry no step coordinate), so a human picks.
pub(crate) fn open_inspect_overlay(ui: &mut DeckUi) -> DeckAction {
    ui.inspect_open = true;
    ui.inspect_sel = 0;
    ui.inspect_view = None;
    ui.inspect_scroll = 0;
    ui.inspect_pending = false;
    DeckAction::Send(WorkspaceInput::InspectRefresh)
}

/// Open the INBOX overlay (`/inbox`). The driver's poller keeps the
/// notification snapshot fresh; nothing to request.
pub(crate) fn open_inbox_overlay(ui: &mut DeckUi) -> DeckAction {
    ui.inbox_open = true;
    ui.inbox_sel = 0;
    DeckAction::Handled
}

/// The help-overlay key map. The overlay is modal: scrolling keys drive it,
/// `q`/`Esc`/`?` close it. The content is long enough to scroll on a typical
/// terminal, so a plain "any key closes" dismiss would make it unreadable.
pub(super) fn handle_help_key(key: KeyEvent, ui: &mut DeckUi) -> DeckAction {
    let (total, height) = (ui.metrics.help_total, ui.metrics.help_height);
    if list_nav::closes(key) || matches!(key.code, KeyCode::Char('?')) {
        ui.help_open = false;
    } else {
        list_nav::scroll(key, &mut ui.help_scroll, total, height, true);
    }
    // Ctrl-C is handled by the caller (quit precedes every modal context).
    // Any other key — modified or not — is swallowed so the overlay stays
    // open and stable; typing into the composer behind it would be
    // invisible and confusing.
    DeckAction::Handled
}
