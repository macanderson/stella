//! Per-call telemetry persistence and its execution-level trust boundary.

use rusqlite::OptionalExtension as _;
use rusqlite::params;

use crate::{Result, Store, sqlite_i64};

/// Whether a run shipped anything — the other half of what `executions` says
/// about a finished run, apart from how it ended (#2808).
mod delivery;

pub use delivery::Delivery;

#[cfg(test)]
pub(crate) mod fixtures;

/// One StepUsage-shaped telemetry record (mirrors the event, plus the
/// derived cache-miss column so analytics never re-derive it).
#[derive(Debug, Clone, PartialEq)]
pub struct TelemetryRow {
    /// The event-stream `seq` — the execution-global call identity, and this
    /// row's half of `UNIQUE (execution_id, stream_seq)`.
    ///
    /// Called `step` until #4924, which is what it was never holding: the
    /// engine's step restarts on every `run_turn` and several calls can share
    /// one, so keying on it would collide, and a collision here double-counts
    /// money. AGENTS.md § Glossary is the authority on how far apart `step`,
    /// `turn_instance` and `call_seq` are. For the engine's own step, see
    /// [`TelemetryRow::engine_step`].
    pub stream_seq: u64,
    /// The `run_turn` this call rode — `step_receipt.turn_instance`.
    ///
    /// `None` is "this row cannot say", never turn 0: a row written before
    /// #4924, or a dead call the engine never got to name. Turn instances are
    /// 0-based, so collapsing the two would claim every legacy row rode the
    /// first turn. Same contract as the event's own field
    /// (`AgentEvent::StepUsage::turn_instance`).
    pub turn_instance: Option<u32>,
    /// The engine's step within that turn — `step_receipt.step`.
    ///
    /// Spelled apart from `step` because this table's column of that name
    /// held a seq for thirty-six schema versions, and reusing the word here
    /// would rebuild the confusion the rename removed. `None` is "cannot
    /// say".
    pub engine_step: Option<u64>,
    /// Which of the calls sharing `(turn_instance, engine_step)` this one was
    /// — `step_receipt.call_seq`. The engine's worker call is 0; the
    /// auxiliary calls riding the same step (the overflow summarizer, a
    /// plugin's management roles) take 1, 2, …
    ///
    /// `None` is "cannot say" and must not be read as the worker, for the
    /// reason `AgentEvent::StepUsage::call_seq` gives: a row written before
    /// the field existed may equally have been an auxiliary call's.
    pub call_seq: Option<u64>,
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
    /// Which delegate spent this call; `None` is the lead's own (#4383).
    ///
    /// Local to `store.db` on purpose: it is deliberately **not** replicated
    /// to the usage hub, which is a cross-project surface and has its own
    /// reviewed column allowlist (`content_free.rs`). A handle like
    /// `plugin:vera/worker#0` names a plugin and an ordinal, which is a fact
    /// about this workspace's configuration and has no business in a rollup.
    pub sub_agent_id: Option<String>,
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
        let stream_seq = sqlite_i64("telemetry stream seq", row.stream_seq)?;
        let engine_step = row
            .engine_step
            .map(|step| sqlite_i64("telemetry engine step", step))
            .transpose()?;
        let call_seq = row
            .call_seq
            .map(|seq| sqlite_i64("telemetry call seq", seq))
            .transpose()?;
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
            "INSERT INTO telemetry (execution_id, stream_seq, turn_instance, engine_step, \
             call_seq, provider, call_role, model, input_tokens, \
             estimated_input_tokens, output_tokens, cache_read_tokens, cache_miss_tokens, \
             cache_write_tokens, cost_usd, duration_ms, retries, tool_calls, usage_complete, \
             sub_agent_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                execution_id,
                stream_seq,
                row.turn_instance,
                engine_step,
                call_seq,
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
                row.sub_agent_id,
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
            "SELECT t.rowid, t.execution_id, COALESCE(e.started_at, ''), t.stream_seq, \
                    t.turn_instance, t.engine_step, t.call_seq, t.provider, \
                    t.call_role, t.model, t.input_tokens, t.estimated_input_tokens, \
                    t.output_tokens, t.cache_read_tokens, t.cache_miss_tokens, \
                    t.cache_write_tokens, t.cost_usd, t.duration_ms, t.retries, t.tool_calls, \
                    t.usage_complete, t.sub_agent_id \
             FROM telemetry t LEFT JOIN executions e ON e.id = t.execution_id \
             WHERE t.rowid > ?1 ORDER BY t.rowid ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![after_rowid, limit as i64], |r| {
            Ok(SourceTelemetryRow {
                source_rowid: r.get(0)?,
                execution_id: r.get(1)?,
                recorded_at: r.get(2)?,
                telemetry: TelemetryRow {
                    stream_seq: r.get::<_, i64>(3)? as u64,
                    turn_instance: r.get::<_, Option<i64>>(4)?.map(|v| v as u32),
                    engine_step: r.get::<_, Option<i64>>(5)?.map(|v| v as u64),
                    call_seq: r.get::<_, Option<i64>>(6)?.map(|v| v as u64),
                    provider: r.get(7)?,
                    call_role: r.get(8)?,
                    model: r.get(9)?,
                    input_tokens: r.get::<_, i64>(10)? as u64,
                    estimated_input_tokens: r.get::<_, i64>(11)? as u64,
                    output_tokens: r.get::<_, i64>(12)? as u64,
                    cache_read_tokens: r.get::<_, i64>(13)? as u64,
                    cache_miss_tokens: r.get::<_, i64>(14)? as u64,
                    cache_write_tokens: r.get::<_, i64>(15)? as u64,
                    cost_usd: r.get(16)?,
                    duration_ms: r.get::<_, i64>(17)? as u64,
                    retries: r.get(18)?,
                    tool_calls: r.get::<_, i64>(19)? as u64,
                    usage_complete: r.get(20)?,
                    sub_agent_id: r.get(21)?,
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
                      execution_id, stream_seq
               FROM telemetry
               WHERE provider = ? AND model = ? AND usage_complete = 1
                 AND estimated_input_tokens > 0 AND input_tokens + cache_write_tokens > 0
               ORDER BY execution_id DESC, stream_seq DESC
               LIMIT ?
             ) ORDER BY execution_id ASC, stream_seq ASC",
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

    /// Record which wrapper ran this turn (#3388) — called right
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
    /// `kind` and this column are the two axes every per-pipeline comparison
    /// groups by, and this module already owns the execution row's
    /// column-level updates. It is not in `lib.rs`: that file is
    /// a grandfathered god file closed to growth.
    pub fn set_pipeline_variant(&self, execution_id: i64, variant: &str) -> Result<()> {
        self.lock().execute(
            "UPDATE executions SET pipeline_variant = ? WHERE id = ?",
            params![variant, execution_id],
        )?;
        Ok(())
    }

    /// Which wrapper ran over one execution, or `None` for a turn nothing
    /// wrapped.
    ///
    /// The reader the column shipped without. Every consumer so far reads it
    /// through the observatory's own SQL, so nothing in this crate's API could
    /// tell whether a write had landed — which is how the only writer came to
    /// pass a constant for two years' worth of releases without a test that
    /// could notice (#3494).
    ///
    /// # Errors
    ///
    /// The underlying `rusqlite` failure. A missing execution id is `Ok(None)`,
    /// not an error: "no such row" and "that row had no wrapper" are both
    /// honestly "nothing ran over it here".
    pub fn pipeline_variant(&self, execution_id: i64) -> Result<Option<String>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT pipeline_variant FROM executions WHERE id = ?",
                params![execution_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
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
    use crate::StepManifestRow as ManifestRow;

    fn row(stream_seq: u64, cost_usd: f64) -> TelemetryRow {
        TelemetryRow {
            stream_seq,
            turn_instance: None,
            engine_step: None,
            call_seq: None,
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
            sub_agent_id: None,
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
    fn role_receipt(stream_seq: u64, call_role: &str, cost_usd: f64) -> TelemetryRow {
        TelemetryRow {
            call_role: call_role.into(),
            ..row(stream_seq, cost_usd)
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

    /// One receipt for a call at `(turn, step, call_seq)`, priced so the two
    /// in the witness below cannot be confused for each other.
    fn receipt_at(turn_instance: u32, step: u64, call_seq: u64, call_role: &str) -> ManifestRow {
        ManifestRow {
            turn_instance,
            step,
            call_seq,
            provider: "zai".into(),
            upstream_provider: None,
            model: "glm-5.2".into(),
            call_role: call_role.into(),
            effective_budget_tokens: 100_000,
            calibration_factor: 1.0,
            estimated_input_tokens: 900,
            stall_seconds_requested: None,
            compiled_frame_id: None,
            frame_hash: None,
            blocks: Vec::new(),
        }
    }

    /// The metering row for that same call, addressed by the stream seq and
    /// carrying the engine-side identity beside it.
    fn cost_of(stream_seq: u64, turn: u32, step: u64, call_seq: u64, usd: f64) -> TelemetryRow {
        TelemetryRow {
            turn_instance: Some(turn),
            engine_step: Some(step),
            call_seq: Some(call_seq),
            ..row(stream_seq, usd)
        }
    }

    /// **Witness (#4924).** A cost can be joined to the receipt of the call
    /// that produced it, on `step_receipt`'s own primary key.
    ///
    /// The case that earns it is the one `call_seq` exists for: two calls
    /// share one `(turn_instance, step)` — the engine's worker and the
    /// overflow summarizer riding the same step — and they cost different
    /// money. Before this change a telemetry row carried only
    /// `execution_id` and the stream seq, so nothing in the store could say
    /// which of the two receipts a given cost belonged to; a reader had to go
    /// back to `stella-events.jsonl` and re-derive it from the event stream.
    ///
    /// Fails on the old schema for the plainest possible reason: the three
    /// columns the join names do not exist, so the statement does not prepare.
    #[test]
    fn a_cost_joins_to_the_receipt_of_the_call_that_produced_it() {
        let store = Store::in_memory().unwrap();
        let id = store.begin_execution("run", "p", "zai", "glm-5.2").unwrap();

        // Two calls on step 4 of turn 2, and one on step 5, so the join has
        // something to get wrong.
        for receipt in [
            receipt_at(2, 4, 0, "worker"),
            receipt_at(2, 4, 1, "summarizer"),
            receipt_at(2, 5, 0, "worker"),
        ] {
            store.record_step_manifest(id, &receipt).unwrap();
        }
        store
            .record_telemetry(id, &cost_of(7, 2, 4, 0, 3.00))
            .unwrap();
        store
            .record_telemetry(id, &cost_of(8, 2, 4, 1, 0.05))
            .unwrap();
        store
            .record_telemetry(id, &cost_of(9, 2, 5, 0, 1.50))
            .unwrap();

        let conn = store.lock();
        let mut stmt = conn
            .prepare(
                "SELECT sr.call_role, t.cost_usd
                 FROM telemetry t
                 JOIN step_receipt sr
                   ON sr.execution_id = t.execution_id
                  AND sr.turn_instance = t.turn_instance
                  AND sr.step = t.engine_step
                  AND sr.call_seq = t.call_seq
                 WHERE t.execution_id = ?1 AND t.turn_instance = 2 AND t.engine_step = 4
                 ORDER BY sr.call_seq ASC",
            )
            .expect("the join key exists");
        let paired: Vec<(String, f64)> = stmt
            .query_map(params![id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(
            paired.len(),
            2,
            "both calls on this step must find their own receipt, got {paired:?}"
        );
        assert_eq!(paired[0].0, "worker");
        assert!(
            (paired[0].1 - 3.00).abs() < 1e-9,
            "the expensive call is the worker's, got {paired:?}"
        );
        assert_eq!(paired[1].0, "summarizer");
        assert!(
            (paired[1].1 - 0.05).abs() < 1e-9,
            "and the cheap one is the summarizer's, got {paired:?}"
        );
    }

    /// The other direction: a row that cannot say which call it was stays
    /// unjoined rather than being attributed to turn 0's worker.
    ///
    /// This is what makes NULL the right absent value. `usage_incomplete`
    /// writes exactly this shape — the call died before the engine named a
    /// turn — and so does every row written before v37. Defaulting the three
    /// columns to 0 would have silently joined all of them to
    /// `(turn 0, step 0, call_seq 0)`, which is a real receipt on almost
    /// every execution.
    #[test]
    fn a_row_that_cannot_say_is_not_attributed_to_turn_zero() {
        let store = Store::in_memory().unwrap();
        let id = store.begin_execution("run", "p", "zai", "glm-5.2").unwrap();
        store
            .record_step_manifest(id, &receipt_at(0, 0, 0, "worker"))
            .unwrap();
        // The dead call: recorded, priced, and unjoinable.
        store.record_telemetry(id, &row(3, 0.0)).unwrap();

        let conn = store.lock();
        let joined: i64 = conn
            .query_row(
                "SELECT count(*)
                 FROM telemetry t
                 JOIN step_receipt sr
                   ON sr.execution_id = t.execution_id
                  AND sr.turn_instance = t.turn_instance
                  AND sr.step = t.engine_step
                  AND sr.call_seq = t.call_seq
                 WHERE t.execution_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            joined, 0,
            "a NULL identity must not match a receipt — `turn 0, step 0, call 0` is a real call"
        );
        let recorded: i64 = conn
            .query_row(
                "SELECT count(*) FROM telemetry WHERE execution_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            recorded, 1,
            "and the row is still there — unjoinable, not absent"
        );
    }
}
