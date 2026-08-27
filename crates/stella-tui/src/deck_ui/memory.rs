// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `x` on a highlighted memory row — SPEC 6.3's `· x reject`.
//!
//! Half of that decision: whether the *highlight* is a memory offering the
//! verb. The other half — whether the keystroke belongs to the keyboard or to
//! the composer — is [`super::row_keys`]'s, which asks it identically for
//! every bare letter a row lends.

use crate::deck::WorkspaceModel;
use crate::deck_ui::DeckUi;
use crate::model::TranscriptEntry;

/// The memory under the transcript highlight, as the `(id, text)` a rejection
/// needs.
pub(super) fn selected_memory(model: &WorkspaceModel, ui: &DeckUi) -> Option<(String, String)> {
    let idx = ui.session_selected?;
    let transcript = &model.agents.get(ui.focused)?.model.transcript;
    match transcript.get(idx)? {
        TranscriptEntry::MemoryLog {
            memory_id, text, ..
        } => Some((memory_id.clone(), text.clone())),
        // A promotion row names a memory too and does not offer this: its own
        // row carries no `x reject`, and an affordance that works on a row not
        // advertising it is as much a surprise as one advertised on a row
        // where it does nothing. The log row for the same memory is where a
        // reader rejects it.
        _ => None,
    }
}
