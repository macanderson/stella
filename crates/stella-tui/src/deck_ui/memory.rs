// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `x` and `e` on a highlighted memory row — SPEC 6.3's `· e edit · x reject`.
//!
//! Half of that decision: whether the *highlight* is a memory offering the
//! verb. The other half — whether the keystroke belongs to the keyboard or to
//! the composer — is [`super::row_keys`]'s, which asks it identically for
//! every bare letter a row lends.

use crossterm::event::{KeyCode, KeyEvent};

use crate::deck::WorkspaceModel;
use crate::deck_ui::{DeckAction, DeckUi};
use crate::envelope::WorkspaceInput;
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

/// The two ways out of a memory edit that store nothing (#5231): Esc, per
/// SPEC 13 — every overlay closes on Esc — and `⏎` on a composer the reader
/// has emptied.
///
/// Both need answering ahead of the deck's modal contexts, because the latch
/// is not one of them: it holds no overlay of its own, so nothing further down
/// recognises it. The blank-`⏎` case needs that position especially — the
/// composer's Enter handling is gated on `!ui.composer.is_blank()`, so an
/// empty submission never reaches `submit_prompt` and would leave the reader
/// silently still latched, their next `⏎` rewriting a memory they thought they
/// had backed out of.
///
/// Abandon rather than store: a memory with no words steers nothing while
/// still being recalled, and `e` again is how the reader changes their mind.
///
/// `None` when no edit is in flight, so the key goes on to do its normal job.
pub(super) fn abandon_edit(key: KeyEvent, ui: &mut DeckUi) -> Option<DeckAction> {
    let backing_out = matches!(key.code, KeyCode::Esc)
        || (matches!(key.code, KeyCode::Enter) && ui.composer.is_blank());
    if !backing_out {
        return None;
    }
    let memory_id = ui.editing_memory.take()?;
    ui.composer.clear();
    ui.notice.push(format!("left {memory_id} as it was"));
    Some(DeckAction::Handled)
}

/// A submission while `e edit` is latched: store the words, do not run a turn
/// saying them (#5231).
///
/// Read before every other route in `submit_prompt`, deck-local commands
/// included: a memory whose replacement text happens to read like `/help` is
/// still a memory, and the reader typed it into a buffer this latch had
/// already claimed.
///
/// `None` when no edit is in flight, so an ordinary prompt is unaffected.
pub(super) fn submit_edit(ui: &mut DeckUi, text: &str) -> Option<DeckAction> {
    let memory_id = ui.editing_memory.take()?;
    let text = text.trim().to_string();
    if text.is_empty() {
        ui.notice.push(format!("left {memory_id} as it was"));
        return Some(DeckAction::Handled);
    }
    Some(DeckAction::Send(WorkspaceInput::EditMemory {
        memory_id,
        text,
    }))
}
