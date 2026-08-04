//! The `execution_reflection` writers — the agent's own account of one turn.
//!
//! Its own module rather than more methods on the already-oversized `lib.rs`,
//! per the working rule the file-size gate enforces: new code goes in a new
//! module instead of raising a grandfathered ceiling.
//!
//! The row has two halves that are written by *different* producers at
//! *different* times, and keeping them from overwriting each other is the whole
//! reason this module exists:
//!
//! - the **objective** half (`prompt`, `produced_output`, `wrote_files`,
//!   `truncated`) is derived from the event and file-touch logs by
//!   [`Store::finalize_execution_reflection`], inside `record_execution_end`.
//! - the **self-review** half (`delivered`, `self_rating`, `what_went_well`,
//!   `what_to_improve`, `critique`) comes from the model, via the post-turn
//!   reflection call, and lands through [`Store::record_self_review`].
//!
//! Both used to funnel through one `INSERT OR REPLACE`, which meant last write
//! wins *over the whole row*. Since finalize runs after reflection in every
//! path, a whole-row replace with the self-review fields defaulted to
//! `None`/`""` would erase the model's assessment moments after it was
//! recorded. Each writer therefore upserts only the columns it actually owns,
//! and the two are order-independent.

use rusqlite::{OptionalExtension, params};

use crate::{ExecutionReflectionRow, Result, Store};

/// The model-authored half of one turn's reflection: the agent grading its own
/// work. Every field is what the model said, never a derived fact — the
/// objective companions live on [`ExecutionReflectionRow`] and are written by
/// [`Store::finalize_execution_reflection`].
///
/// `delivered` and `self_rating` are `Option` because a model that answers
/// without them is the common case, and inventing a number would put a
/// fabricated score in front of the user under a label that promises the
/// model's own words.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelfReviewRow {
    /// Did the agent, by its own account, deliver what was asked?
    pub delivered: Option<bool>,
    /// 0–10, the model's own score for the turn.
    pub self_rating: Option<i64>,
    pub what_went_well: String,
    pub what_to_improve: String,
    pub critique: String,
}

impl SelfReviewRow {
    /// Whether this carries anything worth a write. A review with no rating, no
    /// verdict and no prose is indistinguishable from the empty row finalize
    /// already writes, and storing it would turn "the model declined to assess
    /// this turn" into a row that looks assessed.
    pub fn is_empty(&self) -> bool {
        self.delivered.is_none()
            && self.self_rating.is_none()
            && self.what_went_well.trim().is_empty()
            && self.what_to_improve.trim().is_empty()
            && self.critique.trim().is_empty()
    }
}

