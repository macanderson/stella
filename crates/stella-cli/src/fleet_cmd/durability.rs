// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A fleet attempt's own durable record: where its turns checkpoint, and how a
//! re-dispatch of the same task finds them again (#3232).
//!
//! Every other door binds `crate::durability::SessionDurability` before its
//! first turn — `SessionPresence::announce` for `stella run` and the plain
//! shell, the deck at its session switch, `stella resume`, and a deck lane at
//! its own key. The fan-out bound nothing, so `sink()` answered `None` for
//! every worker turn and a worker killed at step 40 of 50 lost all 40 steps of
//! transcript. The tree kept whatever it wrote; the conversation that produced
//! it did not exist anywhere.
//!
//! ## The handle is the attempt's, never the parent's
//!
//! `Config` clones over one `Arc` cell, so binding the `cfg.durability` a
//! worker inherits would make every worker in the wave a writer of one resume
//! point — each one overwriting the others' transcripts and `discard`ing the
//! survivors' point on the way out. That is the exact hazard
//! `crate::durability`'s "What must NOT inherit a binding" section names, and
//! it is why [`bind`] mints a fresh [`SessionDurability`] rather than binding
//! the one on the `Config`.
//!
//! ## Why the key is the claim holder
//!
//! `{run_id}/{task_id}` is already the identity the fleet coordinates a task's
//! file claims under (`fleet_cmd.rs`'s `claim_holder`), and it is unique
//! exactly where a resume point has to be: two workers of one wave differ, two
//! waves over one workspace differ, and a re-dispatch of a settled task lands
//! back on its own. Deriving a second identity for the same attempt would be
//! two answers to one question.
//!
//! It is not usable as a key verbatim. `WorkJournal::open` needs one that is
//! filesystem-safe and ref-safe at once, and `/` is the character that is one
//! but not the other: `index_file_path` builds `{workspace_id}.{session}
//! .index`, where an embedded `/` reads as a path separator into a directory
//! nothing creates — the write then fails and
//! `JournalCheckpointSink::persist`'s best-effort contract swallows it, so the
//! whole feature would be inert with no visible symptom short of a kill
//! actually losing a transcript. `crate::subsession::lane_journal_key` hit the
//! same wall over a lane id it mints itself; a task id comes out of a plan
//! file and can hold anything, so [`attempt_journal_key`] sanitizes and caps a
//! readable stem and appends a stable hash of the whole holder — the hash
//! being what stops two ids that flatten to one stem from becoming one resume
//! point.
//!
//! ## What this does and does not recover
//!
//! The reader is the re-dispatch. `Fleet::dispatch` is re-runnable once an
//! attempt settles (`crates/stella-fleet/src/fleet.rs`, "Re-dispatch a task
//! after its previous attempt settles"), so a task whose worker was stopped,
//! timed out or panicked re-enters its own transcript instead of starting over
//! against a tree that already holds its partial work.
//!
//! It does **not** recover a killed *process*: a fresh `stella fleet`
//! invocation mints a new `run_id`, so its keys are new and it starts clean.
//! Making a run resumable across invocations means giving a run a stable
//! identity a later process can name, which is a fleet-ledger question and not
//! this module's (#4802) — the alternative, dropping `run_id` from the key, would let
//! two concurrent runs over one workspace write each other's resume points,
//! which is the hazard above wearing a different hat.

use std::path::Path;

use stella_core::EngineConfig;
use stella_protocol::CompletionMessage;

use crate::agent;
use crate::config::Config;
use crate::durability::SessionDurability;

/// One fleet attempt's durable record, plus whatever went wrong opening it.
pub(super) struct AttemptDurability {
    handle: SessionDurability,
    /// The operator-facing note when the record would not open. Carried rather
    /// than printed: a worker's diagnostics belong on its own event lane, and
    /// this module is called before that lane exists.
    warning: Option<String>,
}

/// How much of the claim holder survives into a readable stem. One path
/// component maxes out at 255 bytes on APFS and ext4, and the record's index
/// file is `{workspace_id}.{session}.index` — so a plan free-texting its task
/// ids (a whole prompt as the id, which `stella_fleet::git`'s own slug cap
/// exists for) must not be able to make that name unopenable.
const MAX_KEY_STEM: usize = 64;

