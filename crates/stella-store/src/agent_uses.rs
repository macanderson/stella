// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The `agent_uses` invocation log — one row per agent invocation under an
//! execution, drained once per execution from `stella-tools`' session ledger.
//!
//! Two writers share the table and mint `agent` from different name spaces,
//! which is what [`AgentUseRow::kind`] records (#3822):
//!
//! | `kind` | writer | what `agent` is |
//! |---|---|---|
//! | `definition` | `stella-cli`'s deck (`command_deck::authoring`) | an installed agent definition's name (`reviewer`), at its pinned `version` |
//! | `delegation` | `stella-tools`' `task` tool (`subagent`) | the child id minted from the model's own description (`find-retry-policy-2`), always version 1 |
//!
//! A definition name repeats across sessions and is worth counting; a
//! delegation id is unique per delegation by construction. Without the
//! discriminator the Observatory's `GROUP BY agent` rendered "this session
//! leaned on the reviewer agent" and "this session delegated eight research
//! questions" as the same shape.
//!
//! Never aggregated on the way in: every invocation is its own row, because
//! the unit of analysis is "agent X was invoked by execution Y at time T".

use rusqlite::params;

use crate::{Result, Store};

/// The `kind` of an [`AgentUseRow`] — which writer minted its `agent` name.
/// The stored tokens are the two the table's `CHECK` accepts.
pub const KIND_DEFINITION: &str = "definition";
/// See [`KIND_DEFINITION`]; a `task` delegation's child id.
pub const KIND_DELEGATION: &str = "delegation";

/// One agent-invocation row for the `agent_uses` log: which agent (by name),
/// at which pinned version, was invoked under an execution — with a short
/// free-text reason when one was available, and the `kind` naming which
/// writer's name space `agent` is drawn from. The timestamp column defaults to
/// the insert time; the ledger drains per execution, so insert time is
/// invocation-accurate to within the turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentUseRow {
    pub agent: String,
    pub version: u32,
    pub reason: String,
    /// [`KIND_DEFINITION`] or [`KIND_DELEGATION`]. Anything else is rejected
    /// by the table's `CHECK`, so a caller inventing a third kind fails loudly
    /// at the write rather than quietly at the next reader.
    pub kind: String,
}

impl Store {
    /// Record non-aggregated agent invocations from one execution. One
    /// transaction — see [`Store::record_files_touched`].
    pub fn record_agent_uses(&self, execution_id: i64, uses: &[AgentUseRow]) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        for row in uses {
            tx.execute(
                "INSERT INTO agent_uses (execution_id, agent, version, reason, kind) \
                 VALUES (?, ?, ?, ?, ?)",
                params![
                    execution_id,
                    row.agent,
                    row.version as i64,
                    row.reason,
                    row.kind
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(agent: &str, kind: &str) -> AgentUseRow {
        AgentUseRow {
            agent: agent.to_string(),
            version: 1,
            reason: String::new(),
            kind: kind.to_string(),
        }
    }

    /// The witness for #3822's store half: the two writers' rows stay
    /// distinguishable once written, which is what the Observatory's grouping
    /// needs and what no column could answer before.
    #[test]
    fn a_delegation_and_a_definition_are_distinguishable_once_stored() {
        let store = Store::in_memory().expect("store");
        let id = store
            .begin_execution("deck", "p", "anthropic", "claude")
            .expect("execution");
        store
            .record_agent_uses(
                id,
                &[
                    row("reviewer", KIND_DEFINITION),
                    row("find-retry-policy", KIND_DELEGATION),
                    row("find-retry-policy-2", KIND_DELEGATION),
                ],
            )
            .expect("record");

        let conn = store.lock();
        let delegations: i64 = conn
            .query_row(
                "SELECT count(*) FROM agent_uses WHERE kind = 'delegation'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(delegations, 2);
        let definitions: i64 = conn
            .query_row(
                "SELECT count(*) FROM agent_uses WHERE kind = 'definition'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(definitions, 1);
    }

    /// A third kind is a schema error, not a row nobody notices.
    #[test]
    fn an_unknown_kind_is_refused_by_the_table() {
        let store = Store::in_memory().expect("store");
        let id = store
            .begin_execution("deck", "p", "anthropic", "claude")
            .expect("execution");
        assert!(
            store
                .record_agent_uses(id, &[row("reviewer", "something-else")])
                .is_err()
        );
    }
}
