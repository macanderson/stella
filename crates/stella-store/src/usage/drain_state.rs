//! Drain observability state (#404, #464): the per-org record of the most
//! recent hub→cloud drain attempt, plus the cursor/backlog readers
//! `stella cloud status` renders.
//!
//! The attempts table is created lazily here rather than in the main DDL —
//! it is pure observability (one row per org, overwritten per attempt), so a
//! hub that never drains never carries it, and older builds reading the same
//! hub are unaffected.

use rusqlite::{OptionalExtension, params};

use super::{Result, UsageStore};

/// The most recent drain attempt for one org — `stella cloud status`'s
/// "is my telemetry actually reaching the org?" answer (#464).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainAttempt {
    /// When the attempt ran (RFC 3339 UTC).
    pub attempted_at: String,
    /// Whether the pass completed without being stopped by a rejection.
    pub ok: bool,
    /// A one-line outcome: delivered/quarantined counts, or the stopping
    /// rejection's diagnostic.
    pub detail: String,
}

const ATTEMPTS_DDL: &str = "CREATE TABLE IF NOT EXISTS cloud_drain_attempts (
    org_id       TEXT PRIMARY KEY,
    attempted_at TEXT NOT NULL,
    ok           INTEGER NOT NULL,
    detail       TEXT NOT NULL DEFAULT ''
)";

impl UsageStore {
    /// Record the outcome of one drain pass, overwriting the org's previous
    /// attempt — status wants the latest, and the full history is the intake's
    /// job, not the hub's.
    pub fn record_drain_attempt(
        &self,
        org_id: &str,
        attempted_at: &str,
        ok: bool,
        detail: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(ATTEMPTS_DDL, [])?;
        conn.execute(
            "INSERT INTO cloud_drain_attempts (org_id, attempted_at, ok, detail) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(org_id) DO UPDATE SET \
               attempted_at = excluded.attempted_at, \
               ok = excluded.ok, \
               detail = excluded.detail",
            params![org_id, attempted_at, ok, detail],
        )?;
        Ok(())
    }

    /// The org's most recent drain attempt, or `None` when nothing has ever
    /// tried (including on a hub the attempts table has never been created in).
    pub fn last_drain_attempt(&self, org_id: &str) -> Result<Option<DrainAttempt>> {
        let conn = self.lock();
        conn.execute(ATTEMPTS_DDL, [])?;
        Ok(conn
            .query_row(
                "SELECT attempted_at, ok, detail FROM cloud_drain_attempts WHERE org_id = ?1",
                params![org_id],
                |r| {
                    Ok(DrainAttempt {
                        attempted_at: r.get(0)?,
                        ok: r.get::<_, i64>(1)? != 0,
                        detail: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// The org's monotonic drain cursor — the last hub rowid a confirmed ack
    /// covered, `0` when nothing has ever been acked.
    pub fn cloud_cursor(&self, org_id: &str) -> Result<i64> {
        Ok(self
            .lock()
            .query_row(
                "SELECT last_hub_rowid FROM cloud_sync_cursors WHERE org_id = ?1",
                params![org_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    /// How many of the org's staged rows the cloud has not acked — the depth
    /// `cloud_pending` would page, without loading a single row.
    pub fn cloud_pending_count(&self, org_id: &str) -> Result<u64> {
        Ok(self.lock().query_row(
            "SELECT COUNT(*) FROM telemetry t \
              WHERE t.org_id = ?1 \
                AND t.rowid > COALESCE((SELECT last_hub_rowid FROM cloud_sync_cursors \
                                         WHERE org_id = ?1), 0)",
            params![org_id],
            |r| r.get::<_, i64>(0).map(|n| n.max(0) as u64),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::super::UsageStore;

    #[test]
    fn drain_attempts_overwrite_and_read_back() {
        let hub = UsageStore::in_memory().unwrap();
        assert_eq!(hub.last_drain_attempt("acme").unwrap(), None);

        hub.record_drain_attempt("acme", "2026-07-28T06:00:00Z", false, "HTTP 503")
            .unwrap();
        hub.record_drain_attempt("acme", "2026-07-28T07:00:00Z", true, "12 delivered")
            .unwrap();
        let last = hub.last_drain_attempt("acme").unwrap().expect("attempt");
        assert_eq!(last.attempted_at, "2026-07-28T07:00:00Z");
        assert!(last.ok);
        assert_eq!(last.detail, "12 delivered");
        assert_eq!(
            hub.last_drain_attempt("other").unwrap(),
            None,
            "attempts are per-org"
        );
    }

    #[test]
    fn cursor_and_pending_count_read_the_drain_backlog() {
        let hub = UsageStore::in_memory().unwrap();
        assert_eq!(hub.cloud_cursor("acme").unwrap(), 0);
        assert_eq!(hub.cloud_pending_count("acme").unwrap(), 0);

        let scope = crate::identity::TelemetryScope {
            org_id: Some("acme".into()),
            workspace_id: Some("ws-1".into()),
            repo_id: "repo01".into(),
            project_id: "proj_a".into(),
        };
        let row = |source_rowid: i64| crate::SourceTelemetryRow {
            source_rowid,
            execution_id: 1,
            recorded_at: "2026-07-28T06:00:00Z".into(),
            telemetry: crate::TelemetryRow {
                step: 1,
                provider: "zai".into(),
                call_role: "engine".into(),
                model: "glm-5.2".into(),
                input_tokens: 10,
                estimated_input_tokens: 10,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_miss_tokens: 10,
                cache_write_tokens: 0,
                cost_usd: 0.01,
                duration_ms: 100,
                retries: 0,
                tool_calls: 0,
                usage_complete: true,
            },
        };
        hub.replicate_telemetry(&scope, &[row(1), row(2), row(3)])
            .unwrap();
        assert_eq!(hub.cloud_pending_count("acme").unwrap(), 3);

        let pending = hub.cloud_pending("acme", 10).unwrap();
        hub.ack_cloud_synced("acme", pending[1].hub_rowid).unwrap();
        assert_eq!(hub.cloud_cursor("acme").unwrap(), pending[1].hub_rowid);
        assert_eq!(
            hub.cloud_pending_count("acme").unwrap(),
            1,
            "the count is exactly what cloud_pending would still page"
        );
    }
}
