//! Per-call telemetry persistence and its execution-level trust boundary.

use rusqlite::params;

use crate::{Result, Store, sqlite_i64};

/// One StepUsage-shaped telemetry record (mirrors the event, plus the
/// derived cache-miss column so analytics never re-derive it).
#[derive(Debug, Clone, PartialEq)]
pub struct TelemetryRow {
    pub step: u64,
    pub provider: String,
    pub call_role: String,
    pub model: String,
    pub input_tokens: u64,
    /// The engine's raw pre-call estimate — paired with
    /// `input_tokens + cache_write_tokens` it is one drift sample
    /// ([`Store::drift_samples`]); 0 means no estimate.
    pub estimated_input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_miss_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost_usd: f64,
    pub duration_ms: u64,
    pub retries: u32,
    pub tool_calls: u64,
    pub usage_complete: bool,
}

/// One source-store telemetry row addressed for hub replication: its stable
/// `rowid` (the replication cursor), owning execution, and the execution's
/// start time (the hub's day-bucketing timestamp — per-call telemetry rows
/// carry no clock of their own).
#[derive(Debug, Clone, PartialEq)]
pub struct SourceTelemetryRow {
    pub source_rowid: i64,
    pub execution_id: i64,
    pub recorded_at: String,
    pub telemetry: TelemetryRow,
}

