//! Which concurrently-running lanes may write this session's resume point.
//!
//! [`engine_wiring`](super::engine_wiring)'s
//! `a_bound_session_checkpoints_from_every_role` proves the sink reaches every
//! role that *should* have it. This module proves the other half, which has no
//! natural home there because it is the opposite assertion: the lanes that must
//! NOT have it.
//!
//! A [`crate::durability::SessionDurability`] handle is keyed on one session
//! record, and `Config` is `Clone` over an `Arc` — so every clone of a bound
//! `Config` yields a sink pointing at the *same* `CHECKPOINT_BLOB`. That is
//! correct for the session's own turns, which run one at a time, and wrong for
//! anything that runs *beside* them: two writers of one resume point is one
//! resume point.
//!
//! `stella_core::subagent` already draws this line for dispatched children, in
//! prose worth restating because it names the damage exactly — inheriting the
//! parent's sink is "actively destructive in both directions: every child step
//! would overwrite the parent's resume point with the CHILD's transcript, and
//! the child reaching a terminal outcome would call `discard` — retracting the
//! parent's resume point while the parent still needs it."
//!
//! A deck sub-session (`crate::subsession`) is that same shape reached by a
//! different door: a real engine session on its own OS thread, dispatched
//! *because* the lead is mid-turn, built from a clone of the lead's `Config`.
//! The engine crate cannot see it — a sub-session is not a sub-agent, it goes
//! through `Engine::with_sleeper` rather than `run_sub_agent` — so the line has
//! to be drawn here, at the seam that builds its config.

use super::*;

/// A bound session over two temp dirs, returned with the record so a test can
/// read the resume point back.
///
/// `open_in`, never `open`: the latter reads `STELLA_HOME`, and a test that
/// touched a process-global would race its siblings — the same trade
/// `durability.rs`'s own tests make for the same reason.
fn bound_session(
    session: &str,
) -> (
    Config,
    stella_store::work_journal::WorkJournal,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let store = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let cfg = cfg_for("zai");
    let record =
        stella_store::work_journal::WorkJournal::open_in(store.path(), ws.path(), session).unwrap();
    cfg.durability.bind(
        record.clone(),
        std::sync::Arc::new(stella_tools::ToolRegistry::new(
            ws.path().to_path_buf(),
            stella_tools::RegistryOptions::default(),
        )),
    );
    (cfg, record, store, ws)
}

