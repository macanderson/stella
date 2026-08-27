// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! SPEC 6.3's two memory events, worded for the plain (non-deck) door.
//!
//! The deck draws these as a railed block with the whole
//! `OBSERVATION ▸ RULE ▸ FACT` ladder under the head
//! (`views::transcript::memory_log_body`). This surface has one line per event
//! and no rail to hang a block off, so it states the rung the memory is on and
//! the confidence that put it there — what a reader watching a non-interactive
//! run needs in order to know a lesson landed and where.
//!
//! Split out of [`super`] rather than added to it: that file is at its
//! 1500-line ceiling, and a crossing with no baseline entry is a gate failure
//! whose only remedy is this module (AGENTS.md § "God files").

use stella_protocol::{AgentEvent, MemoryClass};

use super::{EventLine, Tone};

/// `◆ memory logged nod_… (observation conf 0.62): <lesson>`.
pub fn memory_logged(memory_id: &str, text: &str, class: MemoryClass, confidence: u8) -> EventLine {
    EventLine {
        glyph: "◆",
        tone: Tone::Muted,
        strong: false,
        body: format!(
            "memory logged {memory_id} ({} conf {}): {text}",
            class.as_str(),
            fraction(confidence)
        ),
        detail: None,
    }
}

/// `◆ memory promoted <lineage> observation → rule (conf 0.87), now prompt-injected`.
///
/// `strong`, unlike the log above: a promotion is the moment an inferred
/// lesson starts being injected into the prompt as an instruction, and this
/// surface has no rail or metal to say so.
pub fn memory_promoted(
    lineage_id: &str,
    from: MemoryClass,
    to: MemoryClass,
    confidence: u8,
    audit_event_id: &str,
) -> EventLine {
    EventLine {
        glyph: "◆",
        tone: Tone::Muted,
        strong: true,
        body: format!(
            "memory promoted {lineage_id} {} → {} (conf {}), now prompt-injected",
            from.as_str(),
            to.as_str(),
            fraction(confidence)
        ),
        detail: Some(format!("audit event {audit_event_id}")),
    }
}

/// A `0..=100` confidence as the two-decimal fraction SPEC 6.3 states, so the
/// number reads the same on this door as on the deck.
fn fraction(confidence: u8) -> String {
    format!("{:.2}", f64::from(confidence) / 100.0)
}

/// Whichever of the two memory events this is, worded for the plain door.
///
/// Takes the event rather than destructured fields so [`super::event_line`]'s
/// arm is one line: that file is at its ceiling, and two destructuring arms
/// cost twenty lines it does not have. The same shape, for the same reason,
/// as the deck's `model::memory::fold_memory_write`.
pub(super) fn line(event: &AgentEvent) -> Option<EventLine> {
    match event {
        AgentEvent::MemoryLogged {
            memory_id,
            text,
            class,
            confidence,
            ..
        } => Some(memory_logged(memory_id, text, *class, *confidence)),
        AgentEvent::MemoryPromoted {
            lineage_id,
            from,
            to,
            confidence,
            audit_event_id,
            ..
        } => Some(memory_promoted(
            lineage_id,
            *from,
            *to,
            *confidence,
            audit_event_id,
        )),
        // Unreachable through `event_line`, which routes only the two above.
        // A silent arm rather than a panic: this renders a stream a newer
        // binary may have written, and it may never take the session down.
        _ => None,
    }
}
