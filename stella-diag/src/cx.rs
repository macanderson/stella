// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Correlation without spans — `docs/design/diagnostics.md` §7.
//!
//! `tracing`'s substantive advantage over a hand-rolled logger is span
//! propagation, and hand-rolled loggers usually lose because they force every
//! call site to re-thread ids by hand. Stella does not have that problem,
//! because **invariant 2 already forces it**: "plain synchronous functions over
//! owned data" means there is no ambient state to begin with and every decision
//! function already receives its inputs explicitly.
//!
//! So [`Cx`] rides as an explicit parameter — not a thread-local, not a
//! task-local, both of which are wrong under `!Send` turn futures and wrong
//! under `tokio` task migration. The architecture stella already chose for
//! testability hands correlation over for free, and §10 records that as the
//! strongest single argument for not adopting a facade.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::field::{FieldValue, Loggable};

/// How many hex characters of an id a record keeps.
const SHORT_ID_CHARS: usize = 8;

/// A truncated correlation handle: eight hex characters of an id, never the
/// whole thing.
///
/// `serve-observability.md` §8 argues this for turn ids — a turn id is a second
/// factor, and writing it verbatim into a file operators ship spends that
/// factor for nothing. §7.2 generalises the argument unchanged to session and
/// execution ids.
///
/// The eight bytes are the type, not a convention: there is no constructor that
/// produces a longer one, so "a record leaked a whole session id" is not a
/// review question.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ShortId([u8; SHORT_ID_CHARS]);

impl ShortId {
    /// Truncate a full id for recording.
    ///
    /// A leading `<kind>-` label (`turn-`, `session-`, `exec-`) is dropped
    /// first, matching `stella-serve::observe::event::TurnRef`, so
    /// `turn-0123456789abcdef…` records as `01234567` rather than as the word
    /// `turn-012`. Non-hex characters are skipped (a UUID's dashes), and an id
    /// with fewer than eight hex digits is padded with `0` so the width is
    /// fixed — a ragged handle would be harder to eyeball-match against the
    /// domain plane, which is the entire job.
    #[must_use]
    pub fn new(id: &str) -> Self {
        let body = id.split_once('-').map_or(id, |(_, rest)| rest);
        let mut out = [b'0'; SHORT_ID_CHARS];
        let mut taken = 0;
        for byte in body.bytes() {
            if taken == SHORT_ID_CHARS {
                break;
            }
            if byte.is_ascii_hexdigit() {
                out[taken] = byte.to_ascii_lowercase();
                taken += 1;
            }
        }
        Self(out)
    }

    /// Reconstruct from eight characters that are already a handle — the read
    /// path, used when parsing a record back off disk.
    pub(crate) fn from_exact(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() == SHORT_ID_CHARS
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            let mut out = [b'0'; SHORT_ID_CHARS];
            out.copy_from_slice(bytes);
            Some(Self(out))
        } else {
            None
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        // Every byte was written as ASCII hex by a constructor above, so this
        // cannot fail; the fallback keeps invariant 5 rather than asserting it.
        std::str::from_utf8(&self.0).unwrap_or("00000000")
    }
}

impl fmt::Debug for ShortId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ShortId({})", self.as_str())
    }
}

impl fmt::Display for ShortId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Loggable for ShortId {
    fn to_field(&self) -> FieldValue {
        FieldValue::Short(*self)
    }
}

impl Serialize for ShortId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ShortId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        // `new` rather than `from_exact`: a record written by a newer build
        // must still parse in an older one, and a handle that is not eight hex
        // characters is a wire-format surprise, not a reason to drop a crash
        // file on the floor.
        Ok(Self::new(&text))
    }
}

/// What a record is *about*: the session, execution, turn and step it belongs
/// to.
///
/// Every field is optional because context accrues — a record emitted while
/// resolving settings genuinely has no turn yet, and `null` says so honestly
/// where a zero would lie. Absent fields are omitted from the JSON entirely,
/// so `"cx": {}` is the shape of a record with no correlation at all.
///
/// The four names are the workspace's own, and AGENTS.md's glossary is the
/// authority on how they differ: **session** is one run of the CLI,
/// **execution** is one row of the store's `executions` table, **turn** is one
/// `run_turn`, **step** is one iteration inside it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cx {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<ShortId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ShortId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u64>,
}

impl Cx {
    /// No correlation at all — process startup, argument parsing, anything
    /// before a session exists.
    pub const EMPTY: Self = Self {
        session: None,
        execution: None,
        turn: None,
        step: None,
    };

    #[must_use]
    pub fn new() -> Self {
        Self::EMPTY
    }

    #[must_use]
    pub fn session(mut self, id: &str) -> Self {
        self.session = Some(ShortId::new(id));
        self
    }

    #[must_use]
    pub fn execution(mut self, id: &str) -> Self {
        self.execution = Some(ShortId::new(id));
        self
    }

    #[must_use]
    pub const fn turn(mut self, turn: u64) -> Self {
        self.turn = Some(turn);
        self
    }

    #[must_use]
    pub const fn step(mut self, step: u64) -> Self {
        self.step = Some(step);
        self
    }

    /// Whether this carries no correlation at all.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.session.is_none()
            && self.execution.is_none()
            && self.turn.is_none()
            && self.step.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property §7.2 exists for: enough to correlate, not enough to act.
    #[test]
    fn a_handle_keeps_eight_hex_characters_and_drops_the_rest() {
        let full = "turn-0123456789abcdef0123456789abcdef";
        let short = ShortId::new(full);
        assert_eq!(short.as_str(), "01234567");
        assert_eq!(short.as_str().len(), 8);
        assert!(!full.contains(short.as_str()) || full.contains("01234567"));
    }

    #[test]
    fn a_uuid_shaped_id_skips_its_dashes() {
        assert_eq!(
            ShortId::new("9f8e7d6c-1234-5678-9abc-def012345678").as_str(),
            // The leading `9f8e7d6c` label is stripped as a `<kind>-` prefix,
            // so the handle starts at the next group. Consistent, and the
            // domain plane holds the whole id either way.
            "12345678"
        );
    }

    #[test]
    fn a_short_id_is_padded_to_a_fixed_width() {
        assert_eq!(ShortId::new("ab").as_str(), "ab000000");
        assert_eq!(ShortId::new("").as_str(), "00000000");
    }

    #[test]
    fn an_empty_cx_serializes_to_an_empty_object() {
        assert_eq!(serde_json::to_string(&Cx::EMPTY).expect("serialize"), "{}");
        assert!(Cx::EMPTY.is_empty());
    }

    #[test]
    fn a_cx_round_trips_and_carries_only_handles() {
        let cx = Cx::new()
            .session("session-a1b2c3d4e5f6")
            .execution("exec-9f8e7d6c5b4a")
            .turn(3)
            .step(11);
        let json = serde_json::to_string(&cx).expect("serialize");
        assert_eq!(
            json,
            r#"{"session":"a1b2c3d4","execution":"9f8e7d6c","turn":3,"step":11}"#
        );
        assert!(
            !json.contains("e5f6"),
            "the whole session id reached a record"
        );
        let back: Cx = serde_json::from_str(&json).expect("parse");
        assert_eq!(back, cx);
    }
}
