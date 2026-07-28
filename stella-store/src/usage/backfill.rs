//! The registration-time scope backfill (#406).
//!
//! Rows replicated into the hub before a workspace runs `stella cloud
//! register` land with NULL `org_id`/`workspace_id`, and nothing revisits
//! them: [`UsageStore::cloud_pending`] is org-scoped, so pre-registration
//! history was invisible to org reporting and permanently excluded from the
//! cloud drain. `stella cloud register` calls [`UsageStore::backfill_scope`]
//! to close that gap.

use rusqlite::params;

use super::{Result, UsageStore};

impl UsageStore {
    /// Re-stamp this project's NULL-org hub rows with the just-registered
    /// scope, in one transaction — the backfill `stella cloud register` runs
    /// so history replicated *before* registration joins the org's reporting
    /// and drain instead of staying invisible forever (#406).
    ///
    /// Policy for rows that were in the hub under NULL org: they were never
    /// shipped — [`UsageStore::cloud_pending`] is org-scoped and skips
    /// NULL-org rows — so re-stamping cannot double-send anything. The rows
    /// are also *moved* to fresh rowids above the table's current max: the
    /// org's drain cursor only ever advances (`rowid > cursor`), and an org
    /// that already acked rows from other workspaces would otherwise never
    /// see backfilled history parked below its cursor. Rowids carry no
    /// meaning beyond drain order (the stable key is
    /// `(project_id, source_rowid)`), and no cursor or quarantine entry can
    /// reference a NULL-org row.
    ///
    /// Idempotent — a second run finds no NULL-org rows for the project and
    /// changes nothing — and scoped to one `project_id`; another workspace's
    /// rows are never touched. A row with a non-NULL `workspace_id` keeps it.
    ///
    /// Returns how many rows were re-stamped.
    pub fn backfill_scope(
        &self,
        project_id: &str,
        org_id: &str,
        workspace_id: Option<&str>,
    ) -> Result<u64> {
        let conn = self.lock();
        let tx = conn.unchecked_transaction()?;
        let max_rowid: i64 =
            tx.query_row("SELECT COALESCE(MAX(rowid), 0) FROM telemetry", [], |r| {
                r.get(0)
            })?;
        let changed = tx.execute(
            "UPDATE telemetry \
                SET rowid = rowid + ?1, \
                    org_id = ?2, \
                    workspace_id = COALESCE(workspace_id, ?3) \
              WHERE project_id = ?4 AND org_id IS NULL",
            params![max_rowid, org_id, workspace_id, project_id],
        )?;
        tx.commit()?;
        Ok(changed as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::super::UsageStore;

    // Local copies of the parent test module's fixtures — that module is
    // private to `usage.rs`, which sits at its file-size ceiling.
    fn scope(org: Option<&str>, workspace: Option<&str>) -> crate::identity::TelemetryScope {
        crate::identity::TelemetryScope {
            org_id: org.map(String::from),
            workspace_id: workspace.map(String::from),
            repo_id: "repo01".into(),
            project_id: "proj_a".into(),
        }
    }

    fn source_row(source_rowid: i64, cost: f64) -> crate::SourceTelemetryRow {
        crate::SourceTelemetryRow {
            source_rowid,
            execution_id: 1,
            recorded_at: "2026-07-23T10:00:00Z".into(),
            telemetry: crate::TelemetryRow {
                step: source_rowid as u64,
                provider: "zai".into(),
                call_role: "engine".into(),
                model: "glm-5.2".into(),
                input_tokens: 1000,
                estimated_input_tokens: 900,
                output_tokens: 100,
                cache_read_tokens: 500,
                cache_miss_tokens: 500,
                cache_write_tokens: 0,
                cost_usd: cost,
                duration_ms: 1200,
                retries: 0,
                tool_calls: 2,
                usage_complete: true,
            },
        }
    }

    /// Witness for #406: rows replicated before `stella cloud register` land
    /// NULL-org and used to stay NULL forever — invisible to org reporting
    /// and permanently excluded from the cloud drain.
    #[test]
    fn backfill_restamps_pre_registration_rows_into_org_scope() {
        let hub = UsageStore::in_memory().unwrap();
        // History replicated while unregistered → NULL org.
        hub.replicate_telemetry(
            &scope(None, None),
            &[source_row(1, 0.01), source_row(2, 0.02)],
        )
        .unwrap();
        // Another org workspace has already drained and acked rows, so the
        // org cursor sits past the historical rows' rowids.
        let mut other = scope(Some("acme"), Some("ws-other"));
        other.project_id = "proj_b".into();
        hub.replicate_telemetry(&other, &[source_row(1, 0.05)])
            .unwrap();
        let acked = hub.cloud_pending("acme", 10).unwrap();
        hub.ack_cloud_synced("acme", acked.last().unwrap().hub_rowid)
            .unwrap();

        // Register proj_a into acme: its history joins the org report…
        assert_eq!(
            hub.backfill_scope("proj_a", "acme", Some("ws-1")).unwrap(),
            2
        );
        let totals = hub.global_telemetry_totals(Some("acme")).unwrap();
        assert_eq!(totals[0].calls, 3, "historical rows join the org report");

        // …and the drain, even though the org cursor already advanced past
        // the rowids the historical rows used to sit at.
        let pending = hub.cloud_pending("acme", 10).unwrap();
        assert_eq!(pending.len(), 2, "backfilled rows become drainable");
        assert!(pending.iter().all(|p| p.project_id == "proj_a"));
        assert!(
            pending
                .iter()
                .all(|p| p.workspace_id.as_deref() == Some("ws-1")),
            "NULL workspace_id is stamped alongside org_id"
        );

        // Idempotent: nothing left to re-stamp, nothing double-processed.
        assert_eq!(
            hub.backfill_scope("proj_a", "acme", Some("ws-1")).unwrap(),
            0
        );
        assert_eq!(hub.cloud_pending("acme", 10).unwrap().len(), 2);
    }
}
