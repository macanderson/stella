// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! v40 → v41: the autonomous-foundry plane (#5453).
//!
//! Not in [`super::additive_tables`], because this step adds tables **and**
//! touches an existing one, which that module's own rule sends elsewhere.

use crate::Result;
use crate::ddl::{FOUNDRY_INVOCATIONS_DDL, FOUNDRY_TOOL_VERSIONS_DDL};
use crate::migrations::{column_exists, table_exists};

/// v40 → v41: the additive `foundry_tool_versions` (rollback history, bytes
/// included) and `foundry_invocations` (per-launch telemetry the circuit
/// breaker folds over) tables, plus `foundry_tools.disabled_reason` — the
/// breaker's recorded answer to "why is this tool off". `''` — every pre-v41
/// row and every human `--disable` — means no breaker spoke; the gate and
/// the status verb print the reason only when one is recorded.
///
/// Nothing is backfilled: versions and invocations were not recorded before
/// this schema existed, so the empty tables are the truthful starting state,
/// and every existing adoption's `disabled_reason` is genuinely empty.
pub(super) fn migrate_v40_to_v41(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(FOUNDRY_TOOL_VERSIONS_DDL)?;
    tx.execute_batch(FOUNDRY_INVOCATIONS_DDL)?;
    // Column-guarded, like every ALTER in this chain: a legacy file that
    // walked the ladder had `foundry_tools` created at the CURRENT shape by
    // v19 → v20 (the DDL constants describe today's schema), so the column
    // may already be there.
    if table_exists(tx, "foundry_tools")? && !column_exists(tx, "foundry_tools", "disabled_reason")?
    {
        tx.execute_batch(
            "ALTER TABLE foundry_tools ADD COLUMN disabled_reason TEXT NOT NULL DEFAULT '';",
        )?;
    }
    Ok(())
}
