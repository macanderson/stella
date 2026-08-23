// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The usage hub's schema, and the one thing convergence cannot do.
//!
//! Split out of [`super`] (#3396): that file is a grandfathered god file
//! closed to growth, and the hub had just acquired the kind of change its own
//! module doc said it could not carry. The project store keeps its DDL in
//! [`crate::ddl`] for the same reason; this is that shape, one database over.
//!
//! # Two mechanisms, and when each applies
//!
//! **Convergence** is the ordinary one and is unchanged: every statement in
//! [`USAGE_SCHEMA`] is `CREATE ... IF NOT EXISTS`, the whole batch replays on
//! every open, and an additive table or index therefore reaches an existing
//! hub the next time it is opened. Adding a table needs nothing else.
//!
//! **A reshape or a value rewrite is different**, and `super`'s module doc
//! already said so: "a table that ever needs a reshape would need the
//! versioned machinery introduced first." `IF NOT EXISTS` does nothing to a
//! table that already exists, so it cannot correct a value inside one.
//! [`HUB_MIGRATIONS`] is that machinery, introduced at the first moment it
//! was actually needed rather than in advance.
//!
//! It is deliberately the smallest thing that works: an index-ordered array
//! stepped over `PRAGMA user_version`, exactly like [`crate::migrations`],
//! with the same contract that **a slot is claimed by position, not by
//! name**. Two branches that each append "the next hub migration" merge
//! cleanly and produce a ladder where one step runs at the other's version,
//! and nothing in CI catches it -- so append, and never reorder.
//!
//! # Lazy on open is the whole delivery mechanism, and that is settled
//!
//! [`apply`] runs from `UsageStore::open_*`, so a migration reaches a hub the
//! next time any command touches it -- and never reaches a hub belonging to a
//! machine that has stopped running Stella. #3411 asked whether that wants a
//! `stella usage doctor` reporting `user_version`, or the version surfaced in
//! `stella usage report`. It does not, and the reason is what the hub is: a
//! local aggregate read only by the process that opens it. A hub nothing opens
//! is a hub nothing reads, so an uncorrected one is unobservable in exactly
//! the same circumstances that leave it uncorrected -- and the first read
//! after that machine comes back is the read that runs the ladder. A reporting
//! command would be a surface for a state no user can be in while looking at
//! it.
//!
//! The question reopens if a hub is ever read by something other than the
//! process that opened it (a shared or remote aggregate), or if a migration
//! arrives whose *absence* corrupts rather than merely delays -- one that must
//! be known to have run before any write, not merely before any read. Neither
//! is true today.

use rusqlite::Connection;

use crate::Result;

