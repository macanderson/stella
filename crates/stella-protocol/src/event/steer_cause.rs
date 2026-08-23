// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Why a turn was steered.
//!
//! Split out of `event.rs` for the reason [`super::ModelCallRole`]'s module
//! gives: the parent is a god file under the size ratchet, and a vocabulary
//! belongs beside its own enumeration rather than in the middle of the variant
//! list.

use serde::{Deserialize, Serialize};

// Doc-link target only: the variant is named in this module's docs but not
// used in its code. `cfg(doc)` keeps rustdoc's intra-doc link resolving
// without an import a normal build would flag as unused.
#[cfg(doc)]
use super::AgentEvent;

/// What put the message into the turn, for [`AgentEvent::Steered`].
///
/// Three different things emit that event and they are different pathologies
/// with different remedies: a person interrupting, the stuck-loop rung, and
/// the stalled-turn rung (#2022). Before this field the only way to tell them
/// apart was to match the English prose — and `STALL_STEER_PREFIX` *extends*
/// `LOOP_STEER_PREFIX`, so even the prefix test was a substring test on a
/// sentence (#3622). That is precisely the practice
/// [`AgentEvent::LoopDetected`] was introduced to end for the loop rung.
///
/// The consequence was that "the turn was steered" was one bucket, so no
/// consumer could answer *how often does the stall rung actually fire* — the
/// question that decides whether the rung earns its keep.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SteerCause {
    /// Not recorded: an event written before this field existed.
    ///
    /// The default for an **absent** `cause` only, matching
    /// [`super::ModelCallRole::Unknown`]'s precedent for exactly this
    /// situation.
    ///
    /// [`Self::User`] would have been the cheaper default and is the wrong
    /// one: every `Steered` recorded before this field came from one of the
    /// two automatic rungs, so defaulting to `User` would relabel the entire
    /// recorded history as human input — and the tallies that read it would
    /// be wrong in the one direction that matters.
    // The doc above links a Rust path, and schemars publishes a doc comment
    // verbatim as the schema `description` — where no consumer of
    // `--output-format stream-json` can resolve one. So the wire reader gets a
    // description written for them, and the Rust reader keeps the link.
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Not recorded: an event written before this field existed. The default for an absent `cause` only — never `user`, which would relabel a whole recorded history as human input."
        )
    )]
    #[default]
    Unknown,
    /// A person's mid-turn message, drained from
    /// `stella_core::ports::TurnSteering` at a step boundary.
    User,
    /// The stuck-loop rung's nudge, following a `LoopVerdict`. Paired with an
    /// [`AgentEvent::LoopDetected`] carrying the same evidence.
    Loop,
    /// The stalled-turn rung's nudge (#2022): no loop, but the turn has spent
    /// too long waiting. Has no typed twin, which is why it was
    /// indistinguishable from [`Self::Loop`] on the wire.
    Stall,
}

impl SteerCause {
    /// Whether this steer is something a person did.
    ///
    /// The one predicate a surface should ask before attributing a steer to
    /// the user — SPEC 6.1's opening rule labels a queued steer, and a rule
    /// that named a stall-rung auto-steer as something the user typed would be
    /// worse than the blank it replaces (#4185).
    ///
    /// [`Self::Unknown`] is **not** a person: a replayed legacy session keeps
    /// the blank it has today, which is the honest outcome rather than a
    /// guess.
    #[must_use]
    pub fn is_from_a_person(self) -> bool {
        matches!(self, Self::User)
    }
}