impl Store {
    /// Record (or replace) the agent's self-review for one turn, 1:1 with its
    /// execution.
    ///
    /// Writes the whole row, self-review half included. Prefer
    /// [`Store::record_self_review`] for a model-emitted assessment and
    /// [`Store::finalize_execution_reflection`] for the derived half — this one
    /// clobbers whatever the other producer already wrote.
    pub fn record_execution_reflection(
        &self,
        execution_id: i64,
        r: &ExecutionReflectionRow,
    ) -> Result<()> {
        self.lock().execute(
            "INSERT OR REPLACE INTO execution_reflection \
             (execution_id, prompt, delivered, self_rating, what_went_well, \
              what_to_improve, critique, produced_output, wrote_files, truncated) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                execution_id,
                r.prompt,
                r.delivered.map(|b| b as i64),
                r.self_rating,
                r.what_went_well,
                r.what_to_improve,
                r.critique,
                r.produced_output as i64,
                r.wrote_files as i64,
                r.truncated as i64,
            ],
        )?;
        Ok(())
    }

    /// Record the model's own assessment of one turn, touching *only* the
    /// self-review columns.
    ///
    /// Scoped this narrowly on purpose: the derived half is written separately,
    /// after this in every path, and a whole-row write here (or there) would
    /// mean whichever producer finished last silently discarded the other's
    /// work. The row is created if reflection beat finalize to it — every
    /// column this statement omits carries a DDL default.
    pub fn record_self_review(&self, execution_id: i64, r: &SelfReviewRow) -> Result<()> {
        self.lock().execute(
            "INSERT INTO execution_reflection \
             (execution_id, delivered, self_rating, what_went_well, \
              what_to_improve, critique) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(execution_id) DO UPDATE SET \
               delivered = excluded.delivered, \
               self_rating = excluded.self_rating, \
               what_went_well = excluded.what_went_well, \
               what_to_improve = excluded.what_to_improve, \
               critique = excluded.critique",
            params![
                execution_id,
                r.delivered.map(|b| b as i64),
                r.self_rating,
                r.what_went_well,
                r.what_to_improve,
                r.critique,
            ],
        )?;
        Ok(())
    }

    /// Read back one turn's self-review, if the model recorded one.
    ///
    /// `None` for an execution with no row at all; `Some` with an
    /// [`SelfReviewRow::is_empty`] value for a row finalize created but no model
    /// ever graded — the two are different answers and a caller reporting
    /// "unrated" wants to tell them apart.
    pub fn self_review(&self, execution_id: i64) -> Result<Option<SelfReviewRow>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT delivered, self_rating, what_went_well, what_to_improve, critique \
                 FROM execution_reflection WHERE execution_id = ?1",
                params![execution_id],
                |r| {
                    Ok(SelfReviewRow {
                        delivered: r.get::<_, Option<i64>>(0)?.map(|d| d != 0),
                        self_rating: r.get(1)?,
                        what_went_well: r.get(2)?,
                        what_to_improve: r.get(3)?,
                        critique: r.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    /// Derive and record the objective half of this turn's
    /// `execution_reflection` — prompt, plus `produced_output` / `wrote_files`
    /// / `truncated` computed from the event and file-touch logs.
    ///
    /// Leaves the self-review columns exactly as it found them:
    /// [`Store::record_self_review`] has usually already written them by the
    /// time this runs, and the whole-row replace this used to do erased them.
    pub fn finalize_execution_reflection(&self, execution_id: i64) -> Result<()> {
        let (prompt, produced_output, wrote_files, truncated) = {
            let conn = self.lock();
            let prompt: String = conn
                .query_row(
                    "SELECT prompt FROM executions WHERE id = ?1",
                    params![execution_id],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or_default();
            let produced_output: bool = conn.query_row(
                "SELECT COUNT(*) FROM events \
                 WHERE execution_id = ?1 AND event_type IN ('text', 'tool_start')",
                params![execution_id],
                |r| r.get::<_, i64>(0),
            )? > 0;
            // "output-token limit" is the phrase every truncation emitter
            // shares (stella-core's empty-turn abort, stella-model's
            // truncated tool-input error); no wider net — a bare "truncated"
            // substring also matched unrelated failures that merely mention
            // the word (a torn fixture, a cut-short MCP tool list).
            let truncated: bool = conn.query_row(
                "SELECT COUNT(*) FROM events \
                 WHERE execution_id = ?1 AND event_type = 'error' \
                   AND payload LIKE '%output-token limit%'",
                params![execution_id],
                |r| r.get::<_, i64>(0),
            )? > 0;
            let wrote_files: bool = conn.query_row(
                "SELECT COUNT(*) FROM files_touched \
                 WHERE execution_id = ?1 \
                   AND (ops LIKE '%C%' OR ops LIKE '%U%' OR ops LIKE '%D%')",
                params![execution_id],
                |r| r.get::<_, i64>(0),
            )? > 0;
            (prompt, produced_output, wrote_files, truncated)
        };
        self.lock().execute(
            "INSERT INTO execution_reflection \
             (execution_id, prompt, produced_output, wrote_files, truncated) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(execution_id) DO UPDATE SET \
               prompt = excluded.prompt, \
               produced_output = excluded.produced_output, \
               wrote_files = excluded.wrote_files, \
               truncated = excluded.truncated",
            params![
                execution_id,
                prompt,
                produced_output as i64,
                wrote_files as i64,
                truncated as i64,
            ],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_review() -> SelfReviewRow {
        SelfReviewRow {
            delivered: Some(true),
            self_rating: Some(8),
            what_went_well: "read the failing test before editing".into(),
            what_to_improve: "checked the gate by grep instead of exit code".into(),
            critique: "landed, but the first diagnosis was wrong".into(),
        }
    }

    fn row(store: &Store, execution_id: i64) -> (Option<i64>, Option<i64>, String, String, i64) {
        let conn = store.lock();
        conn.query_row(
            "SELECT delivered, self_rating, what_to_improve, prompt, produced_output \
             FROM execution_reflection WHERE execution_id = ?1",
            params![execution_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap()
    }

    /// The order `record_execution_end` actually uses: the model reflects during
    /// the turn, finalize closes the row afterwards. Finalize used to write the
    /// whole row with the self-review fields defaulted, so it erased the
    /// assessment moments after it was recorded — which is why `self_rating` was
    /// NULL on every row in a real workspace even once a producer existed.
    #[test]
    fn finalize_preserves_a_self_review_it_did_not_write() {
        let store = Store::in_memory().unwrap();
        let id = store
            .begin_execution("deck-pipeline", "ship it", "zai", "glm-5.2")
            .unwrap();
        store.record_self_review(id, &a_review()).unwrap();
        store.finalize_execution_reflection(id).unwrap();
        let (delivered, rating, improve, prompt, _) = row(&store, id);
        assert_eq!(delivered, Some(1), "the model's verdict survived finalize");
        assert_eq!(rating, Some(8), "the model's rating survived finalize");
        assert_eq!(improve, a_review().what_to_improve);
        assert_eq!(prompt, "ship it", "and the derived half still landed");
    }

    /// The reverse order, because nothing serializes the two producers: a
    /// self-review arriving after finalize must not wipe the derived half.
    #[test]
    fn a_self_review_preserves_the_derived_half() {
        let store = Store::in_memory().unwrap();
        let id = store
            .begin_execution("deck-pipeline", "ship it", "zai", "glm-5.2")
            .unwrap();
        store
            .record_event(
                id,
                0,
                &stella_protocol::AgentEvent::Text {
                    delta: "done".into(),
                },
            )
            .unwrap();
        store.finalize_execution_reflection(id).unwrap();
        store.record_self_review(id, &a_review()).unwrap();
        let (_, rating, _, prompt, produced_output) = row(&store, id);
        assert_eq!(rating, Some(8));
        assert_eq!(prompt, "ship it", "derived prompt survived the self-review");
        assert_eq!(produced_output, 1, "derived output flag survived too");
    }

    /// A self-review that arrives before finalize creates the row rather than
    /// silently dropping — the reflection call can and does win the race.
    #[test]
    fn a_self_review_creates_the_row_when_it_arrives_first() {
        let store = Store::in_memory().unwrap();
        let id = store
            .begin_execution("deck-pipeline", "ship it", "zai", "glm-5.2")
            .unwrap();
        store.record_self_review(id, &a_review()).unwrap();
        assert_eq!(row(&store, id).1, Some(8));
    }

    #[test]
    fn an_all_empty_review_reports_itself_empty() {
        assert!(SelfReviewRow::default().is_empty());
        assert!(
            SelfReviewRow {
                what_to_improve: "   ".into(),
                ..Default::default()
            }
            .is_empty(),
            "whitespace is not an assessment"
        );
        assert!(!a_review().is_empty());
        assert!(
            !SelfReviewRow {
                self_rating: Some(0),
                ..Default::default()
            }
            .is_empty(),
            "0/10 is a real rating, not a missing one"
        );
    }
}