pub(super) const USAGE_SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS projects (
    project_id    TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    root_path     TEXT NOT NULL,
    first_seen_at TEXT NOT NULL,
    last_seen_at  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS execution_rollup (
    project_id      TEXT NOT NULL,
    execution_id    INTEGER NOT NULL,
    kind            TEXT NOT NULL,
    prompt_digest   TEXT NOT NULL,
    prompt_preview  TEXT NOT NULL DEFAULT '',
    model           TEXT NOT NULL,
    provider        TEXT NOT NULL,
    outcome         TEXT NOT NULL,
    cost_usd        REAL NOT NULL,
    input_tokens    INTEGER NOT NULL,
    output_tokens   INTEGER NOT NULL,
    duration_ms     INTEGER NOT NULL,
    tool_calls      INTEGER NOT NULL,
    files_written   INTEGER NOT NULL,
    produced_output INTEGER NOT NULL,
    self_rating     INTEGER,
    started_at      TEXT NOT NULL,
    -- 0 when this row's figures are a lower bound: the turn finished with at
    -- least one paid call whose envelope never landed (#4171). Not a reason to
    -- withhold the row -- what the receipts prove is more useful than silence.
    usage_complete  INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (project_id, execution_id)
);
-- Convergence works for removals too: the batch replays on every open, so a
-- DROP here reaches existing hubs the same way a new table would. The
-- by-model index served no query (every execution_rollup reader keys on
-- project_id, which the primary key already covers).
DROP INDEX IF EXISTS execution_rollup_by_model;
CREATE TABLE IF NOT EXISTS tool_usage_rollup (
    project_id TEXT NOT NULL,
    tool       TEXT NOT NULL,
    surface    TEXT NOT NULL,
    day        TEXT NOT NULL,
    calls      INTEGER NOT NULL DEFAULT 0,
    errors     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (project_id, tool, surface, day)
);
-- Which executions have already been folded into `tool_usage_rollup` (#3411).
-- That fold is additive, so it must happen once per execution; asking
-- `execution_rollup` instead was wrong because `prune` deletes from it on
-- predicates the rollup does not share. `day` is the bucket's own key, copied
-- so a claim and its bucket age out together. See `super::tool_fold`.
CREATE TABLE IF NOT EXISTS tool_fold_ledger (
    project_id   TEXT NOT NULL,
    execution_id INTEGER NOT NULL,
    day          TEXT NOT NULL,
    PRIMARY KEY (project_id, execution_id)
);
CREATE TABLE IF NOT EXISTS telemetry (
    project_id     TEXT NOT NULL,
    source_rowid   INTEGER NOT NULL,
    org_id         TEXT,
    workspace_id   TEXT,
    repo_id        TEXT NOT NULL DEFAULT '',
    execution_id   INTEGER NOT NULL,
    step           INTEGER NOT NULL,
    recorded_at    TEXT NOT NULL DEFAULT '',
    provider       TEXT NOT NULL,
    call_role      TEXT NOT NULL,
    model          TEXT NOT NULL,
    input_tokens   INTEGER NOT NULL,
    estimated_input_tokens INTEGER NOT NULL,
    output_tokens  INTEGER NOT NULL,
    cache_read_tokens  INTEGER NOT NULL,
    cache_miss_tokens  INTEGER NOT NULL,
    cache_write_tokens INTEGER NOT NULL,
    cost_usd       REAL NOT NULL,
    duration_ms    INTEGER NOT NULL,
    retries        INTEGER NOT NULL,
    tool_calls     INTEGER NOT NULL,
    usage_complete INTEGER NOT NULL,
    PRIMARY KEY (project_id, source_rowid)
);
CREATE INDEX IF NOT EXISTS telemetry_by_org
    ON telemetry(org_id, recorded_at);
-- The observatory's per-(provider, model) hub rollup groups this table by
-- exactly these columns; the index is what lets that GROUP BY scan in index
-- order instead of sorting the whole hub.
CREATE INDEX IF NOT EXISTS telemetry_by_model
    ON telemetry(provider, model);
CREATE TABLE IF NOT EXISTS telemetry_sync_cursors (
    project_id        TEXT PRIMARY KEY,
    last_source_rowid INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS cloud_sync_cursors (
    org_id         TEXT PRIMARY KEY,
    last_hub_rowid INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS cloud_quarantine (
    org_id         TEXT NOT NULL,
    project_id     TEXT NOT NULL,
    source_rowid   INTEGER NOT NULL,
    workspace_id   TEXT,
    repo_id        TEXT NOT NULL DEFAULT '',
    hub_rowid      INTEGER NOT NULL,
    recorded_at    TEXT NOT NULL DEFAULT '',
    provider       TEXT NOT NULL DEFAULT '',
    model          TEXT NOT NULL DEFAULT '',
    input_tokens   INTEGER NOT NULL DEFAULT 0,
    output_tokens  INTEGER NOT NULL DEFAULT 0,
    cost_usd       REAL NOT NULL DEFAULT 0,
    reason         TEXT NOT NULL,
    http_status    INTEGER,
    quarantined_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (org_id, project_id, source_rowid)
);
CREATE INDEX IF NOT EXISTS cloud_quarantine_by_org
    ON cloud_quarantine(org_id, quarantined_at);
";

/// One hub migration function: upgrades an existing `usage.db` exactly one
/// `user_version` step, inside the transaction the runner opened for it.
type HubMigration = fn(&rusqlite::Transaction<'_>) -> Result<()>;

/// One rung of [`HUB_MIGRATIONS`]: the function that performs the reshape,
/// paired with the `user_version` it upgrades a hub *to*.
///
/// `target_version` is redundant with the entry's position under correct
/// use -- and that redundancy is the guard. The `#[cfg(test)]` test
/// `hub_migrations_declare_sequential_versions` (bottom of this module)
/// asserts every entry's `target_version` equals its index + 1, so two
/// branches that each append "the next hub migration" independently (the
/// exact shape #4487 found: PR #4479 took hub v2 -> v3 while #4462 took the
/// *workspace* ladder's v29 -> v30 slot) fail that test instead of silently
/// composing into a ladder where one step runs at the other's version.
struct HubMigrationStep {
    /// The `user_version` this step upgrades a hub to.
    target_version: i64,
    apply: HubMigration,
}

/// Ordered hub migration list. `HUB_MIGRATIONS[i]` upgrades a hub at
/// `user_version` i to i + 1.
///
/// Append only -- see the module doc for why position is the contract -- and
/// **never renumber**: a slot is claimed by position, not by name.
const HUB_MIGRATIONS: [HubMigrationStep; 3] = [
    // v0 -> v1: back-propagate #3388's door-only `kind` vocabulary, and give
    // the hub the `role` column #3395 gave the project store.
    HubMigrationStep {
        target_version: 1,
        apply: migrate_hub_v0_to_v1,
    },
    // v1 -> v2: seat the tool-fold ledger from what is already rolled up.
    HubMigrationStep {
        target_version: 2,
        apply: migrate_hub_v1_to_v2,
    },
    // v2 -> v3: `execution_rollup` learns whether its figures are a floor
    // (#4171). Existing rows default to 1 because the gate this replaces only
    // ever let complete executions through.
    HubMigrationStep {
        target_version: 3,
        apply: migrate_hub_v2_to_v3,
    },
    // ── APPEND POINT — RESERVED SLOTS ───────────────────────────────────
    // This is an INDEX-ORDERED array and `HUB_SCHEMA_VERSION` is its length,
    // so a slot is claimed by position, not by name. Two branches that each
    // append "the next hub migration" merge cleanly -- git sees two additions
    // to different lines -- and, unlike `crate::migrations::MIGRATIONS`,
    // produce a ladder where one step silently runs at the other's version:
    // nothing in CI catches it, because both files compile and
    // `target_version` was never checked against position before #4487.
    // `hub_migrations_declare_sequential_versions` (below) is what catches it
    // now: give your new entry the correct `target_version` and the test
    // fails loudly if a parallel merge left two entries claiming the same
    // slot.
    //
    // Nothing is reserved now: take v3 -> v4 and add your own line here. If a
    // reserved phase ships without needing its slot, delete its line rather
    // than leaving a hole -- index order is the contract.
];

/// The hub schema version this build writes -- the `PRAGMA user_version` of
/// every hub it has opened.
const HUB_SCHEMA_VERSION: i64 = HUB_MIGRATIONS.len() as i64;

/// Apply the schema to a freshly opened hub connection: converge the tables,
/// then step any outstanding migrations.
///
/// Ordering matters and is not arbitrary. Convergence runs **first** so that a
/// migration can assume every table it names exists -- including on a brand
/// new file, where the migrations then run over empty tables and do nothing.
pub(super) fn apply(conn: &mut Connection) -> Result<()> {
    conn.execute_batch(USAGE_SCHEMA)?;

    let mut version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    while version < HUB_SCHEMA_VERSION {
        let index = usize::try_from(version).map_err(|_| {
            crate::StoreError::Other(format!("hub schema version is negative: {version}"))
        })?;
        let step = &HUB_MIGRATIONS[index];
        // Redundant with `hub_migrations_declare_sequential_versions` by
        // design (see [`HubMigrationStep`]): that test cannot run against a
        // hub some *other* build already wrote, so this is the same
        // assertion at the one moment `target_version` still matters for a
        // running process -- a slot collision that reached a release trips
        // this before it ever reshapes the wrong table.
        debug_assert_eq!(
            step.target_version,
            version + 1,
            "HUB_MIGRATIONS[{index}] declares target_version {} but its position \
             implies {} -- see hub_migrations_declare_sequential_versions",
            step.target_version,
            version + 1
        );
        let tx = conn.transaction()?;
        (step.apply)(&tx)?;
        // Stamped inside the same transaction as the reshape, so a crash
        // mid-migration rolls back to the old version AND the old shape,
        // never a mix. The project store's runner makes the same guarantee.
        tx.execute_batch(&format!("PRAGMA user_version = {};", version + 1))?;
        tx.commit()?;
        version += 1;
    }
    Ok(())
}

/// v0 -> v1: the hub adopts the door-only `kind` vocabulary (#3396), and
/// grows the `role` column that carries what used to hide in it (#3395).
///
/// # Why the hub needed its own step
///
/// PR #3393 rewrote two legacy `executions.kind` values in the *project*
/// store. The hub is a second database holding rows replicated from every
/// project store, and it kept the old values: `sync_execution` is an upsert
/// keyed `(project_id, execution_id)`, so a hub row is corrected only if
/// something re-syncs that exact execution -- and replication walks **forward
/// from a durable cursor** (`telemetry_sync_cursors.last_source_rowid`).
/// `stella usage sync --all` walks that same cursor, so it does not re-visit
/// an already-replicated execution either.
///
/// So nothing back-propagated the fix, and a query spanning the two databases
/// would have counted `deck` and `deck-pipeline` as different doors -- the
/// exact double-count #3388 exists to prevent, displaced one database over.
///
/// # Why a rewrite and not a re-sync
///
/// Rewinding the cursor to re-replicate would re-fold the additive
/// `tool_usage_rollup` day counts and double-count them. That rollup is the
/// one part of the hub that is not idempotent under re-sync, which makes "run
/// it again" the wrong instrument. Rewriting the wrong values in place
/// touches only the column that is wrong.
///
/// # Scope
///
/// Exactly #3388's mapping and #3395's, named value by value for the reason
/// both of those give: the legacy set was read off the write sites, and a
/// broad rewrite would corrupt a value an older build wrote that nothing in
/// this tree writes.
///
/// `execution_rollup` is local-only and never drained (see
/// [`crate::content_free`]), so the new column raises no invariant-3
/// question -- it is not a hub `telemetry` column and cannot become egress.
fn migrate_hub_v0_to_v1(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if !column_exists(tx, "execution_rollup", "role")? {
        tx.execute_batch("ALTER TABLE execution_rollup ADD COLUMN role TEXT;")?;
    }
    // Idempotent by construction: after the first pass nothing matches.
    tx.execute_batch(
        "UPDATE execution_rollup SET kind = 'run'  WHERE kind = 'pipeline';
         UPDATE execution_rollup SET kind = 'deck' WHERE kind = 'deck-pipeline';",
    )?;
    for role in crate::migrations::ROLE_KINDS {
        tx.execute(
            "UPDATE execution_rollup SET kind = ?1, role = ?2 WHERE kind = ?2",
            rusqlite::params![crate::migrations::SYSTEM_NON_DOOR, role],
        )?;
    }
    Ok(())
}

/// v1 -> v2: seat `tool_fold_ledger` from the executions already rolled up
/// (#3411).
///
/// Convergence creates the table; this decides what it starts holding. An
/// existing hub has already folded every execution in `execution_rollup`, so
/// without this seat the first re-sync of one would claim the fold as new and
/// add its histogram a second time — the ledger would arrive and immediately
/// certify a double count.
///
/// `substr(started_at, 1, 10)` rather than `date(started_at)`: the day is a
/// raw prefix of the timestamp at the write site (`Store::execution_rollup_row`),
/// and `date()` would normalize a value it can parse and return NULL for one
/// it cannot. Copying the same bytes is what keeps a claim and its bucket on
/// one key.
///
/// Executions whose rollup row `prune` has already deleted are not seated —
/// nothing records that they were folded, and nothing can. Their buckets are
/// the pre-existing exposure this ledger stops accruing; it does not
/// retroactively repair a hub that has already double-counted.
fn migrate_hub_v1_to_v2(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO tool_fold_ledger (project_id, execution_id, day) \
         SELECT project_id, execution_id, substr(started_at, 1, 10) FROM execution_rollup",
        [],
    )?;
    Ok(())
}

/// v2 -> v3: `execution_rollup` learns whether the figures on a row are exact
/// or a floor (#4171).
///
/// Until #4171 an execution whose accounting was short by one call was not
/// rolled up at all, so every project total silently read its spend as zero.
/// It now rolls up with the flag, and the flag has to live somewhere the hub
/// can read back.
///
/// `NOT NULL DEFAULT 1` is a record rather than an invention, the same shape
/// as the store's own v29 -> v30: the gate this replaces admitted complete
/// executions and nothing else, so every row already in a hub is exact by
/// construction.
///
/// `execution_rollup` is local-only and never drained (see
/// [`crate::content_free`]), so the new column raises no invariant-3 question
/// — it is not a hub `telemetry` column and cannot become egress.
fn migrate_hub_v2_to_v3(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if !column_exists(tx, "execution_rollup", "usage_complete")? {
        tx.execute_batch(
            "ALTER TABLE execution_rollup \
             ADD COLUMN usage_complete INTEGER NOT NULL DEFAULT 1;",
        )?;
    }
    Ok(())
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::{HUB_MIGRATIONS, HUB_SCHEMA_VERSION};

    /// The #4487 witness: `HUB_MIGRATIONS` is claimed by position, and this is
    /// what makes a slot collision loud instead of silent. Two branches that
    /// each append "the next hub migration" merge textually clean -- each adds
    /// one array element -- but if both declare the same `target_version` (the
    /// shape #4487 found: PR #4479 took hub v2 -> v3 while #4462 took the
    /// *workspace* ladder's v29 -> v30 slot, and the hub equivalent would have
    /// composed with no textual conflict at all), the ladder ends up with two
    /// entries claiming one version and a hole where the other should be. This
    /// test catches that: every entry's declared `target_version` must equal
    /// its index + 1, or the ladder is out of order or has a duplicate/missing
    /// slot.
    #[test]
    fn hub_migrations_declare_sequential_versions() {
        for (index, step) in HUB_MIGRATIONS.iter().enumerate() {
            assert_eq!(
                step.target_version,
                index as i64 + 1,
                "HUB_MIGRATIONS[{index}] declares target_version {}, but its position \
                 says it should be {} -- a slot collision or a hole in the ladder",
                step.target_version,
                index + 1
            );
        }
        assert_eq!(
            HUB_SCHEMA_VERSION,
            HUB_MIGRATIONS.len() as i64,
            "HUB_SCHEMA_VERSION must track the ladder's length exactly"
        );
    }
}
