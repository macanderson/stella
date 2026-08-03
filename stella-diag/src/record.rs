// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The envelope — `docs/design/diagnostics.md` §5.1.
//!
//! One shape, shared by all seventeen crates:
//!
//! ```json
//! {
//!   "ts": 1754006400123,
//!   "level": "warn",
//!   "code": "store.migration.retry",
//!   "target": "stella_store::migrate",
//!   "cx": { "session": "a1b2c3d4", "execution": "9f8e7d6c", "turn": 3, "step": 11 },
//!   "fields": { "attempt": 2, "backoff_ms": 250 }
//! }
//! ```
//!
//! The **facets** — the per-crate typed enums that render into it — are each
//! crate's own. That is the delta from `serve-observability.md`: one closed
//! `ServeEvent` enum holds at crate scope and breaks at workspace scope,
//! because a shared god-enum makes every new variant recompile the world and
//! stops any crate adding an event without editing a crate it does not own
//! (§4, row 1). Nobody edits a shared enum here, and emission stays typed —
//! which is the property that made the serve design testable in the first
//! place, since a test asserts on a value and never on scraped stderr.
//!
//! [`Record::code`] is a **stable, documented public surface**, in the way
//! `rustc`'s `E0308` is: operators alert on it, docs link it, and it outlives
//! the message text (§11).

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::cx::Cx;
use crate::field::{Fields, StaticStr};
use crate::level::Level;

/// One diagnostic record.
///
/// Note what is **not** here: a message. A record is a `code` plus typed
/// fields, so the thing operators match on cannot drift with the prose, and
/// there is no `String` slot for a well-meaning contributor to drop a path
/// into.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// Epoch milliseconds — the workspace's existing convention
    /// (`stella-store::sessions`), so this needs no time crate.
    pub ts: u64,
    pub level: Level,
    /// The stable diagnostic code, dotted: `store.migration.retry`.
    pub code: StaticStr,
    /// The emitting module, from `module_path!()`: `stella_store::migrate`.
    /// This is what [`Filter`](crate::Filter) matches on.
    pub target: StaticStr,
    #[serde(default)]
    pub cx: Cx,
    #[serde(default)]
    pub fields: Fields,
}

impl Record {
    /// Build a record stamped with the current time.
    #[must_use]
    pub fn new(
        level: Level,
        code: &'static str,
        target: &'static str,
        cx: Cx,
        fields: Fields,
    ) -> Self {
        Self::at(now_millis(), level, code, target, cx, fields)
    }

    /// Build a record at an explicit timestamp.
    ///
    /// The seam tests use: a property over records needs a `ts` it chose, and
    /// a clock read inside the constructor would make every such property
    /// nondeterministic.
    #[must_use]
    pub fn at(
        ts: u64,
        level: Level,
        code: &'static str,
        target: &'static str,
        cx: Cx,
        fields: Fields,
    ) -> Self {
        Self {
            ts,
            level,
            code: StaticStr::new(code),
            target: StaticStr::new(target),
            cx,
            fields,
        }
    }

    /// Roughly how many bytes this record occupies once rendered.
    ///
    /// An estimate on purpose: the crash ring needs a *bound*, and serializing
    /// every record twice to get an exact one would double the cost of the
    /// plane to buy precision nobody consumes. It is NOT an upper bound,
    /// though: every field is charged a flat 24 bytes plus its key, so a wide
    /// value (a `Reviewed` note, a long `&'static str`, a `List`/`Map`)
    /// renders larger than it is charged and the ring can exceed its byte
    /// ceiling — `MAX_RECORDS` is the backstop that actually binds then.
    #[must_use]
    pub fn estimated_bytes(&self) -> usize {
        /// `ts`, `level`, the punctuation, and the four `cx` keys.
        const ENVELOPE_OVERHEAD: usize = 96;
        /// Quotes, colon and comma around each field, plus a value most fields
        /// stay under.
        const PER_FIELD_OVERHEAD: usize = 24;

        let fields: usize = self
            .fields
            .iter()
            .map(|(key, _)| key.len() + PER_FIELD_OVERHEAD)
            .sum();
        ENVELOPE_OVERHEAD + self.code.as_str().len() + self.target.as_str().len() + fields
    }
}

