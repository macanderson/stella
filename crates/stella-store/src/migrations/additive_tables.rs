// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Every step in the ladder whose whole content is "a new table appears".
//!
//! They are grouped because they are the one shape in the chain with nothing
//! to reason about per step: no existing table changes shape, so there is no
//! §7 rebuild, no dedupe, and no backfill, and the shared DDL's
//! `IF NOT EXISTS` already tolerates a partial file that somehow grew the
//! table early. What varies between them is only *which* table and *why it
//! was added*, which is what each doc comment records.
//!
//! A step that adds a table **and** touches an existing one does not belong
//! here — [`super::execution_plane`] holds those.

use crate::Result;
use crate::ddl::{
    AGENT_USES_DDL, EXECUTION_REFLECTION_DDL, FORGOTTEN_DDL, FOUNDRY_TOOLS_DDL, MCP_USAGE_DDL,
    MEMORY_CITATIONS_DDL, REFLECTIONS_DDL, RULES_TABLE, SESSION_TURN_DIFFS_DDL, SKILL_USAGE_DDL,
    TOOL_CALLS_INDEXES, tool_calls_ddl,
};

/// v2 → v3: the `memory_citations` table. Purely additive — no existing
/// table is touched, so no rebuild, no dedupe, no backfill.
pub(super) fn migrate_v2_to_v3(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(MEMORY_CITATIONS_DDL)?;
    tx.execute_batch(RULES_TABLE)?;
    Ok(())
}

/// v3 → v4: the `agent_uses` invocation log (agent-version usage telemetry,
/// drained per execution like `files_touched`). Purely additive — no
/// existing table changes shape, so no §7 rebuild is needed; `IF NOT
/// EXISTS` also covers a partial file that somehow already grew the table.
pub(super) fn migrate_v3_to_v4(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(AGENT_USES_DDL)?;
    Ok(())
}

/// v4 → v5: the additive `skill_usage` table (skill-version usage telemetry,
/// SKILLS tab). Purely additive, mirroring [`migrate_v3_to_v4`].
pub(super) fn migrate_v4_to_v5(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(SKILL_USAGE_DDL)?;
    Ok(())
}

/// v5 → v6: the `mcp_usage` table. Purely additive — no existing table is
/// touched, so no rebuild, no dedupe, no backfill.
pub(super) fn migrate_v5_to_v6(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(MCP_USAGE_DDL)?;
    Ok(())
}

/// v6 → v7: the additive data-plane tables — `tool_calls`,
/// `execution_reflection`, and `reflections`. No existing table changes shape.
pub(super) fn migrate_v6_to_v7(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(&tool_calls_ddl("tool_calls"))?;
    tx.execute_batch(TOOL_CALLS_INDEXES)?;
    tx.execute_batch(EXECUTION_REFLECTION_DDL)?;
    tx.execute_batch(REFLECTIONS_DDL)?;
    Ok(())
}

/// v13 → v14: `forgotten`, the explicit-tombstone table. Purely additive —
/// one new table plus its by-surface index, so no §7 rebuild. `IF NOT EXISTS`
/// in the shared DDL also tolerates a partial file that already grew it.
pub(super) fn migrate_v13_to_v14(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(FORGOTTEN_DDL)?;
    Ok(())
}

/// v19 → v20: the additive `foundry_tools` adoption ledger (#830), mirroring
/// [`migrate_v3_to_v4`]. Nothing is backfilled: a workspace upgrading into
/// this schema has approved no self-authored tools, so the empty table is the
/// truthful starting state rather than a gap to fill in.
pub(super) fn migrate_v19_to_v20(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(FOUNDRY_TOOLS_DDL)?;
    Ok(())
}

/// v20 → v21: the additive `session_turn_diffs` table (#1870). No existing
/// table changes shape.
pub(super) fn migrate_v20_to_v21(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(SESSION_TURN_DIFFS_DDL)?;
    Ok(())
}
