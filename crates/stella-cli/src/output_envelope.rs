// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The `--output-format json|stream-json` summary envelope contract:
//! [`OutputFormat`] itself, the schema version every summary object carries,
//! and the pre-flight error envelope `main` emits before an agent exists.
//! Split out of `main.rs` (the `driver/settlement.rs` pattern, AGENTS.md §
//! God files) — the parent sat within a handful of lines of the 1500-line
//! ratchet. A pure move: every caller already reaches these through
//! `crate::OutputFormat` / `crate::SUMMARY_SCHEMA_VERSION` / etc., and
//! `main.rs`'s own re-export keeps every one of those paths working.

use clap::ValueEnum;

/// How turn output reaches the caller. `stream-json` is a line-per-`AgentEvent`
/// serialization of the exact protocol enum — a stable machine interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// Human-oriented interactive rendering (default).
    Text,
    /// One final JSON object summarizing the turn (non-interactive).
    Json,
    /// One JSON line per AgentEvent as it happens (non-interactive streaming).
    StreamJson,
}

/// Set once a machine-readable summary object has already reached stdout for
/// this process, so [`emit_error_summary`] never follows it with a second
/// envelope describing the same failure. `agent.rs` prints its summary and
/// then still returns `Err` for a verification failure or a hard pipeline
/// error, which would otherwise land in `main`'s catch-all twice.
static JSON_SUMMARY_EMITTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Record that a `--output-format json|stream-json` summary object has been
/// written to stdout.
pub(crate) fn note_json_summary_emitted() {
    JSON_SUMMARY_EMITTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// The version of the `--output-format json|stream-json` summary envelope this
/// build emits. Every summary object carries it — the pipeline summary, the raw
/// step-loop summary, the pre-flight error envelope, and the detached-launch
/// summary ([`crate::daemon::detach`]) — so a consumer can branch on the shape
/// instead of sniffing for keys.
///
/// All four envelopes are structs with the version declared first, so a derived
/// `Serialize` heads the object — a courtesy, not a promise: key order stays
/// outside the contract and consumers must read by key. Building any with
/// `serde_json::json!` would undo that (a `json!` object is a sorted map that
/// buries `schema_version` mid-envelope).
///
/// # When to bump
///
/// Increment only when a consumer written against the previous version could
/// break: a key removed or renamed, a value's type changed, or a key's *meaning*
/// changed while its name and type stay the same. Do **not** bump for a purely
/// additive key — consumers must ignore keys they do not recognize, so an
/// addition cannot break a correct client and bumping would burn the signal.
///
/// The raw summary's `withheld` key is the worked example of that
/// second arm. Nothing was removed, renamed or retyped, and no existing key changed
/// meaning; a v1 consumer reading the new envelope sees exactly what it saw
/// before plus a key it ignores. Bumping would have told every correct client
/// to re-read a contract that did not move.
///
/// The `events` array is out of scope: the event vocabulary carries its own
/// forward-compatibility contract and never bumps this number. The
/// consumer-facing statement lives in `website/content/docs/scripting.mdx`; keep
/// the two in step.
pub(crate) const SUMMARY_SCHEMA_VERSION: u32 = 1;

/// The stdout envelope for a failure under `--output-format json|stream-json`,
/// or `None` when the caller asked for human output (a text run must keep
/// stdout clean; its diagnostic is the stderr line).
///
/// Pre-configuration failures — no API key, unknown provider, unknown model, a
/// malformed settings file — are returned by `run()` before an agent exists,
/// so they never reach `agent.rs`'s summary. Emitting the same
/// `{"schema_version":…,"status":"error","text":null,"reason":…}` shape here
/// gives the single most likely non-interactive failure an envelope instead
/// of empty stdout. `stream-json` gets it compact, so the line-delimited
/// contract holds.
///
/// Built from a struct rather than `serde_json::json!` for the key order: a
/// `json!` object is a sorted map, which would bury `schema_version` in the
/// middle of the envelope, while a derived `Serialize` emits fields in
/// declaration order. Order is not part of the contract — consumers read by key
/// — but a version a human can see at a glance is worth the struct.
#[derive(serde::Serialize)]
struct PreflightErrorSummary<'a> {
    schema_version: u32,
    status: &'static str,
    text: Option<&'a str>,
    reason: &'a str,
}

pub(crate) fn error_summary_json(format: OutputFormat, msg: &str) -> Option<String> {
    let value = PreflightErrorSummary {
        schema_version: SUMMARY_SCHEMA_VERSION,
        status: "error",
        text: None,
        reason: msg,
    };
    // A struct of one integer and three string fields cannot fail to
    // serialize; the fallback keeps the contract rather than proving the point.
    let fallback = || format!(r#"{{"schema_version":{SUMMARY_SCHEMA_VERSION},"status":"error"}}"#);
    match format {
        OutputFormat::Text => None,
        OutputFormat::Json => {
            Some(serde_json::to_string_pretty(&value).unwrap_or_else(|_| fallback()))
        }
        OutputFormat::StreamJson => {
            Some(serde_json::to_string(&value).unwrap_or_else(|_| fallback()))
        }
    }
}

/// Print [`error_summary_json`] unless a summary already went out for this
/// failure.
///
/// `pub(crate)`, widened from the private visibility it had inside
/// `main.rs`: its one caller, `main()`, stays behind in the parent module
/// after this split, and `main.rs`'s own re-export keeps that call
/// unqualified.
pub(crate) fn emit_error_summary(format: OutputFormat, msg: &str) {
    if JSON_SUMMARY_EMITTED.load(std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    if let Some(line) = error_summary_json(format, msg) {
        println!("{line}");
    }
}
