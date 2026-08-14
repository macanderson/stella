//! The end of one turn's durable record, in one place.
//!
//! A turn boundary has always had two writes, and they were spelled out
//! separately at every driver: close the `executions` row (what ran, what it
//! cost), then mark the workspace state the turn ended at so the per-turn
//! diff projection has something to key off (#1870).
//!
//! Spelling them separately is how they came apart. `mark_turn_end` ended up
//! with exactly **one** production caller — the Command Deck's
//! `run_deck_session`, reached only from a real TTY or `stella resume` — while
//! every headless driver closed its execution row and stopped there. The
//! observatory's read side was universal from day one, so `stella run`, CI,
//! benchmarks and fleet workers all showed a permanently empty turn drill,
//! with nothing to distinguish "this turn changed nothing" from "this surface
//! never records diffs at all" (#2177). A dashboard that is silently partial
//! is worse than one that is visibly empty.
//!
//! [`close_turn`] is the fix in shape as well as in behaviour: a driver that
//! ends a turn calls one function, and cannot end it half-recorded.
//!
//! It lives beside `agent.rs` rather than inside it because `agent.rs` is a
//! grandfathered god file, closed to growth (AGENTS.md § "God files — plan
//! around them, never into them").

use std::sync::Arc;

use stella_store::Store;
use stella_tools::ToolRegistry;

use super::{record_execution_end, warn_store_write_failed};
use crate::config::Config;

/// How one turn ended, as the `executions` row records it.
pub(crate) struct TurnOutcomeRecord<'a> {
    /// `executions.outcome` — `completed`, `aborted`, `goal_unmet`, …
    pub(crate) label: &'a str,
    /// Everything this turn spent, reflection included.
    pub(crate) cost_usd: f64,
    /// Whether every event of the turn made it to the durable journal.
    pub(crate) persistence_complete: bool,
}

/// Close one turn: settle its execution row, then mark the workspace state it
/// ended at.
///
/// `execution` is `None` for a claim-mode trial (no store at all) and
/// `session_id` is `None` for a driver with no registry record; each half
/// degrades on its own, and neither can silently skip the other.
///
/// Best-effort throughout, matching every other store write on a turn's exit
/// path: a turn that ended is not made less ended by an unwritable row. The
/// execution half warns, because an operator who loses their audit record
/// should hear about it; the turn mark stays silent, because it is a
/// convenience projection and the deck has always treated it that way.
///
/// An **aborted** turn is marked too: the deck's own boundary has never
/// conditioned the mark on the outcome.
pub(crate) fn close_turn(
    cfg: &Config,
    store: &Option<Arc<Store>>,
    execution: &Option<(Arc<Store>, i64)>,
    registry: &ToolRegistry,
    session_id: Option<&str>,
    outcome: TurnOutcomeRecord<'_>,
) {
    if let Some((execution_store, id)) = execution
        && !record_execution_end(
            execution_store,
            *id,
            registry,
            outcome.label,
            outcome.cost_usd,
            outcome.persistence_complete,
        )
    {
        warn_store_write_failed("the audit record (agent uses / MCP usage / outcome)");
    }
    if let Some(session_id) = session_id {
        cfg.durability
            .mark_turn_end(store, session_id, execution.as_ref().map(|(_, id)| *id));
    }
}