impl Store {
    /// Record one uniquely identified model call's telemetry.
    pub fn record_telemetry(&self, execution_id: i64, row: &TelemetryRow) -> Result<()> {
        let step = sqlite_i64("telemetry step", row.step)?;
        let input_tokens = sqlite_i64("telemetry input tokens", row.input_tokens)?;
        let estimated_input_tokens = sqlite_i64(
            "telemetry estimated input tokens",
            row.estimated_input_tokens,
        )?;
        let output_tokens = sqlite_i64("telemetry output tokens", row.output_tokens)?;
        let cache_read_tokens = sqlite_i64("telemetry cache-read tokens", row.cache_read_tokens)?;
        let cache_miss_tokens = sqlite_i64("telemetry cache-miss tokens", row.cache_miss_tokens)?;
        let cache_write_tokens =
            sqlite_i64("telemetry cache-write tokens", row.cache_write_tokens)?;
        let duration_ms = sqlite_i64("telemetry duration", row.duration_ms)?;
        let tool_calls = sqlite_i64("telemetry tool calls", row.tool_calls)?;
        self.lock().execute(
            "INSERT INTO telemetry (execution_id, step, provider, call_role, model, input_tokens, \
             estimated_input_tokens, output_tokens, cache_read_tokens, cache_miss_tokens, \
             cache_write_tokens, cost_usd, duration_ms, retries, tool_calls, usage_complete) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                execution_id,
                step,
                row.provider,
                row.call_role,
                row.model,
                input_tokens,
                estimated_input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_miss_tokens,
                cache_write_tokens,
                row.cost_usd,
                duration_ms,
                row.retries,
                tool_calls,
                row.usage_complete,
            ],
        )?;
        Ok(())
    }

    /// Every telemetry row above the replication cursor, oldest first,
    /// capped at `limit` — the batch feed for
    /// [`Store::replicate_telemetry_to_usage`].
    pub fn telemetry_rows_after(
        &self,
        after_rowid: i64,
        limit: usize,
    ) -> Result<Vec<SourceTelemetryRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT t.rowid, t.execution_id, COALESCE(e.started_at, ''), t.step, t.provider, \
                    t.call_role, t.model, t.input_tokens, t.estimated_input_tokens, \
                    t.output_tokens, t.cache_read_tokens, t.cache_miss_tokens, \
                    t.cache_write_tokens, t.cost_usd, t.duration_ms, t.retries, t.tool_calls, \
                    t.usage_complete \
             FROM telemetry t LEFT JOIN executions e ON e.id = t.execution_id \
             WHERE t.rowid > ?1 ORDER BY t.rowid ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![after_rowid, limit as i64], |r| {
            Ok(SourceTelemetryRow {
                source_rowid: r.get(0)?,
                execution_id: r.get(1)?,
                recorded_at: r.get(2)?,
                telemetry: TelemetryRow {
                    step: r.get::<_, i64>(3)? as u64,
                    provider: r.get(4)?,
                    call_role: r.get(5)?,
                    model: r.get(6)?,
                    input_tokens: r.get::<_, i64>(7)? as u64,
                    estimated_input_tokens: r.get::<_, i64>(8)? as u64,
                    output_tokens: r.get::<_, i64>(9)? as u64,
                    cache_read_tokens: r.get::<_, i64>(10)? as u64,
                    cache_miss_tokens: r.get::<_, i64>(11)? as u64,
                    cache_write_tokens: r.get::<_, i64>(12)? as u64,
                    cost_usd: r.get(13)?,
                    duration_ms: r.get::<_, i64>(14)? as u64,
                    retries: r.get(15)?,
                    tool_calls: r.get::<_, i64>(16)? as u64,
                    usage_complete: r.get(17)?,
                },
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Recent complete (estimated, actual) input-token pairs, oldest first.
    ///
    /// The actual side is `input_tokens + cache_write_tokens`: cache writes
    /// are real prompt tokens the provider read (they are split out of
    /// `input_tokens` by adapters for *pricing*, not because they were not
    /// sent), and serving the bare column fed calibration a falsely low
    /// actual on every cache-writing step — worst on a cache-enabled
    /// session's first call, where nearly the whole prompt is a cache write
    /// and the resulting ~0 ratio seeded the EWMA with garbage that inflated
    /// the effective compaction budget past the provider's context window.
    /// Summing here also cleans already-recorded history on replay, since
    /// both columns have always been persisted.
    pub fn drift_samples(
        &self,
        provider: &str,
        model: &str,
        limit: usize,
    ) -> Result<Vec<(u64, u64)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT estimated_input_tokens, input_tokens + cache_write_tokens FROM (
               SELECT estimated_input_tokens, input_tokens, cache_write_tokens,
                      execution_id, step
               FROM telemetry
               WHERE provider = ? AND model = ? AND usage_complete = 1
                 AND estimated_input_tokens > 0 AND input_tokens + cache_write_tokens > 0
               ORDER BY execution_id DESC, step DESC
               LIMIT ?
             ) ORDER BY execution_id ASC, step ASC",
        )?;
        let rows = stmt.query_map(params![provider, model, limit as i64], |row| {
            let estimated: i64 = row.get(0)?;
            let actual: i64 = row.get(1)?;
            Ok((estimated as u64, actual as u64))
        })?;
        let mut samples = Vec::new();
        for row in rows {
            samples.push(row?);
        }
        Ok(samples)
    }

    /// Close an execution record with a complete local accounting envelope.
    pub fn finish_execution(&self, execution_id: i64, outcome: &str, cost_usd: f64) -> Result<()> {
        self.finish_execution_accounted(execution_id, outcome, cost_usd, true)
    }

    /// Close an execution while monotonically carrying renderer/forwarder
    /// persistence completeness into the durable export gate.
    pub fn finish_execution_accounted(
        &self,
        execution_id: i64,
        outcome: &str,
        cost_usd: f64,
        usage_complete: bool,
    ) -> Result<()> {
        self.lock().execute(
            "UPDATE executions SET finished_at = CURRENT_TIMESTAMP, outcome = ?1, cost_usd = ?2, \
                    usage_complete = CASE \
                      WHEN usage_status = 'incomplete' OR NOT ?3 THEN 0 ELSE 1 END, \
                    usage_status = CASE \
                      WHEN usage_status = 'incomplete' OR NOT ?3 \
                      THEN 'incomplete' ELSE 'complete' END WHERE id = ?4",
            params![outcome, cost_usd, usage_complete, execution_id],
        )?;
        Ok(())
    }

    /// The spend one execution has already SETTLED: the sum of the durable
    /// per-call receipts it wrote as it ran.
    ///
    /// `executions.cost_usd` is written once, at close-out, so a driver that
    /// died mid-turn leaves it at its `0` default while the `telemetry` rows
    /// beside it record real money. This is the recovery read for that case —
    /// a dead fleet worker's attempt reports what its receipts prove instead
    /// of `$0`, which is the direction that under-counts a `--budget` gate
    /// (#1216). An execution with no landed model call settles at `0.0`,
    /// which is the truth and not a fallback.
    pub fn execution_settled_cost_usd(&self, execution_id: i64) -> Result<f64> {
        self.lock()
            .query_row(
                "SELECT COALESCE(SUM(cost_usd), 0) FROM telemetry WHERE execution_id = ?1",
                params![execution_id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Permanently downgrade one execution's accounting state.
    pub fn mark_execution_usage_incomplete(&self, execution_id: i64) -> Result<()> {
        self.lock().execute(
            "UPDATE executions SET usage_complete = 0, usage_status = 'incomplete' WHERE id = ?1",
            params![execution_id],
        )?;
        Ok(())
    }

    /// Durable completeness bit used by local and enterprise projections.
    pub fn execution_usage_complete(&self, execution_id: i64) -> Result<bool> {
        self.lock()
            .query_row(
                "SELECT finished_at IS NOT NULL AND usage_complete = 1 \
                        AND usage_status = 'complete' FROM executions WHERE id = ?1",
                params![execution_id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(step: u64, cost_usd: f64) -> TelemetryRow {
        TelemetryRow {
            step,
            provider: "zai".into(),
            call_role: "worker".into(),
            model: "glm-5.2".into(),
            input_tokens: 1_000,
            estimated_input_tokens: 900,
            output_tokens: 100,
            cache_read_tokens: 0,
            cache_miss_tokens: 1_000,
            cache_write_tokens: 0,
            cost_usd,
            duration_ms: 500,
            retries: 0,
            tool_calls: 1,
            usage_complete: true,
        }
    }

    /// The recovery read for a driver that died mid-turn: nothing closed the
    /// execution row, so `executions.cost_usd` is still at its `0` default —
    /// while the per-call receipts underneath it are durable and add up to
    /// real money.
    #[test]
    fn settled_cost_sums_the_receipts_of_an_execution_that_never_closed() {
        let store = Store::in_memory().unwrap();
        let id = store
            .begin_execution("fleet", "p", "zai", "glm-5.2")
            .unwrap();
        store.record_telemetry(id, &row(0, 0.02)).unwrap();
        store.record_telemetry(id, &row(1, 0.03)).unwrap();

        let summary = store.execution_summary(id).unwrap().unwrap();
        assert_eq!(summary.cost_usd, 0.0, "the unclosed row reads $0");
        assert!(
            (store.execution_settled_cost_usd(id).unwrap() - 0.05).abs() < 1e-9,
            "its receipts do not"
        );
    }

    /// No landed model call settles at `$0` — the truth, not a fallback — and
    /// an execution that does not exist reads the same way rather than
    /// erroring.
    #[test]
    fn settled_cost_with_no_receipts_is_zero() {
        let store = Store::in_memory().unwrap();
        let id = store
            .begin_execution("fleet", "p", "zai", "glm-5.2")
            .unwrap();

        assert_eq!(store.execution_settled_cost_usd(id).unwrap(), 0.0);
        assert_eq!(store.execution_settled_cost_usd(id + 404).unwrap(), 0.0);
    }
}