/// Milliseconds since the Unix epoch, or `0` if the clock is before it.
///
/// A clock that cannot be read is not a reason to lose a record: `0` is an
/// obviously-wrong timestamp on a record that still carries its code, fields
/// and correlation, which is strictly more than nothing (invariant 5, and the
/// same posture `stella-serve::observe::sink` already takes).
#[must_use]
pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Loggable;

    #[test]
    fn the_envelope_renders_exactly_as_the_design_specifies() {
        let record = Record::at(
            1_754_006_400_123,
            Level::Warn,
            "store.migration.retry",
            "stella_store::migrate",
            Cx::new()
                .session("session-a1b2c3d4ffff")
                .execution("exec-9f8e7d6caaaa")
                .turn(3)
                .step(11),
            Fields::new()
                .with("attempt", 2_u64)
                .with("backoff_ms", 250_u64),
        );
        assert_eq!(
            serde_json::to_string(&record).expect("serialize"),
            concat!(
                r#"{"ts":1754006400123,"level":"warn","code":"store.migration.retry","#,
                r#""target":"stella_store::migrate","#,
                r#""cx":{"session":"a1b2c3d4","execution":"9f8e7d6c","turn":3,"step":11},"#,
                r#""fields":{"attempt":2,"backoff_ms":250}}"#
            )
        );
    }

    /// Invariant 4, and property 8 of §12.
    #[test]
    fn a_record_round_trips() {
        let record = Record::at(
            1,
            Level::Debug,
            "agent.tool.call",
            "stella_cli::agent",
            Cx::new().turn(2),
            Fields::new()
                .with("tool", "bash")
                .with("args_bytes", 412_u64)
                .with("ok", true)
                .with("elapsed", std::time::Duration::from_millis(9)),
        );
        let json = serde_json::to_string(&record).expect("serialize");
        let back: Record = serde_json::from_str(&json).expect("parse");
        assert_eq!(back, record);
        assert_eq!(serde_json::to_string(&back).expect("re-serialize"), json);
    }

    /// A record with no fields and no correlation is still a legal record —
    /// the shape most startup diagnostics have.
    #[test]
    fn a_bare_record_round_trips() {
        let record = Record::at(
            0,
            Level::Info,
            "cli.boot",
            "stella_cli",
            Cx::EMPTY,
            Fields::new(),
        );
        let json = serde_json::to_string(&record).expect("serialize");
        assert_eq!(
            json,
            r#"{"ts":0,"level":"info","code":"cli.boot","target":"stella_cli","cx":{},"fields":{}}"#
        );
        assert_eq!(
            serde_json::from_str::<Record>(&json).expect("parse"),
            record
        );
    }

    #[test]
    fn the_size_estimate_bounds_the_rendered_length() {
        let record = Record::at(
            1_754_006_400_123,
            Level::Warn,
            "store.migration.retry",
            "stella_store::migrate",
            Cx::new().session("session-a1b2c3d4").turn(3),
            Fields::new()
                .with("attempt", 2_u64)
                .with("reason", "locked"),
        );
        let rendered = serde_json::to_vec(&record).expect("serialize").len();
        assert!(
            record.estimated_bytes() >= rendered,
            "the estimate ({}) must not undercount the render ({rendered})",
            record.estimated_bytes()
        );
    }

    #[test]
    fn a_clock_read_produces_a_plausible_timestamp() {
        // 2020-01-01, comfortably in the past and comfortably after 0.
        assert!(now_millis() > 1_577_836_800_000);
    }

    #[test]
    fn a_record_has_no_message_slot() {
        // Not a runtime assertion so much as a place to notice the intent: if
        // a `message: String` is ever added, this test's premise — and §5.2 —
        // is what it breaks.
        let record = Record::at(
            0,
            Level::Error,
            "x.y",
            "t",
            Cx::EMPTY,
            Fields::new().with("n", 1_u8),
        );
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&record).expect("serialize"))
                .expect("parse");
        let object = value.as_object().expect("an object");
        assert_eq!(object.len(), 6, "the envelope has exactly six keys");
        assert!(!object.contains_key("message"));
        assert_eq!(1_u8.to_field(), crate::FieldValue::Uint(1));
    }
}
