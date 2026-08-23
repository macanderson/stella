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
              what_to_improve, critique, produced_output, wrote_files, truncated, \
              partial_run) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
                r.partial_run as i64,
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

    /// Record that this turn's reflection response could not be parsed into a
    /// lessons array, touching *only* the `parse_error` column (#2175).
    ///
    /// Narrow for the same reason [`Store::record_self_review`] is: three
    /// producers write this row at different times, and a whole-row write from
    /// any of them discards the others' work.
    ///
    /// The excerpt is model output about the user's own transcript. It belongs
    /// in the workspace-local `store.db`, which already holds prompts and tool
    /// payloads, and must never become an exportable telemetry column —
    /// `content_free.rs` holds the reviewed hub allowlist and will fail the
    /// gate if one is added without a human answering "is this content?".
    pub fn record_reflection_parse_failure(&self, execution_id: i64, excerpt: &str) -> Result<()> {
        self.lock().execute(
            "INSERT INTO execution_reflection (execution_id, parse_error) VALUES (?1, ?2) \
             ON CONFLICT(execution_id) DO UPDATE SET parse_error = excluded.parse_error",
            params![execution_id, excerpt],
        )?;
        Ok(())
    }

    /// The recorded parse failure for one turn, if reflection had one.
    ///
    /// Three answers, and they are genuinely different: `None` for an
    /// execution with no reflection row at all, `Some(None)` for a turn that
    /// reflected cleanly, and `Some(Some(excerpt))` for a turn whose response
    /// could not be read. Collapsing the last two is the defect this exists to
    /// end — "nothing to learn yet" and "the learning loop has been broken for
    /// nine days" looked identical from every surface.
    pub fn reflection_parse_failure(&self, execution_id: i64) -> Result<Option<Option<String>>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT parse_error FROM execution_reflection WHERE execution_id = ?1",
                params![execution_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?)
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
    /// / `truncated` computed from the event and file-touch logs, and
    /// `partial_run` read off the outcome.
    ///
    /// Leaves the self-review columns exactly as it found them:
    /// [`Store::record_self_review`] has usually already written them by the
    /// time this runs, and the whole-row replace this used to do erased them.
    ///
    /// # Why a cancelled run still gets a row (#3808)
    ///
    /// Because the work it did is real and is measured here. A cancel lands
    /// before the reflection model call, so the self-review half stays empty —
    /// and until `partial_run` existed that empty row was indistinguishable
    /// from a turn the model looked at and declined to assess, which is the
    /// reading that made the runs most worth learning from look like the ones
    /// with nothing to say. Marking the scope is the answer; withholding the
    /// row would throw away the objective half as well.
    ///
    /// Derived here rather than passed in by the caller: the outcome is a
    /// column this function is already inside a read of, and a parameter would
    /// be a second copy of it for the caller to get wrong.
    /// `executions.usage_complete` is untouched and unrelated — that one is
    /// about the provider's usage envelope, which a cancel genuinely makes
    /// unknowable.
    pub fn finalize_execution_reflection(&self, execution_id: i64) -> Result<()> {
        let (prompt, partial_run, produced_output, wrote_files, truncated) = {
            let conn = self.lock();
            let (prompt, outcome): (String, Option<String>) = conn
                .query_row(
                    "SELECT prompt, outcome FROM executions WHERE id = ?1",
                    params![execution_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?
                .unwrap_or_default();
            // The one outcome that stops a turn before it can be assessed.
            // Matched against the label `record_execution_end` writes, which is
            // the same string `terminal_usage_known` reads.
            let partial_run = outcome.as_deref() == Some("cancelled");
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
            // Two sources, because either alone answers "did this turn write
            // anything?" with a confident no when it cannot see (#3413's
            // measurement, and the tool ledger, have disjoint blind spots):
            //
            // - `files_touched` is the turn-boundary tree measurement. It is
            //   the better evidence — it sees `bash`, MCP servers and custom
            //   script tools, none of which name a path in a schema the engine
            //   reads — but it is empty for a session with no work journal
            //   bound, on the session's first snapshot (baseline only), AND
            //   whenever the snapshot itself failed, which
            //   `SessionDurability::snapshot_worktree` cannot distinguish from
            //   a clean tree.
            // - A file-writing tool call that returned `ok` is weaker evidence
            //   about *what* landed — a tool's arguments are a request, not a
            //   measurement, which is why `stella-cli`'s `turn_files` refuses
            //   to synthesize line counts from them (#2290). But this column
            //   is a **boolean**, not a count, and for that question a
            //   succeeded `edit_file` is a fact. It can only ever turn a wrong
            //   `false` into a right `true`.
            //
            // Witnessed in session `ses-1787342320630-36613`: nineteen
            // successful `edit_file` calls against two files that git confirms
            // were left modified, `files_touched` empty, and this column
            // reported `false` — telling the reflection loop a turn that
            // edited one file sixteen times had written nothing.
            let wrote_files: bool = conn.query_row(
                "SELECT \
                   (SELECT COUNT(*) FROM files_touched \
                     WHERE execution_id = ?1 \
                       AND (ops LIKE '%C%' OR ops LIKE '%U%' OR ops LIKE '%D%')) \
                 + (SELECT COUNT(*) FROM tool_calls \
                     WHERE execution_id = ?1 AND ok = 1 \
                       AND name IN ('write_file', 'edit_file', 'delete_file'))",
                params![execution_id],
                |r| r.get::<_, i64>(0),
            )? > 0;
            (prompt, partial_run, produced_output, wrote_files, truncated)
        };
        self.lock().execute(
            "INSERT INTO execution_reflection \
             (execution_id, prompt, produced_output, wrote_files, truncated, partial_run) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(execution_id) DO UPDATE SET \
               prompt = excluded.prompt, \
               produced_output = excluded.produced_output, \
               wrote_files = excluded.wrote_files, \
               truncated = excluded.truncated, \
               partial_run = excluded.partial_run",
            params![
                execution_id,
                prompt,
                produced_output as i64,
                wrote_files as i64,
                truncated as i64,
                partial_run as i64,
            ],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrote_files_of(store: &Store, execution_id: i64) -> i64 {
        column_of(store, execution_id, "wrote_files")
    }

    fn partial_run_of(store: &Store, execution_id: i64) -> i64 {
        column_of(store, execution_id, "partial_run")
    }

    /// One integer column of one reflection row. The column name is a literal
    /// from this module's own tests, never caller input.
    fn column_of(store: &Store, execution_id: i64, column: &str) -> i64 {
        let conn = store.lock();
        conn.query_row(
            &format!("SELECT {column} FROM execution_reflection WHERE execution_id = ?1"),
            params![execution_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    use stella_protocol::event::AgentEvent;
    use stella_protocol::tool::{ToolCall, ToolOutput};

    fn started(call_id: &str, name: &str) -> AgentEvent {
        AgentEvent::ToolStart {
            call: ToolCall {
                call_id: call_id.into(),
                name: name.into(),
                input: serde_json::json!({"path": "src/engine.rs"}),
            },
        }
    }

    fn settled(call_id: &str, output: ToolOutput) -> AgentEvent {
        AgentEvent::ToolResult {
            call_id: call_id.into(),
            output,
            duration_ms: 4,
            speculated: false,
        }
    }

    fn ok_output(content: &str) -> ToolOutput {
        ToolOutput::Ok {
            content: content.into(),
            data: None,
        }
    }

    /// The witness: a turn whose only evidence of writing is the tool ledger
    /// is still a turn that wrote.
    ///
    /// `files_touched` has three benign ways to be empty (unbound journal,
    /// first snapshot of the session, genuinely clean tree) and one that is
    /// not benign at all — a failed measurement, which
    /// `SessionDurability::snapshot_worktree` discards into the same empty
    /// vector. Deriving this column from that table alone therefore reported
    /// a confident `false` whenever the measurement could not see.
    ///
    /// Observed in session `ses-1787342320630-36613`: nineteen successful
    /// `edit_file` calls against two files git confirms were left modified,
    /// zero `files_touched` rows, `wrote_files = 0`.
    #[test]
    fn a_turn_that_edited_files_wrote_files_even_with_no_tree_measurement() {
        let store = Store::in_memory().unwrap();
        let id = store
            .begin_execution("deck", "fix the search ladder", "openrouter", "kimi-k3")
            .unwrap();
        store
            .record_event(id, 0, &started("c1", "edit_file"))
            .unwrap();
        store
            .record_event(id, 1, &settled("c1", ok_output("edited")))
            .unwrap();
        store.materialize_tool_calls(id).unwrap();

        // The tree measurement produced nothing — the defect's exact shape.
        assert_eq!(store.count("files_touched").unwrap(), 0);

        store.finalize_execution_reflection(id).unwrap();
        assert_eq!(
            wrote_files_of(&store, id),
            1,
            "a succeeded edit_file is a fact about whether the turn wrote, even when the \
             turn-boundary snapshot saw nothing"
        );
    }

    /// **#3808's witness.** A cancelled run's reflection used to be a silent
    /// stub — empty critique, NULL rating, and nothing saying why — so the runs
    /// most worth learning from read exactly like the ones the model looked at
    /// and had nothing to say about.
    ///
    /// The shape is the audited one (`ses-1787092335250-47513`, execution 157):
    /// real edits landed, then a human stopped the run. The work is still
    /// measured, and the row now states the scope that measurement covers.
    #[test]
    fn a_cancelled_runs_reflection_says_it_covers_a_partial_run() {
        let store = Store::in_memory().unwrap();
        let id = store
            .begin_execution("deck", "port the build", "openrouter", "kimi-k3")
            .unwrap();
        store
            .record_event(id, 0, &started("c1", "edit_file"))
            .unwrap();
        store
            .record_event(id, 1, &settled("c1", ok_output("edited")))
            .unwrap();
        store.materialize_tool_calls(id).unwrap();
        // A human stopped it. The label is the one `record_execution_end`
        // writes, and `finalize_execution_reflection` runs after it on every
        // path including this one.
        store
            .finish_execution_accounted(id, "cancelled", 0.0, false)
            .unwrap();

        store.finalize_execution_reflection(id).unwrap();

        assert_eq!(
            partial_run_of(&store, id),
            1,
            "a stopped run's reflection must say it is about a stopped run"
        );
        assert_eq!(
            wrote_files_of(&store, id),
            1,
            "and must not throw away the half that is still true: the edits happened"
        );
    }

    /// The other direction, or the column says nothing: a turn that reached its
    /// own end is not partial, however it ended.
    #[test]
    fn a_run_that_reached_its_own_end_is_not_partial() {
        let store = Store::in_memory().unwrap();
        for outcome in ["success", "error", "budget_exhausted"] {
            let id = store
                .begin_execution("run", outcome, "openrouter", "kimi-k3")
                .unwrap();
            store
                .finish_execution_accounted(id, outcome, 0.0, true)
                .unwrap();
            store.finalize_execution_reflection(id).unwrap();
            assert_eq!(partial_run_of(&store, id), 0, "{outcome}");
        }
    }

    /// The self-review producer writes first in every live path, and finalize
    /// must add the scope without erasing the assessment — the clobber #2175's
    /// narrowing already fixed once, now with one more column to lose.
    #[test]
    fn finalize_marks_a_partial_run_without_erasing_a_self_review() {
        let store = Store::in_memory().unwrap();
        let id = store
            .begin_execution("deck", "port the build", "openrouter", "kimi-k3")
            .unwrap();
        store
            .record_self_review(
                id,
                &SelfReviewRow {
                    delivered: Some(false),
                    self_rating: Some(3),
                    what_went_well: "the build passed".into(),
                    what_to_improve: "stopped short".into(),
                    critique: "half the migration landed".into(),
                },
            )
            .unwrap();
        store
            .finish_execution_accounted(id, "cancelled", 0.0, false)
            .unwrap();

        store.finalize_execution_reflection(id).unwrap();

        assert_eq!(partial_run_of(&store, id), 1);
        let critique: String = {
            let conn = store.lock();
            conn.query_row(
                "SELECT critique FROM execution_reflection WHERE execution_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(critique, "half the migration landed");
    }

    /// The other direction must not drift: a read-only turn still wrote
    /// nothing, and a *failed* write is not a write.
    #[test]
    fn reading_and_failing_to_write_are_both_still_wrote_nothing() {
        let store = Store::in_memory().unwrap();
        let id = store
            .begin_execution("deck", "look around", "openrouter", "kimi-k3")
            .unwrap();
        store
            .record_event(id, 0, &started("c1", "read_file"))
            .unwrap();
        store
            .record_event(id, 1, &settled("c1", ok_output("fn main() {}")))
            .unwrap();
        // An edit that did not land. `ok = 0`, so it proves nothing.
        store
            .record_event(id, 2, &started("c2", "edit_file"))
            .unwrap();
        store
            .record_event(
                id,
                3,
                &settled("c2", ToolOutput::error("old_string not found")),
            )
            .unwrap();
        store.materialize_tool_calls(id).unwrap();

        store.finalize_execution_reflection(id).unwrap();
        assert_eq!(
            wrote_files_of(&store, id),
            0,
            "a read is not a write, and neither is an edit that was refused"
        );
    }

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
                    text: "done".into(),
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

    /// #2175 witness: a turn whose reflection response could not be parsed
    /// leaves a durable record, and a turn that reflected cleanly does not.
    ///
    /// The two states are the whole point. Before this column, both wrote
    /// nothing to the store — an unreadable response was `eprintln!`'d and the
    /// reflection execution still closed `completed`, because the *model call*
    /// did complete. Once the scrollback was gone, a learning loop that had
    /// been broken for nine days and a workspace that had simply never learned
    /// anything were the same picture from every surface.
    #[test]
    fn a_parse_failure_is_durable_and_a_clean_reflection_is_not_marked_as_one() {
        let store = Store::in_memory().unwrap();
        let unreadable = store
            .begin_execution("reflection", "ship it", "zai", "glm-5.2")
            .unwrap();
        let clean = store
            .begin_execution("reflection", "ship it again", "zai", "glm-5.2")
            .unwrap();

        store
            .record_reflection_parse_failure(unreadable, "I will now analyze the turn. First,")
            .unwrap();
        store.record_self_review(clean, &a_review()).unwrap();

        assert_eq!(
            store.reflection_parse_failure(unreadable).unwrap(),
            Some(Some("I will now analyze the turn. First,".into())),
            "the excerpt is what tells an operator WHY the loop is starving"
        );
        assert_eq!(
            store.reflection_parse_failure(clean).unwrap(),
            Some(None),
            "a turn that reflected cleanly has a row and no parse failure — \
             recording one here would make every healthy turn look broken"
        );
        assert_eq!(
            store.reflection_parse_failure(unreadable + 1_000).unwrap(),
            None,
            "and an execution that never reflected has no row at all: three \
             states, three answers"
        );
    }

    /// The third producer must not clobber the other two, in either order —
    /// the same order-independence `record_self_review` and
    /// `finalize_execution_reflection` already hold to.
    #[test]
    fn a_parse_failure_and_the_other_producers_do_not_clobber_each_other() {
        for parse_first in [true, false] {
            let store = Store::in_memory().unwrap();
            let id = store
                .begin_execution("deck-pipeline", "ship it", "zai", "glm-5.2")
                .unwrap();
            let record_parse = || {
                store
                    .record_reflection_parse_failure(id, "unreadable")
                    .unwrap()
            };
            if parse_first {
                record_parse();
                store.record_self_review(id, &a_review()).unwrap();
                store.finalize_execution_reflection(id).unwrap();
            } else {
                store.record_self_review(id, &a_review()).unwrap();
                store.finalize_execution_reflection(id).unwrap();
                record_parse();
            }
            let (_, rating, _, prompt, _) = row(&store, id);
            assert_eq!(rating, Some(8), "parse_first={parse_first}: self-review");
            assert_eq!(prompt, "ship it", "parse_first={parse_first}: derived");
            assert_eq!(
                store.reflection_parse_failure(id).unwrap(),
                Some(Some("unreadable".into())),
                "parse_first={parse_first}: the parse failure"
            );
        }
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
