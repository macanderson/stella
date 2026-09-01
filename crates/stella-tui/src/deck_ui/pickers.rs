// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Key routing for the session-override pickers (`/model`, `/agent`) —
//! modal like the SESSIONS/INBOX overlays beside it in the routing chain,
//! and in its own module for the same god-file reason as `super::parked`
//! (not a link: that module is private).
//!
//! The pickers' rows are read live off the deck state here, once per
//! keystroke (see [`crate::views::picker`] on why they are never held in the
//! picker), so `⏎` maps the highlighted index onto whatever the list holds
//! NOW — a list that moved under the highlight sends the row the user is
//! looking at, not the one they opened on.
//!
//! "Now" means *before* the key is folded, because folding it may edit the
//! filter and move the list out from under the index the same keystroke is
//! about. Reading the matches first is what keeps the bounds handed to
//! [`ListPicker::key`] and the row a [`PickerAction::Choose`] resolves to the
//! same list.
//!
//! [`ListPicker::key`]: crate::views::picker::ListPicker::key

use crossterm::event::KeyEvent;

use crate::envelope::WorkspaceInput;
use crate::views::picker::{self, PickerAction};

use super::{DeckAction, DeckUi};

/// Route `key` to whichever picker is up. `None` when neither is (or the
/// key is Ctrl-C, declined so the deck's quit branch still fires). The two
/// are mutually exclusive in practice — each opens from a slash command,
/// which the other, being modal, would have swallowed.
pub(super) fn handle_key(key: KeyEvent, ui: &mut DeckUi) -> Option<DeckAction> {
    if ui.model_picker.open {
        // Snapshot the matches before the key is folded, the way the SETTINGS
        // picker does: the keystroke may edit the filter, and a choice must
        // land on the list the reader was looking at when they pressed `⏎`.
        let matching = picker::model_matches(ui);
        return Some(match ui.model_picker.key(key, matching.len()) {
            PickerAction::Ignored => return None,
            PickerAction::Handled => DeckAction::Handled,
            PickerAction::Close => {
                ui.model_picker.close();
                DeckAction::Handled
            }
            PickerAction::Choose(i) => {
                let spec = matching.get(i).cloned();
                ui.model_picker.close();
                match spec {
                    Some(spec) => DeckAction::Send(WorkspaceInput::ModelOverride { spec }),
                    None => DeckAction::Handled,
                }
            }
        });
    }
    if ui.agent_picker.open {
        let matching = picker::agent_matches(ui);
        return Some(match ui.agent_picker.key(key, matching.len()) {
            PickerAction::Ignored => return None,
            PickerAction::Handled => DeckAction::Handled,
            PickerAction::Close => {
                ui.agent_picker.close();
                DeckAction::Handled
            }
            PickerAction::Choose(i) => {
                let target = matching
                    .get(i)
                    .and_then(|&entry| ui.installed.entries.get(entry))
                    .map(|entry| (entry.name.clone(), entry.scope));
                ui.agent_picker.close();
                match target {
                    Some((name, scope)) => {
                        DeckAction::Send(WorkspaceInput::AgentAssume { name, scope })
                    }
                    None => DeckAction::Handled,
                }
            }
        });
    }
    None
}

/// Whether a picker owns the keyboard — the render side's cursor question.
pub(crate) fn owns_keyboard(ui: &DeckUi) -> bool {
    ui.model_picker.open || ui.agent_picker.open
}
