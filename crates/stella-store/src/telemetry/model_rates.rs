// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What one model has actually cost and taken in *this* workspace: dollars per
//! token and milliseconds per token, folded out of the `telemetry` rows the
//! store already holds.
//!
//! The caller is the ISSUES tab's start-work estimate (`design/tui-v2/SPEC.md`
//! §8.2), which prices a drafted plan before anything runs. It reads measured
//! rates rather than a published price list for the reason SPEC §1 gives about
//! `det %`: an estimate is only worth showing when a measurement stands behind
//! it. A published price is also the wrong number here twice over — it says
//! nothing about wall clock, and it ignores cache reads, which are most of a
//! long session's input tokens and are billed at a fraction.
//!
//! Rates, never totals. A rate divides two sums from the same rows, so it is
//! comparable across workspaces of wildly different age; a total would say
//! more about how long somebody has been using Stella than about the plan
//! being estimated.
//!
//! # What "no answer" means
//!
//! [`Store::model_rates`] answers `None` when this workspace has recorded no
//! **billable, timed** call for the model. That is a real answer and the
//! overlay renders it as one: a workspace on its first run has nothing to
//! price against, and `~$0.00 · ~0 min` would be a claim rather than an
//! absence. Rows with zero tokens are excluded rather than counted, because a
//! failed call that was never billed would drag both rates toward zero while
//! looking like evidence.

use rusqlite::OptionalExtension;

use crate::{Result, Store};

/// One model's measured cost and pace in this workspace.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelRates {
    /// Dollars per token, over every token the model was billed for.
    pub usd_per_token: f64,
    /// Milliseconds of wall clock per token.
    pub ms_per_token: f64,
    /// How many calls the two rates were folded from, so a caller can tell a
    /// rate backed by one call from one backed by a thousand.
    pub calls: u64,
}

impl Store {
    /// Fold this workspace's recorded calls to `model` into a cost rate and a
    /// pace rate, or `None` when it has recorded none it can divide by.
    ///
    /// Total tokens is input + output: an estimate prices a whole call, and
    /// splitting the rate by direction would need the caller to predict its
    /// own output size — a guess standing in for the measurement this exists
    /// to supply.
    pub fn model_rates(&self, model: &str) -> Result<Option<ModelRates>> {
        let conn = self.lock();
        let row: Option<(f64, i64, i64, i64)> = conn
            .query_row(
                "SELECT SUM(cost_usd), \
                        SUM(input_tokens + output_tokens), \
                        SUM(duration_ms), \
                        COUNT(*) \
                 FROM telemetry \
                 WHERE model = ?1 \
                   AND input_tokens + output_tokens > 0 \
                   AND duration_ms > 0",
                [model],
                |row| {
                    Ok((
                        row.get::<_, Option<f64>>(0)?.unwrap_or(0.0),
                        row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                        row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                        row.get(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((usd, tokens, duration_ms, calls)) = row else {
            return Ok(None);
        };
        if tokens <= 0 || calls <= 0 {
            return Ok(None);
        }
        let tokens = tokens as f64;
        Ok(Some(ModelRates {
            usd_per_token: usd / tokens,
            ms_per_token: duration_ms as f64 / tokens,
            calls: calls as u64,
        }))
    }
}

#[cfg(test)]
mod tests {
    use crate::Store;
    use crate::TelemetryRow;

    /// One recorded call: `n` input + `n` output tokens, `cost` dollars,
    /// `ms` of wall clock.
    fn record(store: &Store, execution: i64, seq: u64, model: &str, n: u64, cost: f64, ms: u64) {
        store
            .record_telemetry(
                execution,
                &TelemetryRow {
                    stream_seq: seq,
                    turn_instance: None,
                    engine_step: None,
                    call_seq: None,
                    provider: "test".into(),
                    call_role: "worker".into(),
                    model: model.into(),
                    input_tokens: n,
                    estimated_input_tokens: n,
                    output_tokens: n,
                    cache_read_tokens: 0,
                    cache_miss_tokens: 0,
                    cache_write_tokens: 0,
                    cost_usd: cost,
                    duration_ms: ms,
                    retries: 0,
                    tool_calls: 0,
                    usage_complete: true,
                    sub_agent_id: None,
                },
            )
            .expect("record");
    }

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path()).expect("store");
        (dir, store)
    }

    fn execution(store: &Store) -> i64 {
        store
            .begin_execution("deck", "goal", "test", "m1")
            .expect("execution")
    }

    #[test]
    fn the_rates_are_the_two_sums_divided_by_the_tokens_they_cover() {
        let (_dir, store) = store();
        let execution = execution(&store);
        record(&store, execution, 1, "m1", 100, 0.20, 4_000);
        record(&store, execution, 2, "m1", 300, 0.60, 12_000);
        let rates = store.model_rates("m1").expect("query").expect("some");
        // 0.80 over 800 tokens, 16s over 800 tokens.
        assert!((rates.usd_per_token - 0.001).abs() < 1e-9, "{rates:?}");
        assert!((rates.ms_per_token - 20.0).abs() < 1e-9, "{rates:?}");
        assert_eq!(rates.calls, 2);
    }

    #[test]
    fn a_model_this_workspace_has_never_called_has_no_rate() {
        let (_dir, store) = store();
        assert!(store.model_rates("never-run").expect("query").is_none());
    }

    /// A call billed nothing and timed nothing is not evidence about either
    /// rate, and counting it would pull both toward zero.
    #[test]
    fn untimed_and_unbilled_calls_are_excluded_rather_than_counted() {
        let (_dir, store) = store();
        let execution = execution(&store);
        record(&store, execution, 1, "m1", 0, 0.0, 0);
        assert!(store.model_rates("m1").expect("query").is_none());
        record(&store, execution, 2, "m1", 100, 0.40, 2_000);
        let rates = store.model_rates("m1").expect("query").expect("some");
        assert_eq!(rates.calls, 1, "only the timed, billed call: {rates:?}");
        assert!((rates.usd_per_token - 0.002).abs() < 1e-9, "{rates:?}");
    }

    #[test]
    fn one_models_rate_never_folds_in_anothers() {
        let (_dir, store) = store();
        let execution = execution(&store);
        record(&store, execution, 1, "m1", 100, 0.20, 1_000);
        record(&store, execution, 2, "m2", 100, 2.00, 90_000);
        let rates = store.model_rates("m1").expect("query").expect("some");
        assert_eq!(rates.calls, 1, "{rates:?}");
        assert!((rates.usd_per_token - 0.001).abs() < 1e-9, "{rates:?}");
    }
}
