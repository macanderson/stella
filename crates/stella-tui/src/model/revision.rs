// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The scrollback record of a plan revision a failing gate put up (SPEC 8.1
//! item 3).
//!
//! Not part of [`SessionModel::apply`]'s fold, because a proposal is not an
//! `AgentEvent`: `stella_store::plan_graph::RevisionGate` authors it
//! host-side from a `GateBoard` the host already evaluated, and it reaches the
//! deck on `Inbound::RevisionProposed`. A replayed event log therefore shows
//! the gate board that provoked the proposal rather than the proposal — the
//! same shape `Inbound::GraphSnapshot` and the rest of the out-of-band
//! envelopes have. Routing it as an event would put a host-side reading into the
//! session's durable journal, where a second producer could then write one.
//!
//! The *withholding* the proposal implies is `DeckUi::pending_revisions`', not
//! this row's. State a key press clears belongs to the interaction layer
//! (`model`'s own module doc draws that line), and this is the record of what
//! was asked.

use stella_protocol::RevisionProposal;

use super::{SessionModel, TranscriptEntry};

impl SessionModel {
    /// File the proposal in the scrollback.
    ///
    /// A superseded proposal's row stays where it is: it is what the reader
    /// was looking at, and deleting it would leave an action row they had
    /// half-answered with no trace of what it asked.
    pub fn propose_revision(&mut self, proposal: RevisionProposal) {
        self.transcript
            .push(TranscriptEntry::RevisionProposal { proposal });
        self.evict_transcript_overflow();
    }
}
