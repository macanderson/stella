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

/// What one execution's durable receipts prove it spent, as a scalar
/// subquery over `telemetry` keyed on `?1`.
///
/// Both writers of `executions.cost_usd` embed this, so "what the receipts
/// prove" has exactly one definition in the crate — a second copy is how the
/// rollup and the ledger it summarizes drift apart again.
const RECEIPTS_TOTAL_USD: &str =
    "(SELECT COALESCE(SUM(cost_usd), 0) FROM telemetry WHERE execution_id = ?1)";

impl Store {
    /// Record one uniquely identified model call's telemetry, and raise the
    /// owning execution's cost rollup to what its receipts now prove.
    ///
    /// The rollup is **recomputed** from the ledger, never incremented: a
    /// repeat of this statement lands the same number, where a `+=` would
    /// double-count real money. And it only ever moves **up** —
    /// `executions.cost_usd` is a lower bound on what the run cost, and
    /// neither of its two writers may lower it (see
    /// [`Store::finish_execution_accounted`] for the other one).
    ///
    /// Before #2570 that column was written once, at close-out, from the
    /// driver's in-memory aggregate. A cancelled turn drops its turn future
    /// while the long-running roles are still settling, so the aggregate held
    /// only the stages that had already finished: execution 99 recorded
    /// $0.8659773 of a $5.28786465 run — to the digit the
    /// triage + research + plan prefix — while all 116 receipts underneath it
    /// were durable and `usage_complete`. The bias is one-directional and
    /// worst where it costs most, because the roles in flight when a cancel
    /// lands are the expensive ones. Maintaining the rollup as each call
    /// settles removes both halves of that failure: the total no longer
    /// depends on when the driver stopped looking, and a receipt that lands
    /// *after* close-out (the forwarder drain outruns the closeout on the
    /// cancel path) still repairs it.
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
        let mut conn = self.lock();
        // One transaction, because the receipt and the rollup it raises are
        // one fact: a reader must never see the ledger without the total it
        // now implies.
        let tx = conn.transaction()?;
        tx.execute(
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
        tx.execute(
            &format!(
                "UPDATE executions SET cost_usd = MAX(cost_usd, {RECEIPTS_TOTAL_USD}) WHERE id = ?1"
            ),
            params![execution_id],
        )?;
        tx.commit()?;
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
    ///
    /// `cost_usd` is a **floor, not an assignment**: the row keeps the larger
    /// of the caller's aggregate and what the execution's receipts prove
    /// ([`Store::execution_settled_cost_usd`]). Both are lower bounds on real
    /// spend and each can miss what the other saw — the aggregate misses calls
    /// still settling when a cancel lands (#2570), the ledger misses a call
    /// whose telemetry write failed, which is exactly what `usage_complete`
    /// records — so the larger of the two is the tightest honest answer. The
    /// asymmetry is deliberate: under-reporting spend is the direction that
    /// flatters us, and a measurement layer that discards the arm under test
    /// cannot answer the question it exists for.
    pub fn finish_execution_accounted(
        &self,
        execution_id: i64,
        outcome: &str,
        cost_usd: f64,
        usage_complete: bool,
    ) -> Result<()> {
        self.lock().execute(
            &format!(
                "UPDATE executions SET finished_at = CURRENT_TIMESTAMP, outcome = ?2, \
                        cost_usd = MAX(?3, {RECEIPTS_TOTAL_USD}), \
                        usage_complete = CASE \
                          WHEN usage_status = 'incomplete' OR NOT ?4 THEN 0 ELSE 1 END, \
                        usage_status = CASE \
                          WHEN usage_status = 'incomplete' OR NOT ?4 \
                          THEN 'incomplete' ELSE 'complete' END WHERE id = ?1"
            ),
            params![execution_id, outcome, cost_usd, usage_complete],
        )?;
        Ok(())
    }

    /// Record which wrapper variant ran this turn (#3388) — called right
    /// after [`Store::begin_execution`] on a wrapped run, and not at all on
    /// an unwrapped one.
    ///
    /// # NULL means "no wrapper", and that is a fact
    ///
    /// The raw turn loop is the ordinary case, so a NULL here is a positive
    /// statement rather than a missing measurement. That is why nothing
    /// writes a placeholder string for the unwrapped case: `'none'` and
    /// "nobody recorded it" would then be indistinguishable, which is the
    /// exact confusion this column was split out of `kind` to end.
    ///
    /// # Why this lives beside the accounting updates
    ///
    /// `kind` and this column are the two axes every per-variant comparison
    /// groups by, and this module already owns the execution row's
    /// column-level updates. It is deliberately not in `lib.rs`: that file is
    /// a grandfathered god file closed to growth.
    pub fn set_pipeline_variant(&self, execution_id: i64, variant: &str) -> Result<()> {
        self.lock().execute(
            "UPDATE executions SET pipeline_variant = ? WHERE id = ?",
            params![variant, execution_id],
        )?;
        Ok(())
    }

    /// Open an execution row for a **standalone system model call** — one
    /// that is not a turn: no prompt the user wrote, and no door they came in
    /// by (`reflection`, `skill_author`, `domain_inference`, `agent_author`,
    /// `ingest_extraction`).
    ///
    /// The counterpart of [`Store::begin_execution`](crate::Store::begin_execution),
    /// and the reason this is a second constructor rather than a parameter on
    /// that one: these rows differ in *what the columns mean*, not in a
    /// setting. Callers used to reach `begin_execution` with the role in the
    /// `kind` argument, which is precisely how the door column came to hold
    /// four role values (#3395).
    ///
    /// `kind` is written as the non-door sentinel and `role` carries the
    /// role, so a caller cannot write a role into the door column by
    /// mistake — the door is not theirs to choose.
    pub fn begin_standalone_execution(
        &self,
        role: &str,
        prompt: &str,
        provider: &str,
        model: &str,
    ) -> Result<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO executions \
               (kind, role, prompt, provider, model, usage_complete, usage_status, journal_era) \
             VALUES (?, ?, ?, ?, ?, 0, 'pending', ?)",
            params![
                crate::migrations::SYSTEM_NON_DOOR,
                role,
                prompt,
                provider,
                model,
                crate::JournalEra::CURRENT.code()
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// The spend one execution has already SETTLED: the sum of the durable
    /// per-call receipts it wrote as it ran.
    ///
    /// This is the receipts side on its own, and it stays a distinct question
    /// from `executions.cost_usd` even now that
    /// [`Store::record_telemetry`] keeps that rollup at or above this figure
    /// (#2570): the rollup is a floor over *both* sources, so it can also
    /// carry spend the caller's aggregate saw and no receipt records. Callers
    /// that must answer "what did this run's durable receipts prove?" — a
    /// dead fleet worker's attempt reporting settled spend instead of `$0`,
    /// which is the direction that under-counts a `--spend-limit` gate (#1216) —
    /// ask here. An execution with no landed model call settles at `0.0`,
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
    /// execution row, and the per-call receipts underneath it are durable and
    /// add up to real money.
    ///
    /// Before #2570 the rollup on an unclosed row was still `0` — the `0`
    /// this read exists to route around. It now tracks the receipts as they
    /// land, so the two agree here; the read stays distinct because the
    /// rollup is a floor over the receipts *and* the driver's aggregate,
    /// while this is the receipts alone.
    #[test]
    fn settled_cost_sums_the_receipts_of_an_execution_that_never_closed() {
        let store = Store::in_memory().unwrap();
        let id = store
            .begin_execution("fleet", "p", "zai", "glm-5.2")
            .unwrap();
        store.record_telemetry(id, &row(0, 0.02)).unwrap();
        store.record_telemetry(id, &row(1, 0.03)).unwrap();

        let summary = store.execution_summary(id).unwrap().unwrap();
        assert!(
            (store.execution_settled_cost_usd(id).unwrap() - 0.05).abs() < 1e-9,
            "the receipts add up"
        );
        assert!(
            (summary.cost_usd - 0.05).abs() < 1e-9,
            "and the unclosed row already says so"
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

    /// One receipt per role, priced and labelled as the run's stages settle.
    fn role_receipt(step: u64, call_role: &str, cost_usd: f64) -> TelemetryRow {
        TelemetryRow {
            call_role: call_role.into(),
            ..row(step, cost_usd)
        }
    }

    /// **Witness for #2570.** Execution 99's shape, scaled to five roles: the
    /// cheap early stages settle, the expensive ones (worker, witness author)
    /// settle behind them, and then a cancel closes the row with the driver's
    /// in-memory aggregate — which holds only the prefix that had finished
    /// when the turn future was dropped.
    ///
    /// Every one of those calls was paid for and every receipt is durable, so
    /// the closed row must report all of them. On the old code the aggregate
    /// was written verbatim and the row reported the prefix: 16.4% of the
    /// real spend, biased low by exactly the roles that cost the most.
    #[test]
    fn a_cancelled_execution_reports_every_receipt_it_paid_for() {
        let store = Store::in_memory().unwrap();
        let id = store.begin_execution("run", "p", "zai", "glm-5.2").unwrap();
        let roles = [
            ("triage", 0.11),
            ("research", 0.31),
            ("plan", 0.44),
            ("worker", 3.02),
            ("witness_author", 1.41),
        ];
        for (step, (call_role, cost_usd)) in roles.iter().enumerate() {
            store
                .record_telemetry(id, &role_receipt(step as u64, call_role, *cost_usd))
                .unwrap();
        }
        // What the driver's aggregate held: the stages that completed before
        // the worker began.
        let pre_cancel_prefix = 0.11 + 0.31 + 0.44;

        store
            .finish_execution_accounted(id, "cancelled", pre_cancel_prefix, false)
            .unwrap();

        let summary = store.execution_summary(id).unwrap().unwrap();
        let receipts = store.execution_settled_cost_usd(id).unwrap();
        assert!(
            (receipts - 5.29).abs() < 1e-9,
            "fixture check: the receipts total $5.29, got {receipts}"
        );
        assert!(
            (summary.cost_usd - receipts).abs() < 1e-9,
            "the cancelled row must report every receipt it paid for: \
             reported ${}, receipts prove ${receipts}",
            summary.cost_usd
        );
        assert_eq!(
            summary.outcome.as_deref(),
            Some("cancelled"),
            "reconciling the cost changes nothing about the outcome"
        );
        assert!(
            !store.execution_usage_complete(id).unwrap(),
            "nor about the completeness bit the caller asked for"
        );
    }

    /// A receipt that lands **after** close-out still repairs the rollup.
    ///
    /// This is the other half of #2570's mechanism and the reason the floor
    /// is maintained per receipt rather than only reconciled at finish: on
    /// the cancel path the driver closes the execution row without awaiting
    /// the forwarder's drain, so the last calls' telemetry can be written
    /// after `finish_execution_accounted` has already run. A rollup fixed
    /// only at close-out would still lose them.
    #[test]
    fn a_receipt_landing_after_closeout_still_raises_the_rollup() {
        let store = Store::in_memory().unwrap();
        let id = store.begin_execution("run", "p", "zai", "glm-5.2").unwrap();
        store
            .record_telemetry(id, &role_receipt(0, "triage", 0.25))
            .unwrap();
        store
            .finish_execution_accounted(id, "cancelled", 0.25, false)
            .unwrap();

        store
            .record_telemetry(id, &role_receipt(1, "worker", 4.0))
            .unwrap();

        let summary = store.execution_summary(id).unwrap().unwrap();
        assert!(
            (summary.cost_usd - 4.25).abs() < 1e-9,
            "the late receipt heals the closed row, got ${}",
            summary.cost_usd
        );
        assert!(
            summary.finished_at.is_some(),
            "and does not reopen it — the row stays closed"
        );
    }

    /// The rollup is a floor over *both* sources, so an aggregate that saw
    /// spend no receipt records survives untouched. That case is real: a
    /// telemetry write can fail where the call itself settled, which is what
    /// `usage_complete = false` exists to say. Reconciling *down* to the
    /// receipts would turn that flag into a silent $-loss.
    #[test]
    fn a_rollup_above_its_receipts_is_never_lowered_to_them() {
        let store = Store::in_memory().unwrap();
        let id = store.begin_execution("run", "p", "zai", "glm-5.2").unwrap();
        store
            .record_telemetry(id, &role_receipt(0, "worker", 0.10))
            .unwrap();

        store
            .finish_execution_accounted(id, "completed", 0.40, false)
            .unwrap();

        let summary = store.execution_summary(id).unwrap().unwrap();
        assert!(
            (summary.cost_usd - 0.40).abs() < 1e-9,
            "the higher of the two lower bounds wins, got ${}",
            summary.cost_usd
        );
    }

    /// The cheap standing guard #2570 asks for: for **every** execution, at
    /// **every** outcome — closed, open, cancelled, never-run — the rollup is
    /// at least what the receipts prove. One disagreement in the wrong
    /// direction is money the store has been paid for and does not report.
    #[test]
    fn no_execution_reports_less_than_its_receipts_at_any_outcome() {
        let store = Store::in_memory().unwrap();
        let seed = |outcome: Option<&str>, costs: &[f64], reported: f64| {
            let id = store.begin_execution("run", "p", "zai", "glm-5.2").unwrap();
            for (step, cost) in costs.iter().enumerate() {
                store
                    .record_telemetry(id, &row(step as u64, *cost))
                    .unwrap();
            }
            if let Some(outcome) = outcome {
                store
                    .finish_execution_accounted(id, outcome, reported, false)
                    .unwrap();
            }
            id
        };
        let ids = [
            seed(Some("completed"), &[0.5, 0.25], 0.75),
            seed(Some("cancelled"), &[0.5, 2.25], 0.5),
            seed(Some("aborted"), &[1.5], 0.0),
            seed(Some("failed"), &[], 0.0),
            seed(None, &[0.125, 0.125], 0.0),
        ];

        for id in ids {
            let reported = store.execution_summary(id).unwrap().unwrap().cost_usd;
            let receipts = store.execution_settled_cost_usd(id).unwrap();
            assert!(
                reported >= receipts - 1e-9,
                "execution {id} reports ${reported} against ${receipts} of receipts"
            );
        }
    }
}
