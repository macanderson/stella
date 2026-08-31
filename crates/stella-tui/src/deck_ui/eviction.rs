// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Moving the reader's saved spots when old lines drop off the top.
//!
//! The deck keeps slot numbers per agent. One marks an open entry. One marks
//! a folded turn. One marks the cursor. When old entries drop, every slot
//! below them moves up. A number left alone points at the wrong entry.

use crate::deck::WorkspaceModel;
use crate::deck_ui::DeckUi;

/// Move each saved slot number by the shift its agent just took. Drop the
/// ones that named a lost entry.
///
/// They move rather than clear. `stella_transcript::fold` holds that reader
/// state should live through changes around it. What a reader opened is
/// reader state. A number for a lost entry is dropped, not clamped. Clamping
/// would pick an entry the reader never chose.
pub(super) fn rebase(model: &WorkspaceModel, ui: &mut DeckUi) {
    for (idx, agent) in model.agents.iter().enumerate() {
        let evicted = agent.model.evicted_entries();
        let seen = ui.evicted_seen.get(&agent.meta.id).copied().unwrap_or(0);
        if evicted <= seen {
            continue;
        }
        ui.evicted_seen.insert(agent.meta.id.clone(), evicted);
        // The shift counts slots, not entries. Each pass drains a chunk and
        // puts one marker in its place. So the two differ by one, and only on
        // the first pass. Later passes drain a marker that already held a
        // slot. Get this wrong the other way and the live pane hides entries
        // that were never saved.
        let slots = (evicted - seen).saturating_sub(usize::from(seen == 0));
        ui.scrollback.shift_after_eviction(&agent.meta.id, slots);
        // Slot 0 holds the marker. A survivor lands at 1 or more. An old
        // number at or below `slots` named a lost entry, or the old marker,
        // which the new one took over at slot 0.
        let shift = move |i: usize| -> Option<usize> {
            if i == 0 && seen > 0 {
                return Some(0);
            }
            i.checked_sub(slots).filter(|&n| n >= 1)
        };
        if let Some(set) = ui.expanded.get_mut(&agent.meta.id) {
            *set = set.iter().copied().filter_map(shift).collect();
            if set.is_empty() {
                ui.expanded.remove(&agent.meta.id);
            }
            ui.expanded_rev += 1;
        }
        // Fold sets are keyed on turn-start slots. They take the same shift,
        // so they move together.
        if let Some(set) = ui.folded_turns.get_mut(&agent.meta.id) {
            *set = set.iter().copied().filter_map(shift).collect();
            if set.is_empty() {
                ui.folded_turns.remove(&agent.meta.id);
            }
            ui.fold_rev += 1;
        }
        // The cursor belongs to the focused agent's transcript.
        if idx == ui.focused {
            ui.session_selected = ui.session_selected.and_then(shift);
        }
    }
}
