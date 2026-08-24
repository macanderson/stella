//! Which durable record each concurrently-running lane writes its resume
//! point to.
//!
//! [`engine_wiring`](super::engine_wiring)'s
//! `a_bound_session_checkpoints_from_every_role` proves the sink reaches every
//! role that *should* carry the SESSION's own sink. This module proves the
//! other half: a lane that runs *beside* the lead turn must never share it —
//! it must write its own.
//!
//! A [`crate::durability::SessionDurability`] handle is keyed on one session
//! record, and `Config` is `Clone` over an `Arc` — so every clone of a bound
//! `Config` yields a sink pointing at the *same* `CHECKPOINT_BLOB`. That is
//! correct for the session's own turns, which run one at a time, and wrong for
//! anything that runs *beside* them: two writers of one resume point is one
//! resume point, not two.
//!
//! `stella_core::subagent` draws this line for dispatched children by
//! stripping the sink entirely, in prose worth restating because it names the
//! damage exactly — inheriting the parent's sink is "actively destructive in
//! both directions: every child step would overwrite the parent's resume point
//! with the CHILD's transcript, and the child reaching a terminal outcome
//! would call `discard` — retracting the parent's resume point while the
//! parent still needs it." A dispatched child has no durable identity of its
//! own to re-key the sink to, so stripping is the whole fix there.
//!
//! A deck sub-session (`crate::subsession`) is that same shape reached by a
//! different door — a real engine session on its own OS thread, dispatched
//! *because* the lead is mid-turn, built from a clone of the lead's `Config` —
//! but it DOES have an identity of its own: its lane id. So `run_worker`
//! re-keys the sink to the lane's own handle instead of stripping it (#3233),
//! and the engine crate still cannot see any of this — a sub-session goes
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
    cfg.durability.bind(record.clone());
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
/// Both arms are asserted, and that is the point. The first reproduces what
/// the old wiring did — `subsession::run_worker` called `engine_config_for`, so
/// the worker's sink WAS the lead's — and the second shows the seam closing it.
/// Without the first arm this test would pass on a build where the damage is
/// merely unreachable rather than prevented, and could not say what the fence
/// below is protecting.
#[test]
fn a_sub_sessions_turn_cannot_destroy_the_leads_resume_point() {
    // ── Arm 1: the old wiring, played out. ──────────────────────────────
    let (cfg, record, _store, _ws) = bound_session("ses-inherited-sink");
    let lead = crate::agent::engine_config_for(&cfg)
        .checkpoint_sink
        .expect("the lead session is bound");
    lead.persist(r#"{"version":1,"lane":"lead"}"#);

    // What the sub-session used to be handed: the lead's own sink, because
    // `Config` is `Clone` over one `Arc` cell. `drive` persists at every
    // committed step boundary and discards on every terminal path, so a whole
    // worker turn is those two calls.
    let inherited = crate::agent::engine_config_for(&cfg)
        .checkpoint_sink
        .expect("bound");
    inherited.persist(r#"{"version":1,"lane":"sub"}"#);
    inherited.discard();

    assert!(
        record.checkpoint().is_none(),
        "the damage this test exists to prevent did not reproduce — an \
         inherited sink no longer shares the lead's CHECKPOINT_BLOB, so the \
         second arm below proves nothing. Re-derive what a sub-session is \
         actually handed before trusting this file.",
    );

    // ── Arm 2: the seam. ────────────────────────────────────────────────
    let (lead_cfg, lead_record, _store, _ws) = bound_session("ses-isolated-sink");
    let lead = crate::agent::engine_config_for(&lead_cfg)
        .checkpoint_sink
        .expect("the lead session is bound");
    lead.persist(r#"{"version":1,"lane":"lead"}"#);

    // The lane's own handle, bound to its own journal — never the lead's
    // `cfg.durability` cell. `ses-isolated-sink__req-1`, not `.../req:1`:
    // `lane_journal_key`'s actual sanitized shape (`__`, and no `:`) — both
    // production `WorkJournal::open`'s "filesystem- and ref-safe" contract
    // forbids.
    let (lane_cfg, lane_record, _lane_store, _lane_ws) = bound_session("ses-isolated-sink__req-1");
    let sub = crate::agent::subsession_engine_config_for(&lead_cfg, &lane_cfg.durability)
        .checkpoint_sink
        .expect("the lane is bound");
    sub.persist(r#"{"version":1,"lane":"sub"}"#);
    sub.discard();

    assert_eq!(
        lead_record.checkpoint().as_deref(),
        Some(r#"{"version":1,"lane":"lead"}"#),
        "a sub-session's turn ending retracted the LEAD's resume point. The \
         deck dispatches a sub-session precisely because the lead is mid-turn, \
         so the two must never share one `CHECKPOINT_BLOB` — and the loser \
         would be the turn a human is waiting on. `stella-core::subagent` \
         strips the sink for a dispatched child, which has no identity of its \
         own to re-key to; a sub-session reaches the same door through \
         `Engine::with_sleeper` instead of `run_sub_agent`, but DOES have an \
         identity — its lane id — so it must re-key rather than share.",
    );
    assert!(
        lane_record.checkpoint().is_none(),
        "the lane's own checkpoint must discard at its own turn's end, \
         exactly like the lead's",
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

/// A sub-session carries the LANE's own sink, never the lead's — and never
/// none, now that a lane has a durable identity of its own to re-key to.
///
/// Stated separately from the witness above because it is the *design*
/// decision, not the damage: `crate::subsession::run_worker` binds a
/// [`crate::durability::SessionDurability`] under the lane's own journal key
/// (`{session}/{lane}`) before building its engine, so the lane is
/// independently resumable (#3233) — this is what will fail if a future
/// change goes back to stripping the sink instead.
#[test]
fn a_sub_session_carries_its_own_checkpoint_sink_never_the_leads() {
    let (lead_cfg, _lead_record, _store, _ws) = bound_session("ses-sub-own-sink");
    let (lane_cfg, lane_record, _lane_store, _lane_ws) = bound_session("ses-sub-own-sink__req-1");

    let sub = crate::agent::subsession_engine_config_for(&lead_cfg, &lane_cfg.durability)
        .checkpoint_sink
        .expect("a sub-session must carry the LANE's sink, not none");
    sub.persist(r#"{"version":1}"#);
    assert_eq!(
        lane_record.checkpoint().as_deref(),
        Some(r#"{"version":1}"#),
        "a sub-session's checkpoint must land in the lane's own record",
    );

    // Everything else about the config is the session's own tuning — the
    // re-key is one field, not a separate engine shape.
    let lead = crate::agent::engine_config_for(&lead_cfg);
    let sub_cfg = crate::agent::subsession_engine_config_for(&lead_cfg, &lane_cfg.durability);
    assert_eq!(sub_cfg.max_steps, lead.max_steps);
    assert_eq!(
        sub_cfg.compaction_budget_tokens,
        lead.compaction_budget_tokens
    );
    assert_eq!(sub_cfg.cwd, lead.cwd);
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