/// The work-journal key an attempt binds under: a readable stem of the claim
/// holder plus a stable short hash of the whole thing.
///
/// The stem is sanitized rather than escaped because `WorkJournal::open`'s key
/// has to be safe on two axes at once — a filesystem name and a git ref
/// component — and a task id comes out of a plan file, so it can hold
/// anything. The hash is what keeps that lossy: two ids that sanitize or
/// truncate to the same stem are two attempts, and two attempts sharing a
/// record is one resume point rather than two, which is the whole failure this
/// module exists to prevent. FNV-1a written out rather than `DefaultHasher`,
/// because a resume point has to be findable by a later process and
/// `DefaultHasher`'s output is not promised to be stable across builds.
///
/// The same shape as `stella_fleet::git::worktree_slug`, which caps and hashes
/// for these two reasons over the same two inputs; its helpers are private to
/// that crate.
fn attempt_journal_key(claim_holder: &str) -> String {
    let stem: String = claim_holder
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .take(MAX_KEY_STEM)
        .collect();
    // A git ref component may not end in `-` or `.`, and a truncation can land
    // on either.
    let stem = stem
        .trim_start_matches(['-', '.'])
        .trim_end_matches(['-', '.']);
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in claim_holder.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{stem}-{hash:016x}")
}

/// Open this attempt's durable record under `claim_holder`.
///
/// Never fails: a record that will not open leaves the handle unbound, which
/// is exactly as recoverable as every fleet attempt was before this existed.
/// Refusing to start the attempt over it would trade a working worker for
/// none — the same contract `crate::durability::bind_session` states.
pub(super) fn bind(workspace_root: &Path, claim_holder: &str) -> AttemptDurability {
    let handle = SessionDurability::default();
    let warning = crate::durability::bind_session(
        &handle,
        workspace_root,
        &attempt_journal_key(claim_holder),
    );
    AttemptDurability { handle, warning }
}

impl AttemptDurability {
    /// Put this attempt's durability facts on its own event lane: a record
    /// that would not open, and a transcript re-entered from one that did.
    ///
    /// The lane rather than stderr, for the reason the withheld-claim notice
    /// beside it takes the lane: N workers share one stderr and each has its
    /// own journal, so a note printed there cannot be attributed to the
    /// attempt it is about.
    pub(super) fn announce(
        &self,
        tx: &tokio::sync::mpsc::UnboundedSender<stella_protocol::AgentEvent>,
        resume_note: Option<String>,
    ) {
        for text in self
            .warning
            .as_deref()
            .map(|warning| format!("note: {warning}"))
            .into_iter()
            .chain(resume_note.map(|note| format!("↻ {note}")))
        {
            let _ = tx.send(stella_protocol::AgentEvent::Text { text });
        }
    }

    /// This attempt's engine config: `agent::engine_config_for`, re-keyed onto
    /// this attempt's own checkpoint sink rather than the parent's.
    pub(super) fn engine_config(&self, cfg: &Config) -> EngineConfig {
        agent::subsession_engine_config_for(cfg, &self.handle)
    }

