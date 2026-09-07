// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The two ways an [`AgentEvent`] reaches an execution's journal.
//!
//! [`Store::record_event`] is the streaming writer. A renderer draining a live
//! turn counts what it has written. It hands the next number in. The sequence
//! is the caller's to keep.
//!
//! [`Store::append_event`] is for a writer that comes after that renderer is
//! gone. It has no count to hand in. So it reads the next number out of the
//! table, in the same transaction that writes the row. Two appenders cannot
//! pick one number that way.
//!
//! `stella run --pipeline <variant>` is what asked for it. Each round opens a
//! row and closes the channel that fed it. The verdict is picked after the last
//! round has done both (`stella_cli::wrapper_plugin`'s `run_wrapped`).
//!
//! Both write the row and its `tool_calls` projection in one transaction, for
//! the reason [`Store::record_event`] gives.

use rusqlite::{Transaction, params};
use stella_protocol::AgentEvent;

use crate::{Result, Store, StoreError, event_clock, sqlite_i64, tool_calls};

/// Write one event row and project it, inside `tx`.
///
/// Shared by the two writers above. The payload, the type tag, the `task_id`
/// lift and the projection get one statement rather than two that can drift.
fn write_event(
    tx: &Transaction<'_>,
    execution_id: i64,
    seq: i64,
    event: &AgentEvent,
) -> Result<()> {
    let payload = serde_json::to_string(event).map_err(|e| StoreError::Other(e.to_string()))?;
    // Read the tagged `type` by DESERIALIZING it. Never scan for the first
    // `"type":"` in the text. That scan yields the wrong tag, or "unknown",
    // the moment the JSON is pretty-printed, wrapped, or reordered. A
    // one-field struct keeps the guarantee and builds no throwaway tree. A
    // `tool_result` payload holds the whole tool output, and this runs once
    // per event on the streaming path.
    #[derive(serde::Deserialize)]
    struct EventTag {
        #[serde(rename = "type")]
        ty: String,
    }
    let event_type = serde_json::from_str::<EventTag>(&payload)
        .map(|tag| tag.ty)
        .unwrap_or_else(|_| "unknown".into());
    let task_id = event.task_id().map(stella_protocol::TaskId::as_str);
    tx.execute(
        event_clock::INSERT_EVENT,
        params![execution_id, seq, event_type, payload, task_id],
    )?;
    tool_calls::project_event(tx, execution_id, seq, event)?;
    Ok(())
}

impl Store {
    /// Add one event to the stream at `seq`, and fold it into the
    /// `tool_calls` projection in the same transaction.
    ///
    /// The two writes are atomic **on purpose**. `events` is the truth.
    /// `tool_calls` is drawn from it. Any window where one has landed and the
    /// other has not is a window where the dashboard's counts disagree with
    /// the log. Committing them together shuts that window. No order of these
    /// two statements can leave a `tool_start` in the log without its row.
    ///
    /// Before v18 the projection was built once, at turn end. A live turn
    /// reported zero tool calls. An interrupted one reported zero forever.
    /// See [`Store::materialize_tool_calls`] for the whole story. It is the
    /// repair path now, not the only writer.
    ///
    /// The event's own `task_id` is lifted into its column in the same
    /// transaction, for the same reason. [`Store::task_events`] is the
    /// selection it exists for.
    pub fn record_event(&self, execution_id: i64, seq: u64, event: &AgentEvent) -> Result<()> {
        let seq = sqlite_i64("event sequence", seq)?;
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        write_event(&tx, execution_id, seq, event)?;
        tx.commit()?;
        Ok(())
    }

    /// Append `event` to `execution_id`'s journal at the next free sequence.
    /// Reports the sequence it took.
    ///
    /// For a writer with nothing to count from: the round's renderer owned the
    /// sequence and is gone. Reading `max(seq) + 1` inside the writing
    /// transaction is what makes that safe. A second appender blocks on the
    /// same connection lock. It then reads the number this call committed. So
    /// neither can pick the other's sequence, and
    /// `UNIQUE (execution_id, seq)` never has to settle it.
    ///
    /// The row it lands on is a finished execution. That is ordinary. Nothing
    /// in the schema closes a journal, and every reader of one
    /// ([`Store::execution_events`]) orders by sequence. So an appended event
    /// reads as the last thing that happened to that execution, which it is.
    ///
    /// # Errors
    ///
    /// Whatever the write refuses: a store that will not open a transaction,
    /// or an event that will not encode.
    pub fn append_event(&self, execution_id: i64, event: &AgentEvent) -> Result<u64> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let seq: i64 = tx.query_row(
            "SELECT coalesce(max(seq) + 1, 0) FROM events WHERE execution_id = ?1",
            params![execution_id],
            |row| row.get(0),
        )?;
        write_event(&tx, execution_id, seq, event)?;
        tx.commit()?;
        Ok(seq as u64)
    }
}

#[cfg(test)]
mod tests {
    use stella_protocol::AgentEvent;

    use crate::Store;

    fn text(body: &str) -> AgentEvent {
        AgentEvent::Text { text: body.into() }
    }

    /// **The witness.** An append lands after everything the streaming writer
    /// put there, and says which sequence it took.
    ///
    /// A writer that guessed `0` — the only number a caller with no count has —
    /// would hit the first streamed event and fail.
    #[test]
    fn an_append_takes_the_sequence_after_the_last_recorded_event() {
        let store = Store::in_memory().expect("open");
        let id = store
            .begin_execution("run", "goal", "zai", "glm-5.2")
            .expect("execution");
        store.record_event(id, 0, &text("a")).expect("first");
        store.record_event(id, 1, &text("b")).expect("second");

        let at = store.append_event(id, &text("c")).expect("append");
        assert_eq!(at, 2, "the append takes the next free sequence");

        let seqs: Vec<i64> = store
            .execution_events(id)
            .expect("journal")
            .events
            .iter()
            .map(|row| row.seq)
            .collect();
        assert_eq!(seqs, vec![0, 1, 2], "and the journal reads in order");
    }

    /// An empty journal starts at zero rather than refusing. A turn that wrote
    /// nothing still takes an appended event.
    #[test]
    fn an_append_to_an_empty_journal_starts_at_zero() {
        let store = Store::in_memory().expect("open");
        let id = store
            .begin_execution("run", "goal", "zai", "glm-5.2")
            .expect("execution");
        assert_eq!(store.append_event(id, &text("only")).expect("append"), 0);
    }

    /// Two appends take two sequences. The second reads the first out of the
    /// table rather than off a count the caller kept.
    #[test]
    fn two_appends_take_two_sequences() {
        let store = Store::in_memory().expect("open");
        let id = store
            .begin_execution("run", "goal", "zai", "glm-5.2")
            .expect("execution");
        assert_eq!(store.append_event(id, &text("a")).expect("first"), 0);
        assert_eq!(store.append_event(id, &text("b")).expect("second"), 1);
    }
}
