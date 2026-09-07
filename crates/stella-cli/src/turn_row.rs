// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Which store row a turn wrote to, handed back to whoever asked for the turn.
//!
//! # The hole this closes
//!
//! `stella run --pipeline <variant>` picks a verdict after its last round ends.
//! Each round is a `crate::agent::run_turn`. That call opens a row of its own.
//! It closes the channel that feeds the row before it returns. So by the time
//! `crate::wrapper_plugin`'s `run_wrapped` holds a verdict, the registry's
//! event slot is empty. The send it made there did nothing at all. Neither
//! `AgentEvent::Verdict` nor `AgentEvent::GateBoard` reached the store.
//!
//! That is a real loss. `crate::dataset_cmd` folds one row's journal, through
//! `Store::execution_events`. It wants the verdict and the turn's own
//! `FileChange` events in one fold. A verdict on any other row is one that fold
//! can never see. The plugin's own row shares nothing but the session id.
//!
//! # What this holds
//!
//! One shared handle on the row the turn opened. Nothing else.
//! `crate::agent::run_turn` sets it through the door it was asked through
//! (`crate::agent::persistence::TurnDoor`). The caller reads it after the turn.
//! It holds no channel. The renderer is gone by then, and
//! [`Store::append_event`] is the write that needs none.
//!
//! A driver that runs many rounds keeps the last row. That is the round the
//! verdict is about: `DispatchReport::verdict` is the last round's. An earlier
//! row would date the call to a turn that was later revised.
//!
//! It sits beside `agent.rs` for `crate::turn_facts`' reason. That file is near
//! the 1500-line ratchet (AGENTS.md § "God files"), so new logic lands in a
//! sibling. `turn_facts` taps the turn's events. This holds the row they went
//! to.

use std::sync::{Arc, Mutex, PoisonError};

use stella_protocol::event::AgentEvent;
use stella_store::Store;

/// A store handle and the row id in it — what `begin_execution` hands back,
/// and what every persistence call site here already passes around.
type ExecutionRow = (Arc<Store>, i64);

/// The store row a turn opened, shared with whoever asked for the turn.
///
/// Cheap to clone, like [`crate::turn_facts::TurnFacts`]: it is an `Arc`
/// inside. The door takes one handle into the turn. The caller keeps another
/// to read after it.
#[derive(Clone, Default)]
pub(crate) struct TurnRow {
    row: Arc<Mutex<Option<ExecutionRow>>>,
}

/// Written by hand, because [`Store`] is not `Debug`. The id is all a reader
/// wants here: `TurnRow(4)`, or `TurnRow(none)` with persistence off.
impl std::fmt::Debug for TurnRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.lock().as_ref() {
            Some((_, id)) => write!(f, "TurnRow({id})"),
            None => f.write_str("TurnRow(none)"),
        }
    }
}

impl TurnRow {
    /// A handle with no row in it yet.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Take the row a turn just opened, over whatever an earlier turn left.
    ///
    /// `None` is a run with persistence off. It clears the handle rather than
    /// keeping the last round's row, so a later append cannot land on a row
    /// this turn never used.
    pub(crate) fn opened(&self, execution: Option<&ExecutionRow>) {
        *self.lock() = execution.cloned();
    }

    /// Add `event` to the held row's journal. Reports whether it landed.
    ///
    /// Best effort, like every other durable write at a turn's close. A run
    /// whose work is done is not made less done by a store that will not
    /// write. The `bool` is for a caller that wants to say so. Nothing here
    /// raises.
    pub(crate) fn append(&self, event: &AgentEvent) -> bool {
        let held = self.lock().clone();
        match held {
            Some((store, id)) => store.append_event(id, event).is_ok(),
            None => false,
        }
    }

    /// The held row, healing a poisoned lock rather than panicking.
    ///
    /// AGENTS.md's no-panics rule, and [`crate::turn_facts::TurnFacts`]'
    /// argument word for word. A panic somewhere else cannot leave an `Option`
    /// in a state this reads wrongly. Losing the verdict's home because some
    /// other thread panicked is the silence this module removes.
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<ExecutionRow>> {
        self.row.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use stella_protocol::event::AgentEvent;
    use stella_store::Store;

    use super::TurnRow;

    fn text(body: &str) -> AgentEvent {
        AgentEvent::Text { text: body.into() }
    }

    /// An append lands on the last row taken, and on no other.
    #[test]
    fn an_append_lands_on_the_last_row_recorded() {
        let store = Arc::new(Store::in_memory().expect("open"));
        let first = store
            .begin_execution("run", "goal", "zai", "glm-5.2")
            .expect("first row");
        let second = store
            .begin_execution("run", "goal", "zai", "glm-5.2")
            .expect("second row");

        let row = TurnRow::new();
        row.opened(Some(&(store.clone(), first)));
        row.opened(Some(&(store.clone(), second)));
        assert!(row.append(&text("the verdict")), "the append landed");

        assert!(
            store
                .execution_events(first)
                .expect("first")
                .events
                .is_empty(),
            "the round that was revised keeps no verdict"
        );
        assert_eq!(
            store.execution_events(second).expect("second").events.len(),
            1,
            "the last round's row is the one the verdict lands on"
        );
    }

    /// With no row, an append says so rather than panicking.
    #[test]
    fn an_append_with_no_row_is_reported_rather_than_panicking() {
        assert!(
            !TurnRow::new().append(&text("nowhere to go")),
            "a run with persistence off holds no row and says so"
        );
    }

    /// A turn that opened no row clears the one before it. An append cannot
    /// land on a round this turn never used.
    #[test]
    fn a_turn_with_no_row_clears_the_previous_one() {
        let store = Arc::new(Store::in_memory().expect("open"));
        let id = store
            .begin_execution("run", "goal", "zai", "glm-5.2")
            .expect("row");
        let row = TurnRow::new();
        row.opened(Some(&(store.clone(), id)));
        row.opened(None);
        assert!(!row.append(&text("nowhere to go")));
        assert!(
            store
                .execution_events(id)
                .expect("journal")
                .events
                .is_empty()
        );
    }
}