    /// This attempt's opening transcript: restored from a prior interrupted
    /// attempt on this exact key, or built fresh from `prompt`. The second
    /// element is a note for the attempt's own transcript when a restore
    /// happened.
    ///
    /// Turn-boundary fidelity, matching what a deck lane already does
    /// (`crate::subsession`): the system prompt is regenerated because rules
    /// and config may have moved since the checkpoint was written, the
    /// transcript is not. A checkpoint this build cannot parse degrades to a
    /// fresh start rather than to a refused attempt.
    pub(super) fn initial_messages(
        &self,
        system_prompt: String,
        prompt: &str,
    ) -> (Vec<CompletionMessage>, Option<String>) {
        let restored = self
            .handle
            .checkpoint()
            .and_then(|json| stella_core::step::Checkpoint::from_json(&json).ok());
        match restored {
            Some(checkpoint) => {
                let step = checkpoint.step;
                (
                    crate::session_persist::restore_messages(checkpoint.messages, &system_prompt),
                    Some(format!(
                        "resuming an earlier attempt at this task, interrupted at step {step} — \
                         its completed steps are in the transcript and will not be re-run"
                    )),
                )
            }
            None => (
                vec![
                    CompletionMessage::system(system_prompt),
                    CompletionMessage::user(prompt),
                ],
                None,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use stella_store::work_journal::WorkJournal;

    /// A `Config` and an attempt handle bound to their own records, over temp
    /// directories.
    ///
    /// `WorkJournal::open_in`, never `open`: the latter resolves the store from
    /// `STELLA_HOME`, and a test that touched a process-global would race its
    /// siblings — the trade `agent/tests/durability_isolation.rs` makes for the
    /// same reason. It is the one line of [`bind`] a test cannot execute; the
    /// key it would be handed is covered by the cases above, and the call
    /// itself is the one four other doors already make.
    fn bound_attempt(
        session: &str,
    ) -> (
        AttemptDurability,
        WorkJournal,
        tempfile::TempDir,
        tempfile::TempDir,
    ) {
        let store = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let record = WorkJournal::open_in(store.path(), workspace.path(), session).unwrap();
        let handle = SessionDurability::default();
        handle.bind(record.clone());
        // Both directories come back: the store holds the git repository the
        // resume point is written into, so dropping it here would delete the
        // record out from under every assertion below.
        (
            AttemptDurability {
                handle,
                warning: None,
            },
            record,
            store,
            workspace,
        )
    }

    fn cfg() -> Config {
        let provider = crate::config::PROVIDERS
            .iter()
            .find(|provider| provider.id == "zai")
            .expect("zai is a seeded provider")
            .clone();
        let model_id = provider.default_model.to_string();
        Config::for_tests(provider, model_id)
    }

    /// **The witness.** A fleet attempt's engine carries a checkpoint sink at
    /// all, and it is the attempt's own.
    ///
    /// Driven at the sink seam rather than through a real worker, for the
    /// reason `durability_isolation.rs` gives: reaching `run_task` needs a
    /// provider, a registry, a git tree and a fleet ledger, none of which
    /// change the answer. What decides it is which sink the attempt's
    /// `EngineConfig` carries, and `Engine::drive` — not this crate — turns
    /// that into a `persist` per step.
    ///
    /// Both arms, because the first is what names the defect: on the old
    /// wiring the fleet built its config with `agent::engine_config_for` over a
    /// `Config` nothing ever bound, so the sink was `None` and every worker
    /// step serialized nothing into nowhere.
    #[test]
    fn a_fleet_attempt_checkpoints_into_its_own_record() {
        // ── Arm 1: the old wiring. An unbound `Config` yields no sink. ──
        assert!(
            agent::engine_config_for(&cfg()).checkpoint_sink.is_none(),
            "a fleet worker's `Config` is never bound, so the config it used to \
             build carried no sink — if that has changed, the arm below proves \
             nothing about this fix"
        );

        // ── Arm 2: the seam. ────────────────────────────────────────────
        let (attempt, record, _store, _workspace) = bound_attempt("run-17-task-a");
        let sink = attempt
            .engine_config(&cfg())
            .checkpoint_sink
            .expect("a bound attempt checkpoints");
        sink.persist(r#"{"version":1,"attempt":"task-a"}"#);

        assert_eq!(
            record.checkpoint().as_deref(),
            Some(r#"{"version":1,"attempt":"task-a"}"#),
            "a killed attempt must leave a readable resume point"
        );
    }

    /// A resume point a prior attempt at this task left behind, as the engine
    /// would have written it.
    fn checkpoint_of(said: &str) -> String {
        stella_core::step::Checkpoint {
            version: stella_core::step::CHECKPOINT_VERSION,
            step: 7,
            messages: vec![
                CompletionMessage::system("the prompt the interrupted attempt ran under"),
                CompletionMessage::user("do the thing"),
                CompletionMessage::assistant(said),
            ],
            budget: stella_core::step::BudgetSnapshot {
                mode: stella_protocol::BudgetMode::Observed,
                turn_limit_usd: None,
                session_limit_usd: None,
                turn_spent_usd: 0.5,
                session_spent_usd: 0.5,
            },
            total_cost_usd: 0.5,
            calibration_model: None,
            loop_steered: false,
            loop_steered_pattern: vec![],
            loop_steered_inputs: None,
            transcript_rewrites: 0,
            loop_steers_spent: 0,
        }
        .to_json()
        .expect("a checkpoint serializes")
    }

    /// **The reader.** Binding the write side alone would serialize a whole
    /// transcript per step with nothing to read it — which is the trade #3232
    /// says must not be made. This is what reads it: `Fleet::dispatch` is
    /// re-runnable once an attempt settles, and a re-dispatch lands on the same
    /// key, so it re-enters the interrupted transcript instead of starting over
    /// against a tree that already holds its partial work.
    #[test]
    fn a_redispatched_attempt_re_enters_its_interrupted_transcript() {
        let (attempt, record, _store, _workspace) = bound_attempt("run-17-task-a");
        record
            .record_checkpoint(&checkpoint_of("half the work"), None)
            .expect("the record accepts a checkpoint");

        let (messages, note) =
            attempt.initial_messages("a freshly built prompt".to_string(), "do the thing");

        assert_eq!(
            messages.len(),
            3,
            "the interrupted transcript is re-entered, not replaced by the prompt"
        );
        assert_eq!(
            messages[0].content, "a freshly built prompt",
            "the system prompt is regenerated — rules and config may have moved \
             since the checkpoint was written"
        );
        assert_eq!(messages[2].content, "half the work");
        assert!(
            note.expect("a restore is announced").contains("step 7"),
            "the attempt's journal says where it resumed from"
        );
    }

    /// A checkpoint this build cannot read degrades to a fresh attempt rather
    /// than to a refused one — the same "not preferred" rule every other
    /// restore path here follows.
    #[test]
    fn an_unreadable_checkpoint_degrades_to_a_fresh_attempt() {
        let (attempt, record, _store, _workspace) = bound_attempt("run-17-task-c");
        record
            .record_checkpoint("{\"version\":999,\"from\":\"a newer stella\"}", None)
            .expect("the record accepts the bytes; reading them back is what fails");

        let (messages, note) = attempt.initial_messages("sys".to_string(), "do the thing");

        assert_eq!(messages.len(), 2);
        assert!(note.is_none());
    }

    /// Two workers of one wave in a **shared** tree write different resume
    /// points — the property that makes binding safe at all, and the one an
    /// inherited handle would destroy.
    #[test]
    fn two_concurrent_attempts_write_different_records() {
        let (first, first_record, _first_store, _first_ws) = bound_attempt("run-17-task-a");
        let (second, second_record, _second_store, _second_ws) = bound_attempt("run-17-task-b");

        first
            .engine_config(&cfg())
            .checkpoint_sink
            .expect("bound")
            .persist(r#"{"version":1,"attempt":"task-a"}"#);
        second
            .engine_config(&cfg())
            .checkpoint_sink
            .expect("bound")
            .persist(r#"{"version":1,"attempt":"task-b"}"#);

        assert_eq!(
            first_record.checkpoint().as_deref(),
            Some(r#"{"version":1,"attempt":"task-a"}"#)
        );
        assert_eq!(
            second_record.checkpoint().as_deref(),
            Some(r#"{"version":1,"attempt":"task-b"}"#)
        );
    }

    /// Both bytes the journal key cannot carry are in every claim holder the
    /// fleet mints: `/` between the run and the task, and `:` inside a task id
    /// that names an issue. An unsanitized key fails `WorkJournal::open` on
    /// the filesystem half and the sink swallows the error, so the whole
    /// feature would be silently inert.
    #[test]
    fn the_journal_key_carries_no_byte_the_record_refuses() {
        // A task id straight out of a plan file: a colon, a space, a slash, and
        // a trailing dot a git ref may not end on.
        let key = attempt_journal_key("run-17/issue:4403 fix the thing.");
        assert!(
            key.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')),
            "{key} must be safe as a filesystem name and as a ref component"
        );
        assert!(!key.ends_with(['-', '.']), "{key} may not end a ref");
        assert!(
            key.starts_with("run-17-issue-4403-fix"),
            "{key} stays readable"
        );
    }

    /// The cap is what stops a plan whose task id is a whole prompt from
    /// building a name the filesystem refuses; the hash is what stops the cap
    /// from turning two attempts into one.
    #[test]
    fn a_free_text_task_id_is_capped_without_becoming_ambiguous() {
        let long = "x".repeat(4000);
        let first = attempt_journal_key(&format!("run-17/{long}a"));
        let second = attempt_journal_key(&format!("run-17/{long}b"));
        assert!(first.len() <= MAX_KEY_STEM + 17);
        assert_ne!(
            first, second,
            "two task ids that truncate to one stem are still two attempts"
        );
    }

    /// Two workers of one wave, and two waves over one workspace, must land on
    /// different records — a shared key is a second writer of one resume
    /// point, not a second resume point.
    #[test]
    fn two_attempts_never_share_a_record() {
        let sibling_task = attempt_journal_key("run-17/task-a");
        let other_task = attempt_journal_key("run-17/task-b");
        let other_run = attempt_journal_key("run-18/task-a");
        assert_ne!(sibling_task, other_task);
        assert_ne!(sibling_task, other_run);
    }

    /// A re-dispatch of a settled task is the reader this write side exists
    /// for, so the same attempt must derive the same key twice.
    #[test]
    fn a_redispatch_of_one_task_lands_on_its_own_record() {
        assert_eq!(
            attempt_journal_key("run-17/task-a"),
            attempt_journal_key("run-17/task-a")
        );
    }

    /// With nothing checkpointed, an attempt opens on its prompt exactly as it
    /// did before this module existed.
    #[test]
    fn an_unbound_attempt_starts_fresh() {
        let attempt = AttemptDurability {
            handle: SessionDurability::default(),
            warning: None,
        };
        let (messages, note) = attempt.initial_messages("sys".to_string(), "do the thing");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].content, "do the thing");
        assert!(note.is_none());
    }
}
