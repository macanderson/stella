//! Project ids the hub's data tables carry with no `projects` row.
//!
//! The prune's gone-checkout scan reads its candidates from `projects`. For
//! each one it asks whether the root is still on disk. An id that was never
//! registered has no row there. That scan cannot name it, however long it
//! runs. The rows are out of its reach by construction.
//!
//! Two paths produce one. `replicate_telemetry` writes under the
//! `project_id` its scope carries. `sync_execution` is the call that
//! registers the project. A hub that saw only the first one holds telemetry
//! for a project it has no record of. A re-key ([`super::rekey`]) that moved
//! some tables and not others leaves the same shape.
//!
//! An orphan is GC-eligible on its own. There is no root to check, because
//! nothing ever said where it was. [`UsageStore::prune`]'s org-row guard
//! still holds for it. An id with un-acked cloud-drain rows stays until the
//! drain acks them, just as a registered project would.

use super::{Result, UsageStore};

impl UsageStore {
    /// Project ids present in `telemetry` or `execution_rollup` with no
    /// matching `projects` row, sorted. The module doc says how a hub comes
    /// to hold one. It also says why the prune may take it.
    pub fn orphan_project_ids(&self) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT project_id FROM telemetry \
             WHERE project_id NOT IN (SELECT project_id FROM projects) \
             UNION \
             SELECT project_id FROM execution_rollup \
             WHERE project_id NOT IN (SELECT project_id FROM projects) \
             ORDER BY 1",
        )?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{rollup, source_row};
    use super::super::{PrunePolicy, UsageStore};
    use crate::identity::TelemetryScope;

    /// Local, not borrowed from the parent's fixtures. Widening that one
    /// reflows its signature across four lines, and `usage.rs` is a
    /// grandfathered god file already at its ceiling.
    fn scope(org: Option<&str>, project_id: &str) -> TelemetryScope {
        TelemetryScope {
            org_id: org.map(String::from),
            workspace_id: org.map(|_| "ws-1".into()),
            repo_id: "repo01".into(),
            project_id: project_id.into(),
        }
    }

    /// **The witness for orphan GC.** `replicate_telemetry` writes rows under
    /// a project id. `sync_execution` is what registers the project. Rows
    /// that came only through the first path have no `projects` row. So the
    /// CLI's gone-checkout scan never names them. `orphan_project_ids` does.
    /// `prune` then clears them under the same org-row guard: a NULL-org
    /// orphan goes, an org-scoped one stays for the drain.
    #[test]
    fn telemetry_with_no_registered_project_is_an_orphan_the_prune_reaches() {
        let hub = UsageStore::in_memory().unwrap();
        let ghost = scope(None, "proj_ghost");
        hub.replicate_telemetry(&ghost, &[source_row(1, 0.01), source_row(2, 0.01)])
            .unwrap();
        let acme = scope(Some("acme"), "proj_acme_ghost");
        hub.replicate_telemetry(&acme, &[source_row(1, 0.05)])
            .unwrap();
        // A registered project is never an orphan.
        hub.sync_execution(&rollup(1, vec![])).unwrap();

        let orphans = hub.orphan_project_ids().unwrap();
        assert_eq!(
            orphans,
            vec!["proj_acme_ghost".to_string(), "proj_ghost".to_string()]
        );

        let report = hub
            .prune(&PrunePolicy {
                gc_project_ids: orphans,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            report.gc_projects, 1,
            "the org-scoped orphan is kept for the drain"
        );
        assert_eq!(report.gc_rows, 2);
        assert_eq!(
            hub.orphan_project_ids().unwrap(),
            vec!["proj_acme_ghost".to_string()]
        );
    }
}
