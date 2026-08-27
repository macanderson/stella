// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `x` on a highlighted memory row — SPEC 6.3's `· x reject`. The key is
//! routed from `handle_session_key`; everything that decides whether the press
//! means *reject this memory* or means *type an `x`* lives here.
//!
//! The sibling of [`super::undo`], and the same shape for the same reason:
//! `deck_ui.rs` is a god file closed to growth, so it keeps one dispatch line
//! and the question lives beside it.
//!
//! ## Why the text travels with the id
//!
//! The rejection is a tombstone, and a tombstone stored against the id alone
//! would not hold: the reflection loop re-mines paraphrases of lessons it has
//! already learned, so the same rejected lesson comes back tomorrow under a
//! fresh `nod_…`. `stella-store`'s `forget` compares candidates against the
//! *content* it copied in at forget time, which is what catches the
//! restatement — so the text is not decoration on the notice, it is the half
//! that makes the rejection durable.

use crossterm::event::{KeyEvent, KeyModifiers};

use crate::deck::WorkspaceModel;
use crate::deck_ui::{DeckAction, DeckUi};
use crate::envelope::WorkspaceInput;
use crate::model::TranscriptEntry;

/// The rejection `x` means right now, or `None` when it means the letter —
/// SPEC 6.3's `· x reject`, the affordance a logged memory's own footer
/// advertises.
///
/// The whole of the decision lives here rather than beside the key in
/// `handle_session_key`, because `deck_ui.rs` is a god file closed to growth
/// and its arm has to be one line. A reader following `x` lands on this doc.
///
/// Three ways it means the letter, each of which [`super::undo`]'s `u` guards
/// the same way: the press carries a modifier, the composer holds a draft
/// somebody is mid-sentence in, or the highlight is not a logged memory.
pub(super) fn reject_selected(
    key: &KeyEvent,
    model: &WorkspaceModel,
    ui: &DeckUi,
) -> Option<DeckAction> {
    if key.modifiers.intersects(
        KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::META | KeyModifiers::ALT,
    ) || !ui.composer.is_blank()
    {
        return None;
    }
    let (memory_id, text) = selected_memory(model, ui)?;
    Some(DeckAction::Send(WorkspaceInput::RejectMemory {
        memory_id,
        text,
    }))
}

/// The memory under the transcript highlight, as the `(id, text)` a rejection
/// needs.
fn selected_memory(model: &WorkspaceModel, ui: &DeckUi) -> Option<(String, String)> {
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
