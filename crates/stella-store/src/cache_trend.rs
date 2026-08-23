//! Per-session cache-economics trend — [`Store::session_cache_trend`] —
//! split out of `lib.rs` to keep that file small.
//!
//! `telemetry` already carries every call's cache read/write tokens keyed by
//! `execution_id`, and `executions.session_id` already chains a multi-turn
//! session's executions together — the persistence issue #267 asks for
//! ("persist per-session cache stats … so `stella stats` trends across
//! sessions") is already on disk, one row per call, forever. This module is
//! the read side: it groups that existing telemetry by session so
//! `stella-cli`'s `stats.rs` can render a hit-rate trend across a
//! workspace's recent sessions, exactly the way [`crate::cache_gaps`] groups
//! the same tables by call instead of by session. No TTL, pricing, or
//! diagnosis policy lives here — `stella-store` deliberately does not depend
//! on `stella-model`; this module only surfaces the facts that policy needs.

use crate::{Result, Store};

/// One session's aggregated cache facts — see [`Store::session_cache_trend`].
/// `started_at` is the session's first recorded execution's timestamp (SQLite
/// `CURRENT_TIMESTAMP` text, `YYYY-MM-DD HH:MM:SS`), for display only — the
/// row order is the authoritative recency signal (see the method docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCacheTrendRow {
    pub session_id: String,
    pub started_at: String,
    /// Executions recorded under this session (its turn count) — the real
    /// per-session turn count `stella-cli`'s low-hit-rate diagnosis needs;
    /// unlike `UsageStatsRow::runs` (executions summed across every session
    /// for a provider/model pair), this never conflates "many one-shot
    /// `stella run`s" with "one long multi-turn session".
    pub turns: i64,
    /// The session's first recorded execution's provider — sessions
    /// overwhelmingly stay on one provider for their lifetime, and the
    /// diagnosis only needs one to resolve cache posture / TTL.
    pub provider: String,
    /// The session's first recorded execution's model, paired with
    /// [`Self::provider`] for the same reason and read the same way. A
    /// gateway's cache posture is decided by the upstream this slug names, so
    /// the diagnosis cannot resolve it from the provider alone.
    pub model: String,
    pub input_tokens: i64,
    /// The largest single call's **full prompt** in the session —
    /// `input_tokens + cache_write_tokens` for one telemetry row, maximized
    /// over the session, never summed. The cache floor is a per-call
    /// question, and the Anthropic family reports writes outside
    /// `input_tokens`, so neither the sum nor `input_tokens` alone answers
    /// it. Policy (what the floor *is*) stays in `stella-model`; this is the
    /// fact it needs.
    pub largest_prompt_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
}

