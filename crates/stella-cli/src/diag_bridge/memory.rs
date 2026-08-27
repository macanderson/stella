// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! SPEC 6.3's two memory events, as diagnostics.
//!
//! A sibling module rather than two more arms in `diag_bridge.rs`, which sits
//! against its 1500-line limit and takes no baseline entry (AGENTS.md § "God
//! files").
//!
//! Neither record carries the memory's id or its text. `stella-diag`'s field
//! values cannot hold a `String` at all — a compile error, not a review
//! question — and that is the rule keeping a lesson the model wrote about
//! somebody's code out of the diagnostic stream. What a diagnostic reader
//! needs is the *shape* of the write: which rung, how confident, how far from
//! promoting. The transcript is where a reader learns which lesson.

use stella_diag::Level;
use stella_protocol::MemoryClass;

use super::DomainBridge;

/// One memory landed in the context store.
///
/// `Debug`: a log is the ordinary case, and an operator watching at the
/// default level is not asking to see every lesson the loop writes down.
///
/// The `.with` chain is written out here rather than shared with [`promoted`]
/// through a helper, even though both name `class` and `confidence` the same
/// way. `make diag-reference` reads these calls literally to generate each
/// code's field list in `docs/reference/diagnostics.md`, so a field assembled
/// behind a function call is a field the reference silently stops listing —
/// the record keeps carrying it and the documentation stops saying so.
pub(super) fn logged(
    bridge: &DomainBridge,
    class: MemoryClass,
    confidence: u8,
    decays: bool,
    promotes_at: u8,
) {
    bridge.emit(
        Level::Debug,
        "agent.memory.logged",
        bridge
            .at_seq()
            .with("class", class.as_str())
            .with("confidence", u32::from(confidence))
            .with("decays", decays)
            .with("promotes_at", u32::from(promotes_at)),
    );
}

/// A memory moved up the ladder.
///
/// `Info`, unlike the log above: a promotion is when an inferred lesson starts
/// being injected into the prompt as an instruction, so it is the moment
/// something the loop guessed begins steering every later turn. An operator
/// reading diagnostics at the default level needs to see that happen.
pub(super) fn promoted(bridge: &DomainBridge, from: MemoryClass, to: MemoryClass, confidence: u8) {
    bridge.emit(
        Level::Info,
        "agent.memory.promoted",
        bridge
            .at_seq()
            .with("from", from.as_str())
            .with("to", to.as_str())
            .with("confidence", u32::from(confidence)),
    );
}
