// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The GRAPH tab's keyboard: the neighborhood walk, the modal file picker,
//! and the modal free-form query box.
//!
//! In its own module for the god-file reason `super::pickers` gives — the
//! deck's key router is at its line ceiling and this cluster is the part of
//! it that grew.
//!
//! Two modals, deliberately not one. The picker filters a list the deck
//! already holds ([`crate::graph::GraphSnapshot::files`]), so it is a selection and every
//! keystroke narrows a visible set. The query box holds text the deck cannot
//! resolve at all — `stella-tui` never sees the index — so it is an
//! utterance, sent whole on `⏎` and answered by the driver with a fresh
//! snapshot. Collapsing them into one widget would have to pretend the
//! second has a list to walk (#4335).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::envelope::WorkspaceInput;

use super::{DeckAction, DeckUi, list_nav};

/// The GRAPH tab's own keys, reached only once no modal is up.
pub(super) fn handle_key(
    key: KeyEvent,
    ui: &mut DeckUi,
    composer_empty: bool,
) -> Option<DeckAction> {
    let node_count = ui.graph.as_ref().map(|g| g.nodes.len()).unwrap_or(0);
    // The neighborhood is one list; `←`/`→` are the focus tree's sibling
    // step (the tabs either side of GRAPH), not a second way to walk it.
    if list_nav::select(key, &mut ui.graph_cursor, node_count, composer_empty) {
        return Some(DeckAction::Handled);
    }
    match key.code {
        // `/` (filter-as-you-type) or Enter opens the file picker so a user can
        // re-root the neighborhood on any indexed file, not just the busiest
        // one the tab seeds. Gated on an empty composer so both keys stay
        // typeable as the first character of a prompt. Only meaningful once a
        // graph with a file list has loaded.
        KeyCode::Char('/') | KeyCode::Enter if composer_empty && has_files(ui) => {
            open_picker(ui);
            Some(DeckAction::Handled)
        }
        // `q` opens the free-form query box. Gated the same way as `/`, and
        // on a loaded graph for the same reason: with no index behind it the
        // box could only take text nothing would ever answer.
        KeyCode::Char('q') if composer_empty && has_files(ui) => {
            ui.graph_query = Some(String::new());
            Some(DeckAction::Handled)
        }
        _ => None,
    }
}

/// Whether a graph snapshot with at least one listed file is loaded — the
/// precondition for opening the file picker.
pub(super) fn has_files(ui: &DeckUi) -> bool {
    ui.graph.as_ref().is_some_and(|g| !g.files.is_empty())
}

/// Open the file picker, defaulting the selection to the file the neighborhood
/// is currently rooted on (`focus`) — the busiest file on first load. That
/// keeps the sensible default while making every other file reachable: the
/// selection starts on "where you already are", not forced there.
pub(super) fn open_picker(ui: &mut DeckUi) {
    ui.graph_picker_query.clear();
    ui.graph_picker_open = true;
    ui.graph_picker_sel = ui
        .graph
        .as_ref()
        .and_then(|g| g.files.iter().position(|f| *f == g.focus))
        .unwrap_or(0);
}

/// The modal file picker's key map. Printable keys narrow the filter, ↑/↓ walk
/// the filtered matches, Enter re-roots the neighborhood on the selected file
/// (a [`WorkspaceInput::FocusGraphFile`] round-trip — see the envelope docs),
/// and Esc / a cleared-then-Backspace closes it. Selection bounds and the
/// selected path both come from [`crate::graph::GraphSnapshot::matching_files`] so they can
/// never disagree with the rendered list.
pub(super) fn handle_picker_key(key: KeyEvent, ui: &mut DeckUi) -> DeckAction {
    // Snapshot the current match count/selection off the shared filter helper.
    let match_count = ui
        .graph
        .as_ref()
        .map(|g| g.matching_files(&ui.graph_picker_query).len())
        .unwrap_or(0);

    // A type-to-filter input: letters are the query, so only the arrow
    // forms move the selection and only Esc closes.
    if list_nav::select(key, &mut ui.graph_picker_sel, match_count, false) {
        return DeckAction::Handled;
    }
    match key.code {
        KeyCode::Esc => {
            ui.graph_picker_open = false;
            DeckAction::Handled
        }
        KeyCode::Enter => {
            let picked = ui.graph.as_ref().and_then(|g| {
                g.matching_files(&ui.graph_picker_query)
                    .get(ui.graph_picker_sel)
                    .map(|f| f.to_string())
            });
            ui.graph_picker_open = false;
            match picked {
                Some(file) => DeckAction::Send(WorkspaceInput::FocusGraphFile { file }),
                None => DeckAction::Handled, // filter matched nothing — just close
            }
        }
        KeyCode::Backspace => {
            ui.graph_picker_query.pop();
            ui.graph_picker_sel = 0; // the match set changed — re-anchor
            DeckAction::Handled
        }
        // Printable characters extend the filter. Modified chords (Ctrl/Cmd)
        // are not filter input — let them fall through as Ignored so global
        // shortcuts still resolve.
        KeyCode::Char(c) if !modified(&key) => {
            ui.graph_picker_query.push(c);
            ui.graph_picker_sel = 0; // the match set changed — re-anchor
            DeckAction::Handled
        }
        _ => DeckAction::Ignored,
    }
}

/// The modal query box's key map. Printable keys type into it, Enter sends
/// the text as a [`WorkspaceInput::GraphQuery`], Esc closes, and Backspace on
/// an empty box closes rather than doing nothing — the same
/// clear-then-back gesture the picker answers to.
///
/// A blank (or whitespace-only) query closes without sending: the driver
/// would have nothing to look up, and a snapshot of nothing is a worse answer
/// than the neighborhood already on screen.
pub(super) fn handle_query_key(key: KeyEvent, ui: &mut DeckUi) -> DeckAction {
    let Some(text) = ui.graph_query.as_mut() else {
        return DeckAction::Ignored;
    };
    match key.code {
        KeyCode::Esc => {
            ui.graph_query = None;
            DeckAction::Handled
        }
        KeyCode::Enter => {
            let text = text.trim().to_string();
            ui.graph_query = None;
            if text.is_empty() {
                return DeckAction::Handled;
            }
            DeckAction::Send(WorkspaceInput::GraphQuery { text })
        }
        KeyCode::Backspace => {
            if text.pop().is_none() {
                ui.graph_query = None;
            }
            DeckAction::Handled
        }
        KeyCode::Char(c) if !modified(&key) => {
            text.push(c);
            DeckAction::Handled
        }
        _ => DeckAction::Ignored,
    }
}

/// Whether `key` carries a chord modifier, in which case it is a shortcut
/// rather than input and must fall through to the deck's global handlers.
fn modified(key: &KeyEvent) -> bool {
    key.modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::META)
}