impl Store {
    /// Cache-read/write/input totals per session, most-recent-first.
    ///
    /// Grouped by `executions.session_id`; sessions with no `session_id`
    /// (pre-v8 telemetry, or an execution dispatched outside a registered
    /// session — same exclusion [`Store::cache_call_gaps`]
    /// makes) are excluded, since there is no session to trend. Ordered by
    /// `MIN(e.id) DESC` rather than `MIN(started_at) DESC`: execution ids are
    /// assigned in AUTOINCREMENT order and `started_at`'s
    /// one-second-resolution `CURRENT_TIMESTAMP` can tie within a single fast
    /// test (or a fast session), so `id` is the collision-free recency proxy
    /// — `started_at` is carried along purely for display.
    pub fn session_cache_trend(&self) -> Result<Vec<SessionCacheTrendRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT e.session_id,
                    min(e.started_at) AS started_at,
                    count(*) AS turns,
                    (SELECT ex.provider FROM executions ex
                       WHERE ex.session_id = e.session_id
                       ORDER BY ex.id ASC LIMIT 1) AS provider,
                    (SELECT ex.model FROM executions ex
                       WHERE ex.session_id = e.session_id
                       ORDER BY ex.id ASC LIMIT 1) AS model,
                    CAST(coalesce(sum(t.input_tokens), 0) AS INTEGER) AS input_tokens,
                    CAST(coalesce(max(t.largest_prompt_tokens), 0) AS INTEGER)
                      AS largest_prompt_tokens,
                    CAST(coalesce(sum(t.cache_read_tokens), 0) AS INTEGER) AS cache_read_tokens,
                    CAST(coalesce(sum(t.cache_write_tokens), 0) AS INTEGER) AS cache_write_tokens,
                    min(e.id) AS first_id
             FROM executions e
             LEFT JOIN (
               SELECT execution_id,
                      sum(input_tokens) AS input_tokens,
                      max(input_tokens + cache_write_tokens) AS largest_prompt_tokens,
                      sum(cache_read_tokens) AS cache_read_tokens,
                      sum(cache_write_tokens) AS cache_write_tokens
               FROM telemetry
               GROUP BY execution_id
             ) t ON t.execution_id = e.id
             WHERE e.session_id IS NOT NULL
             GROUP BY e.session_id
             ORDER BY first_id DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(SessionCacheTrendRow {
                session_id: row.get(0)?,
                started_at: row.get(1)?,
                turns: row.get(2)?,
                provider: row.get(3)?,
                model: row.get(4)?,
                input_tokens: row.get(5)?,
                largest_prompt_tokens: row.get(6)?,
                cache_read_tokens: row.get(7)?,
                cache_write_tokens: row.get(8)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn telemetry(cache_read: u64, cache_write: u64) -> crate::TelemetryRow {
        crate::TelemetryRow {
            step: 0,
            provider: "anthropic".into(),
            call_role: "worker".into(),
            model: "claude".into(),
            input_tokens: cache_read + 1_000,
            estimated_input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: cache_read,
            cache_miss_tokens: 1_000,
            cache_write_tokens: cache_write,
            cost_usd: 0.0,
            duration_ms: 0,
            retries: 0,
            tool_calls: 0,
            usage_complete: true,
            sub_agent_id: None,
        }
    }

    #[test]
    fn largest_prompt_is_one_calls_input_plus_its_writes_never_a_sum() {
        // Both halves of this matter to the cache-floor question upstream, and
        // both were got wrong once. A hundred small calls must not add up to a
        // large prompt (the floor is per call), and a call reporting a tiny
        // `input_tokens` beside a large `cache_write_tokens` IS a large
        // prompt — the Anthropic family reports writes outside the input
        // count, which is how a 4543-token prompt reads as 2 tokens.
        let store = Store::in_memory().unwrap();
        let e1 = store
            .begin_execution("run", "p", "anthropic", "claude")
            .unwrap();
        store.set_execution_session(e1, "s").unwrap();
        // Three ordinary small calls: input 1_000 each (the helper's shape).
        for step in 0..3 {
            let mut call = telemetry(0, 0);
            call.step = step;
            store.record_telemetry(e1, &call).unwrap();
        }
        // One call whose prompt lives almost entirely in the write column.
        let mut mostly_written = telemetry(0, 4_541);
        mostly_written.step = 3;
        mostly_written.input_tokens = 2;
        store.record_telemetry(e1, &mostly_written).unwrap();

        let rows = store.session_cache_trend().unwrap();
        let row = rows.iter().find(|r| r.session_id == "s").unwrap();
        assert_eq!(
            row.largest_prompt_tokens, 4_543,
            "the full prompt of the single largest call: 2 input + 4541 written"
        );
        assert!(
            row.largest_prompt_tokens < row.input_tokens + row.cache_write_tokens,
            "and it is a maximum, not the session's summed traffic"
        );
    }

    #[test]
    fn session_cache_trend_groups_by_session_most_recent_first() {
        let store = Store::in_memory().unwrap();

        // Session "s1": two turns.
        let e1 = store
            .begin_execution("run", "p1", "anthropic", "claude")
            .unwrap();
        store.set_execution_session(e1, "s1").unwrap();
        store.record_telemetry(e1, &telemetry(0, 500)).unwrap();
        // Second turn switches provider (a session CAN cross providers) —
        // the representative provider must still be the FIRST turn's.
        let e2 = store
            .begin_execution("run", "p2", "zai", "glm-5.2")
            .unwrap();
        store.set_execution_session(e2, "s1").unwrap();
        store.record_telemetry(e2, &telemetry(900, 0)).unwrap();

        // Session "s2": one turn, recorded after s1 — must sort first.
        let e3 = store
            .begin_execution("run", "p3", "anthropic", "claude")
            .unwrap();
        store.set_execution_session(e3, "s2").unwrap();
        store.record_telemetry(e3, &telemetry(0, 200)).unwrap();

        // No session registered — excluded entirely.
        let e4 = store
            .begin_execution("run", "p4", "zai", "glm-5.2")
            .unwrap();
        store.record_telemetry(e4, &telemetry(0, 0)).unwrap();

        let trend = store.session_cache_trend().unwrap();
        assert_eq!(
            trend.len(),
            2,
            "the session-less run is excluded: {trend:?}"
        );
        assert_eq!(trend[0].session_id, "s2", "most recent session sorts first");
        assert_eq!(trend[0].turns, 1);
        assert_eq!(trend[0].provider, "anthropic");
        assert_eq!(trend[0].cache_write_tokens, 200);

        assert_eq!(trend[1].session_id, "s1");
        assert_eq!(trend[1].turns, 2);
        // The session's SECOND turn switched to zai, but the representative
        // provider is still the FIRST turn's (anthropic) — diagnosis needs a
        // consistent posture/TTL to resolve against, not the latest one.
        assert_eq!(trend[1].provider, "anthropic");
        // e1: telemetry(0, 500) -> input 1_000; e2: telemetry(900, 0) -> input 1_900.
        assert_eq!(trend[1].input_tokens, 1_000 + 1_900);
        assert_eq!(trend[1].cache_read_tokens, 900);
        assert_eq!(trend[1].cache_write_tokens, 500);
    }

    #[test]
    fn session_cache_trend_empty_store_returns_nothing() {
        let store = Store::in_memory().unwrap();
        assert!(store.session_cache_trend().unwrap().is_empty());
    }
}
