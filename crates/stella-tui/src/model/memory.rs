// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Folding the three events a memory write can announce itself with.
//!
//! One module rather than three arms in [`super::SessionModel::apply`], because
//! the three are one subject and the fold has to keep them consistent: they
//! feed one counter, and the receipt they feed must not count one write twice.
//! `model.rs` is a god file closed to growth, so a fold that grows with the
//! vocabulary belongs beside it (AGENTS.md § "God files").
//!
//! ## The three, and how they differ
//!
//! * [`AgentEvent::MemoryLogged`] names one memory: its id, its text, the rung
//!   it entered on, and what it would take to promote it. SPEC 6.3's `memory`
//!   (log) event.
//! * [`AgentEvent::MemoryPromoted`] says one lesson moved up the ladder and is
//!   now injected into the prompt as an instruction. SPEC 6.3's `memory`
//!   (promote) event.
//! * [`AgentEvent::ContextWrite`] is the aggregate the other two replace:
//!   `n facts · m superseded → provider`, with no id, no text and no rung.
//!   Nothing in this workspace emits it (`event::consumers` records the
//!   consumer gap as #4501; the producer gap is #5249), so its row is
//!   unreachable on every live path — kept because a stream recorded elsewhere
//!   may still carry it, and a transcript that dropped an event it can read is
//!   worse than one that renders the older shape.

use stella_protocol::AgentEvent;

use super::{SessionModel, TranscriptEntry};

impl SessionModel {
    /// Fold whichever of the three arrived, or leave the model untouched for
    /// anything else.
    ///
    /// Takes the whole event rather than destructured fields so `apply`'s arm
    /// is one line: the alternative is three arms in a file with a single
    /// line of headroom under its ceiling.
    pub(super) fn fold_memory_write(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::ContextWrite {
                provider,
                upserts,
                superseded,
                ..
            } => {
                self.turn_counters.memories = self.turn_counters.memories.saturating_add(*upserts);
                self.transcript.push(TranscriptEntry::ContextWrite {
                    provider: provider.clone(),
                    upserts: *upserts,
                    superseded: *superseded,
                });
            }
            AgentEvent::MemoryLogged {
                memory_id,
                text,
                class,
                confidence,
                kind,
                decays,
                promotes_at,
                ..
            } => {
                // The receipt's memory count, from the event that names each
                // memory. It came only from `ContextWrite::upserts` before,
                // which nothing emits — so `receipt … · n memories` had never
                // once rendered a number. Both feed it and neither
                // double-counts the other: a write is announced by one of
                // them, never by both.
                self.turn_counters.memories = self.turn_counters.memories.saturating_add(1);
                self.transcript.push(TranscriptEntry::MemoryLog {
                    memory_id: memory_id.clone(),
                    text: text.clone(),
                    class: *class,
                    confidence: *confidence,
                    kind: kind.clone(),
                    decays: *decays,
                    promotes_at: *promotes_at,
                });
            }
            AgentEvent::MemoryPromoted {
                lineage_id,
                from,
                to,
                confidence,
                audit_event_id,
                ..
            } => {
                // Not counted into the receipt's `memories`. A promotion moves
                // a memory the turn already counted when it logged it, or one
                // an earlier turn did; counting it again would report more
                // memories than the turn wrote.
                self.transcript.push(TranscriptEntry::MemoryPromote {
                    lineage_id: lineage_id.clone(),
                    from: *from,
                    to: *to,
                    confidence: *confidence,
                    audit_event_id: audit_event_id.clone(),
                });
            }
            // Unreachable through `apply`, which routes only the three above.
            // A silent arm rather than a panic: this is a fold over a stream a
            // newer binary may have written, and the one thing it may never do
            // is take the session down.
            _ => {}
        }
    }
}
