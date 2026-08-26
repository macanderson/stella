// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! One metering row for tests to vary, instead of four copies of the same
//! twenty-line literal.
//!
//! [`TelemetryRow`] is exhaustive by design — a new column has to be answered
//! for at every construction site — and four crate tests had each written the
//! whole literal out. Adding #4924's three join columns meant editing all four,
//! two of which live in files closed to growth (`src/tests.rs` and
//! `src/usage.rs` are grandfathered god files; AGENTS.md § "God files"). One
//! builder plus `..` update syntax is both the smaller diff and the better
//! shape: a test that varies `cost_usd` now says only that.
//!
//! `#[cfg(test)]` rather than an `#[allow(dead_code)]` builder on the public
//! type: nothing ships a call to this, and the compiler should keep it that
//! way (AGENTS.md § "Code style", on why `cfg(test)` beats a suppression).

use super::TelemetryRow;

/// A complete, plausible metering row: one worker call that read some cache,
/// settled, and named no delegate and no receipt.
///
/// Vary what a test is about and leave the rest:
///
/// ```ignore
/// TelemetryRow { cost_usd: 3.0, ..metering_row(7, "zai", "glm-5.2") }
/// ```
///
/// The receipt identity is `None` — "this row cannot say" — because that is
/// the shape a fixture that has not opted into the join should have. A test
/// about the join sets all three.
pub(crate) fn metering_row(stream_seq: u64, provider: &str, model: &str) -> TelemetryRow {
    TelemetryRow {
        stream_seq,
        turn_instance: None,
        engine_step: None,
        call_seq: None,
        provider: provider.into(),
        call_role: "worker".into(),
        model: model.into(),
        input_tokens: 1_000,
        estimated_input_tokens: 900,
        output_tokens: 100,
        cache_read_tokens: 600,
        cache_miss_tokens: 400,
        cache_write_tokens: 0,
        cost_usd: 0.001,
        duration_ms: 800,
        retries: 0,
        tool_calls: 1,
        usage_complete: true,
        sub_agent_id: None,
    }
}
