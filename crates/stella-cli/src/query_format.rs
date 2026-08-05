//! The shared machine-output idiom for query commands (#1568).
//!
//! Two different promises share the word "format" in this CLI, and this
//! module exists to keep them from blurring:
//!
//! - **Turn output** — `--output-format <text|json|stream-json>` — belongs to
//!   the commands that run the agent (`run`, `fleet`; `arena` is fixed
//!   stream-json). It is declared per-command since #1493 and its JSON
//!   envelope is versioned by [`crate::SUMMARY_SCHEMA_VERSION`].
//! - **Query output** — `--format <text|json[|csv]>` — belongs to the
//!   read-only commands that report local state (`stats`, `stats graph`,
//!   `usage report`, `inspect`, `calibration`, `memory list`, `context
//!   list|validate`, `scripts list`). Before #1568 this idiom had sprawled
//!   into three hand-rolled enums plus three `--json` booleans, and most of
//!   the JSON shapes carried no version a consumer could branch on.
//!
//! Every query command declares one of the two enums below, spelled
//! `--format`, defaulting to `text`, and wraps its JSON in the versioned
//! envelope ([`Rows`] for array payloads, [`Versioned`] for object payloads).
//! The one deliberate exemption is `scripts list`: its frame is shared
//! byte-for-byte with the `list_scripts` tool and pinned at its own
//! `schema_version` by `docs/spec/scripts-index.md`, so wrapping it here
//! would version the same object twice.
//!
//! A tree-walking test in `src/tests.rs`
//! (`query_commands_share_one_format_idiom`) fails the build if a `--json`
//! boolean reappears or a `--format` flag strays from this vocabulary.

use clap::ValueEnum;
use serde::Serialize;

/// The version of the query-command JSON envelope this build emits. Every
/// [`Rows`] and [`Versioned`] payload carries it as its first key.
///
/// Bump rules are the same as [`crate::SUMMARY_SCHEMA_VERSION`]'s, and for
/// the same reason (#644): increment only when a consumer written against
/// the previous version could break — a key removed or renamed, a value's
/// type changed, or a key's meaning changed under a stable name. Never bump
/// for a purely additive key; consumers must ignore keys they do not
/// recognize.
pub(crate) const QUERY_SCHEMA_VERSION: u32 = 1;

/// How a query command renders: human text or the versioned JSON envelope.
///
/// `table` parses as an alias of `text` — half of these surfaces spelled
/// their human rendering "table" before #1568 converged the vocabulary, and
/// a scripted `--format table` should not break over a word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum QueryFormat {
    /// Human-readable rendering (default).
    #[value(alias = "table")]
    Text,
    /// Pretty-printed JSON under the versioned envelope.
    Json,
}

/// [`QueryFormat`] plus CSV, for the tabular telemetry surfaces (`stats`,
/// `stats graph`, `usage report`) where a spreadsheet is a first-class
/// consumer. Same spelling, same alias, same envelope on the JSON arm; CSV
/// stays a bare RFC-4180 body (a header row is its version story).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum StatsFormat {
    /// Aligned human-readable columns (default).
    #[value(alias = "table")]
    Text,
    /// Pretty-printed JSON under the versioned envelope.
    Json,
    /// RFC-4180-style CSV with a header row.
    Csv,
}

/// The query envelope for an array payload:
/// `{"schema_version":N,"rows":[…]}`.
///
/// A struct rather than `serde_json::json!` so `schema_version` leads the
/// object — the same courtesy-not-contract stance as the turn envelope
/// (order stays outside the contract; consumers read by key).
#[derive(Serialize)]
pub(crate) struct Rows<'a, T: Serialize> {
    schema_version: u32,
    rows: &'a [T],
}

impl<'a, T: Serialize> Rows<'a, T> {
    pub(crate) fn new(rows: &'a [T]) -> Self {
        Self {
            schema_version: QUERY_SCHEMA_VERSION,
            rows,
        }
    }
}

/// The query envelope for an object payload: `schema_version` first, then
/// the payload's own fields flattened in their declaration order.
#[derive(Serialize)]
pub(crate) struct Versioned<T: Serialize> {
    schema_version: u32,
    #[serde(flatten)]
    payload: T,
}

impl<T: Serialize> Versioned<T> {
    pub(crate) fn new(payload: T) -> Self {
        Self {
            schema_version: QUERY_SCHEMA_VERSION,
            payload,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_envelope_leads_with_the_version() {
        let rendered = serde_json::to_string(&Rows::new(&[1, 2, 3])).unwrap();
        assert_eq!(rendered, r#"{"schema_version":1,"rows":[1,2,3]}"#);
    }

    #[test]
    fn versioned_envelope_flattens_the_payload_after_the_version() {
        #[derive(Serialize)]
        struct Payload {
            b: u32,
            a: u32,
        }
        let rendered = serde_json::to_string(&Versioned::new(Payload { b: 2, a: 1 })).unwrap();
        assert_eq!(rendered, r#"{"schema_version":1,"b":2,"a":1}"#);
    }

    #[test]
    fn table_parses_as_an_alias_of_text_on_both_enums() {
        assert_eq!(
            QueryFormat::from_str("table", true).unwrap(),
            QueryFormat::Text
        );
        assert_eq!(
            StatsFormat::from_str("table", true).unwrap(),
            StatsFormat::Text
        );
    }
}
