// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Which board task an event belongs to — the wire half of "a task is an
//! evidence ledger and a cost" (`design/tui-v2/SPEC.md` §7.1, §6.2).
//!
//! SPEC 7.1 gives a task three parts: a contract ([`crate::task_contract`]),
//! an evidence ledger, and a cost. The other two are selections over this tag,
//! and neither was expressible before it — no event named a task, so "what did
//! task 3 edit?" and "what did task 3 cost?" had nothing but a reader's guess
//! from timestamps behind them, which two concurrent lanes make unguessable.
//!
//! The cost it makes computable is `$ · tok · cache rd% · model calls · est
//! remain`, with **no `det %`**: that clause was specified once and dropped
//! for having no source, and nothing here reintroduces it.
//!
//! Never read an absent tag as a task. Board ids start at `"1"`, so absence
//! means the event is in no task's ledger, and a consumer must render nothing
//! rather than invent one. [`crate::event::task_tag`] says which cases carry
//! the field at all.

use serde::{Deserialize, Serialize};

/// The id of one row on the session task board — the per-session ordinal
/// (`"1"`, `"2"`, …) that a `task_update` snapshot's items carry.
///
/// Transparent on the wire: a tagged event carries a plain JSON string, so a
/// consumer joins it against a board snapshot's `id` directly, with no wrapper
/// object to unpick. Board ids start at `"1"` and are never reused within a
/// session, so an absent tag can never be confused with a real one.
//
// The doc comment above is published verbatim as this type's `description` in
// `docs/wire/agentevent.schema.json` and `agentevent.d.ts`, so it names no
// Rust path and carries no intra-doc link — the same discipline
// `event/payload.rs` keeps with its `cfg(doc)` imports, and for the same
// reason: a `crate::…` path in the wire contract describes a language the
// reader is not writing in.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskId(String);

/// # Why a newtype, and why it is a field rather than an envelope
///
/// A newtype for the reason [`crate::LaneId`] is one: this workspace already
/// has several ids that read alike (AGENTS.md's glossary), and an unwrapped
/// `String` is how the next one joins them. In particular this is **not**
/// `stella_fleet::TaskId`, which names a unit of work dispatched to a worker
/// inside a fleet run.
///
/// [`crate::journal::StampedEvent`] makes the opposite call for `ts`, and the
/// two look alike enough that the difference has to be named. A timestamp is a
/// fact about the **write**: one event reaches several sinks, and the engine
/// that produced it owns no clock. A task id is a fact about the **work** —
/// the engine dispatching a call is exactly the thing that knows which task it
/// is dispatching for — so it belongs to the event, is identical in every sink
/// the event reaches, and survives a replay that rewrites the line.
///
/// # The stamping contract
///
/// - **Optional, always.** `None` means no task was running when the event was
///   dispatched, or the stream predates the field.
/// - **Stamped once, at dispatch**, by whoever knows the running task — see
///   `stella_core::EventSender::attach_running_task`, where a host declares
///   that source for a turn's whole stream. A tag already present is never
///   overwritten, so a producer closer to the work outranks the ambient one.
/// - **Opaque.** A board ordinal, never instruction text: no consumer may read
///   meaning out of it beyond equality and the board's own numeric ordering.
impl TaskId {
    /// Wrap a board id.
    ///
    /// [`TaskItem`](crate::TaskItem)'s `id` is still a `String` and this is
    /// convertible from one, which is a seam rather than a design: unifying
    /// them is a mechanical change across every board consumer, tracked in
    /// #5159. Until then this crossing is the one place they meet, and it is
    /// total — a board id is already exactly this.
    ///
    /// Total, and unvalidated: what may be a board id is
    /// `stella_core::tasks::TaskBoard`'s decision (it mints them), and
    /// `stella-protocol` carries no logic by rule — a second validator here
    /// would be one rule in two places.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The id as written — the join key against the store's `tasks` rows and
    /// against a [`TaskItem`](crate::TaskItem)'s `id`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for TaskId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

impl From<&str> for TaskId {
    fn from(id: &str) -> Self {
        Self(id.to_owned())
    }
}

impl From<TaskId> for String {
    fn from(id: TaskId) -> Self {
        id.0
    }
}

/// Compare a tag against a board id still spelled as a `str`, so the crossing
/// documented above does not force an allocation at every call site that has
/// one side of it untyped.
impl PartialEq<str> for TaskId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for TaskId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AGENTS.md #4: everything crossing a boundary round-trips byte for
    /// byte. The shape being asserted is the *transparency* — a tag is a bare
    /// JSON string, not `{"0":"3"}` — because that is what lets an existing
    /// `task_update` consumer join the two without learning a new shape.
    #[test]
    fn a_task_id_is_a_bare_string_on_the_wire() {
        let id = TaskId::new("3");
        let json = serde_json::to_string(&id).expect("serialize");
        assert_eq!(json, r#""3""#);
        let back: TaskId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, id);
        assert_eq!(json, serde_json::to_string(&back).expect("re-serialize"));
    }

    /// The board crossing is total in both directions and loses nothing, which
    /// is what lets `TaskItem::id` stay a `String` until the two are unified.
    #[test]
    fn a_board_id_crosses_into_the_tag_and_back_unchanged() {
        let board_id = "12".to_string();
        let tag = TaskId::from(board_id.clone());
        assert_eq!(tag.as_str(), board_id);
        assert_eq!(tag, *"12");
        assert_eq!(tag.to_string(), board_id);
        assert_eq!(String::from(tag), board_id);
    }
}