/// **The witness.** A deck sub-session's turn must not be able to destroy the
/// lead session's resume point.
///
/// Driven at the sink seam rather than through a real turn, and that choice is
/// the honest one to name: reaching `subsession::run_worker` needs a provider, a
/// tool registry, a spawned OS thread and a current-thread runtime, none of
/// which change the answer. What decides it is which sink the sub-session's
/// `EngineConfig` carries, and `Engine::drive` — not this crate — is what turns
/// that into a `persist` per step and a `discard` at the end. So the two engine
/// calls are played by hand, in the order `drive` makes them, against the config
/// the production seam actually builds.
///
/// Before the fix, `subsession::run_worker` called `engine_config_for`, so the
/// sub-session's sink WAS the lead's: the `persist` below overwrote the lead's
/// in-flight transcript with the worker's, and the `discard` then deleted it.
/// A deck killed while its lead turn was running resumed into a conversation
/// belonging to a different agent, or into nothing at all.
#[test]
fn a_sub_sessions_turn_cannot_destroy_the_leads_resume_point() {
    let (cfg, record, _store, _ws) = bound_session("ses-lead-vs-sub");

    // The lead is mid-turn: `drive` has committed a step and written the
    // resume point that a crash from here on would be recovered from.
    let lead = crate::agent::engine_config_for(&cfg)
        .checkpoint_sink
        .expect("the lead session is bound");
    lead.persist(r#"{"version":1,"lane":"lead"}"#);

    // Concurrently — this is the whole reason sub-sessions exist — the deck
    // dispatches a prompt to a worker session, which runs a full turn and
    // ends. `drive` persists at every committed step boundary and discards on
    // every terminal path, so both halves are played.
    let sub = crate::agent::subsession_engine_config_for(&cfg).checkpoint_sink;
    if let Some(sub) = &sub {
        sub.persist(r#"{"version":1,"lane":"sub"}"#);
        sub.discard();
    }

    assert_eq!(
        record.checkpoint().as_deref(),
        Some(r#"{"version":1,"lane":"lead"}"#),
        "a sub-session's turn ending retracted the LEAD's resume point. The \
         deck dispatches a sub-session precisely because the lead is mid-turn, \
         so the two run at once against one `CHECKPOINT_BLOB` — and the loser \
         is the turn a human is waiting on. `stella-core::subagent` strips the \
         sink for exactly this reason; a sub-session reaches the same hazard \
         through `Engine::with_sleeper` instead of `run_sub_agent`, where the \
         engine crate cannot see it.",
    );
}

/// The delta the witness is measured against: the lead's own sink still works.
/// Without this, stripping the sink from *everything* would pass the test above
/// while silently ending step-level durability for the session that needs it.
#[test]
fn the_lead_session_still_checkpoints_and_still_discards() {
    let (cfg, record, _store, _ws) = bound_session("ses-lead-alone");
    let lead = crate::agent::engine_config_for(&cfg)
        .checkpoint_sink
        .expect("the lead session is bound");

    lead.persist(r#"{"version":1,"step":3}"#);
    assert_eq!(
        record.checkpoint().as_deref(),
        Some(r#"{"version":1,"step":3}"#),
    );

    // And the lead's own terminal path still retracts: a checkpoint outliving
    // its turn invites a resume that replays work the caller saw finish.
    lead.discard();
    assert!(record.checkpoint().is_none());
}

/// A sub-session carries no sink at all, rather than a differently-keyed one.
///
/// Stated separately from the witness because it is the *design* decision, not
/// the damage: a sub-session is not separately resumable today — nothing reads
/// a worker lane's resume point, and `stella resume` restores the session
/// record's own turn — so a second sink would be a write with no reader, and
/// `Engine::persist_checkpoint` serializes the whole transcript before the sink
/// ever sees it. Giving a worker lane its own durable identity is the tracked
/// follow-up, and this assertion is what will fail when someone does it, which
/// is the right place for that conversation to happen.
#[test]
fn a_sub_session_carries_no_checkpoint_sink() {
    let (cfg, _record, _store, _ws) = bound_session("ses-sub-none");
    assert!(
        crate::agent::subsession_engine_config_for(&cfg)
            .checkpoint_sink
            .is_none(),
        "a sub-session must carry no sink rather than the lead's",
    );
    // Everything else about the config is the session's own tuning — the strip
    // is one field, not a separate engine shape.
    let lead = crate::agent::engine_config_for(&cfg);
    let sub = crate::agent::subsession_engine_config_for(&cfg);
    assert_eq!(sub.max_steps, lead.max_steps);
    assert_eq!(sub.compaction_budget_tokens, lead.compaction_budget_tokens);
    assert_eq!(sub.cwd, lead.cwd);
}

/// The source fence. `subsession::run_worker` must build its engine through
/// [`crate::agent::subsession_engine_config_for`] and never through
/// `engine_config_for`.
///
/// A lexical guard for the reason `resume_frame.rs`'s
/// `every_pipeline_construction_declares_its_resume_frame` is one: what regresses
/// here is *wiring*, and the regression is invisible at runtime. An inherited
/// sink and a correct one differ only in which session's blob a write lands in,
/// which nothing observes until a process dies mid-turn — so the assertions
/// above pin the behaviour while this pins the call site that reaches it.
#[test]
fn the_sub_session_worker_builds_its_engine_through_the_isolating_seam() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/subsession.rs");
    let body = std::fs::read_to_string(&src)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", src.display()));
    // Built rather than written out, so the fence does not match itself.
    let inherited = format!("agent::engine_config{}(", "_for");
    assert!(
        !body.contains(&inherited),
        "subsession.rs builds its engine through `engine_config_for`, which \
         attaches the LEAD session's checkpoint sink. A sub-session runs \
         concurrently with the lead turn, so its steps overwrite — and its \
         terminal path deletes — the resume point the lead is relying on. Use \
         `agent::subsession_engine_config_for` instead.",
    );
    assert!(
        body.contains("agent::subsession_engine_config_for("),
        "subsession.rs no longer builds its engine through the isolating seam",
    );
}
