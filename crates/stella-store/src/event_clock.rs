//! The clock stamped on `events.ts`, and the insert that stamps it.
//!
//! The `events` table declares `ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP`
//! (`ddl::events_ddl`). That default holds whole seconds and nothing finer.
//! A fast turn writes many events inside one second. Left to the default,
//! they all share one reading, and a timeline drawn from them can show
//! seconds and no more.
//!
//! `INSERT_EVENT` stamps the column rather than leaving it to the default.
//! `strftime('%Y-%m-%d %H:%M:%f', 'now')` reads the same UTC clock that
//! `CURRENT_TIMESTAMP` reads, and adds a field of milliseconds on the end.
//! The first 19 characters do not change. A reader that slices that prefix
//! still works, and no stored row has to move.
//!
//! Older rows hold whole seconds and stay that way, so a reader of `ts` has
//! to take both widths.
//!
//! The default in the DDL stays as it is. A change there would leave a fresh
//! file and a migrated file with two shapes at one schema version. To agree
//! them costs a rebuild of the largest table in the store, and the default it
//! would install is one this writer always overrides.

/// The `events` insert, with `ts` stamped to the millisecond.
///
/// The five placeholders bind `execution_id`, `seq`, `event_type`, `payload`
/// and `task_id`, in that order. `ts` is SQL, not a bound value, so a row is
/// dated by the database that stores it and never by a caller's clock.
pub(crate) const INSERT_EVENT: &str = "INSERT INTO events \
     (execution_id, seq, ts, event_type, payload, task_id) \
     VALUES (?, ?, strftime('%Y-%m-%d %H:%M:%f', 'now'), ?, ?, ?)";

#[cfg(test)]
mod tests {
    use stella_protocol::AgentEvent;

    use crate::Store;

    /// **The witness.** Every recorded event carries a millisecond field.
    ///
    /// A row that took the DDL default would be 19 characters wide and hold
    /// no `.`, so it fails every check below. This asks the shape of one
    /// stamp. It does not compare two, so a machine fast enough to write both
    /// rows inside one millisecond cannot make it flap.
    #[test]
    fn a_recorded_event_is_stamped_to_the_millisecond() {
        let store = Store::in_memory().unwrap();
        let id = store
            .begin_execution("run", "goal", "zai", "glm-5.2")
            .unwrap();
        store
            .record_event(id, 0, &AgentEvent::Text { text: "a".into() })
            .unwrap();
        store
            .record_event(id, 1, &AgentEvent::Text { text: "b".into() })
            .unwrap();

        let journal = store.execution_events(id).unwrap();
        assert_eq!(journal.events.len(), 2, "both events landed");
        for row in &journal.events {
            let ts = &row.ts;
            assert_eq!(ts.len(), 23, "expected YYYY-MM-DD HH:MM:SS.sss, got {ts:?}");
            assert_eq!(&ts[19..20], ".", "no millisecond field in {ts:?}");
            assert!(
                ts[20..].chars().all(|c| c.is_ascii_digit()),
                "millisecond field is not three digits: {ts:?}"
            );
            assert!(
                ts[..19]
                    .chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, '-' | ' ' | ':')),
                "the 19-character prefix a reader slices changed shape: {ts:?}"
            );
        }
    }
}
