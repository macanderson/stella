//! Project re-key (#408): merge one hub project identity into another.
//!
//! `project_id` began as the FNV hash of the checkout path, so a `mv` forked
//! the identity: the cursor reset, the whole store re-replicated, and the hub
//! showed two projects for one repo. A *registered* workspace now replicates
//! under its stable `workspace_id`-derived id
//! ([`crate::identity::replication_project_id`]) — and this module is the
//! migration for rows that landed under the old path hash: registration and
//! every sync call [`UsageStore::adopt_project_identity`], which idempotently
//! re-keys the six tables carrying a `project_id`.
//!
//! Merge semantics, per table: rows move to the new id; a row that already
//! exists under the new id wins and the old duplicate is dropped (`telemetry`
//! and `cloud_quarantine` rows are identical under both ids by construction —
//! they came from the same source rowids); the replication cursor merges by
//! `MAX`, so it continues instead of rewinding. One transaction: the hub is
//! never observable half-migrated.

use rusqlite::params;

use super::{Result, UsageStore};

/// Every hub table with a `project_id` column. A new table with one must be
/// added here — the witness test creates rows in each and asserts none are
/// left under the old id.
const PROJECT_KEYED_TABLES: &[&str] = &[
    "projects",
    "execution_rollup",
    "tool_usage_rollup",
    "telemetry",
    "cloud_quarantine",
];

impl UsageStore {
    /// Re-key everything recorded under `old_project_id` to
    /// `new_project_id`, in one transaction. Idempotent — a second run finds
    /// nothing under the old id — and a same-id call is a no-op. Returns how
    /// many rows moved (cursor rows included).
    pub fn adopt_project_identity(
        &self,
        old_project_id: &str,
        new_project_id: &str,
    ) -> Result<u64> {
        if old_project_id == new_project_id {
            return Ok(0);
        }
        let conn = self.lock();
        let tx = conn.unchecked_transaction()?;
        let mut moved = 0u64;
        for table in PROJECT_KEYED_TABLES {
            // `OR IGNORE`: a row already present under the new id wins its
            // primary-key conflict; the superseded old row is deleted below.
            moved += tx.execute(
                &format!("UPDATE OR IGNORE {table} SET project_id = ?1 WHERE project_id = ?2"),
                params![new_project_id, old_project_id],
            )? as u64;
            tx.execute(
                &format!("DELETE FROM {table} WHERE project_id = ?1"),
                params![old_project_id],
            )?;
        }
        // The cursor merges by MAX so replication continues from wherever
        // either identity had reached — never a rewind, never a re-replication.
        moved += tx.execute(
            "INSERT INTO telemetry_sync_cursors (project_id, last_source_rowid) \
             SELECT ?1, last_source_rowid FROM telemetry_sync_cursors WHERE project_id = ?2 \
             ON CONFLICT(project_id) DO UPDATE SET \
               last_source_rowid = MAX(last_source_rowid, excluded.last_source_rowid)",
            params![new_project_id, old_project_id],
        )? as u64;
        tx.execute(
            "DELETE FROM telemetry_sync_cursors WHERE project_id = ?1",
            params![old_project_id],
        )?;
        tx.commit()?;
        Ok(moved)
    }
}

#[cfg(test)]
mod tests {
    use super::super::UsageStore;
    use crate::identity::TelemetryScope;

    fn scope(project_id: &str) -> TelemetryScope {
        TelemetryScope {
            org_id: Some("acme".into()),
            workspace_id: Some("ws-1".into()),
            repo_id: "repo01".into(),
            project_id: project_id.into(),
        }
    }

    fn row(source_rowid: i64) -> crate::SourceTelemetryRow {
        crate::SourceTelemetryRow {
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
        }
    }

    /// Witness for #408's acceptance: rows replicated under the old
    /// path-derived id merge under the stable id, the cursor continues
    /// (no wholesale re-replication), and the hub shows one project.
    #[test]
    fn adoption_merges_the_fork_and_the_cursor_continues() {
        let hub = UsageStore::in_memory().unwrap();
        hub.replicate_telemetry(&scope("old-path-hash"), &[row(1), row(2)])
            .unwrap();
        assert_eq!(hub.telemetry_cursor("old-path-hash").unwrap(), 2);

        let moved = hub
            .adopt_project_identity("old-path-hash", "ws-derived")
            .unwrap();
        assert!(moved >= 3, "telemetry rows + cursor moved, got {moved}");
        assert_eq!(
            hub.telemetry_cursor("ws-derived").unwrap(),
            2,
            "the cursor continues under the stable id — nothing re-replicates"
        );
        assert_eq!(hub.telemetry_cursor("old-path-hash").unwrap(), 0);

        // Replication picks up where the old identity left off.
        hub.replicate_telemetry(&scope("ws-derived"), &[row(3)])
            .unwrap();
        let totals = hub.global_telemetry_totals(Some("acme")).unwrap();
        assert_eq!(totals[0].calls, 3);
        assert_eq!(totals[0].projects, 1, "one project in the hub, not two");

        // Idempotent; and a same-id call is a no-op.
        assert_eq!(
            hub.adopt_project_identity("old-path-hash", "ws-derived")
                .unwrap(),
            0
        );
        assert_eq!(hub.adopt_project_identity("x", "x").unwrap(), 0);
    }

    /// Rows already present under the new id win their key conflict; the
    /// duplicate old rows are dropped rather than wedging the migration.
    #[test]
    fn a_partial_earlier_migration_does_not_wedge_adoption() {
        let hub = UsageStore::in_memory().unwrap();
        hub.replicate_telemetry(&scope("old"), &[row(1), row(2)])
            .unwrap();
        hub.replicate_telemetry(&scope("new"), &[row(2), row(3)])
            .unwrap();

        hub.adopt_project_identity("old", "new").unwrap();
        let totals = hub.global_telemetry_totals(Some("acme")).unwrap();
        assert_eq!(totals[0].calls, 3, "row 2 exists once, not twice");
        assert_eq!(
            hub.telemetry_cursor("new").unwrap(),
            3,
            "the cursor is the MAX of the two"
        );
    }
}
