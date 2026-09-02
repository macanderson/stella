// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Key routing for the session-override pickers (`/model`, `/agent`).
//! They are modal, like the SESSIONS/INBOX overlays beside them in the
//! routing chain. They sit in their own module for the same god-file
//! reason as `super::parked` (not a link: that module is private).
//!
//! The rows are read live off the deck state here, once per keystroke.
//! See [`crate::views::picker`] for why the picker never holds them.
//! So `⏎` maps the highlighted index onto the list as it stands now. A
//! list that moved under the highlight sends the row the user sees, not
//! the row they opened on.
//!
//! "Now" means *before* the key is folded. Folding it may edit the
//! filter, which moves the list out from under the index that same
//! keystroke is about. So the matches are read first. That keeps two
//! things on one list: the bounds handed to [`ListPicker::key`], and the
//! row a [`PickerAction::Choose`] lands on.
//!
//! [`ListPicker::key`]: crate::views::picker::ListPicker::key
//! [`PickerAction::Choose`]: crate::views::picker::PickerAction::Choose

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
        // Read the matches before the key is folded, as the SETTINGS picker
        // does. The keystroke may edit the filter. A choice must land on the
        // list the reader saw when they pressed `⏎`.
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
