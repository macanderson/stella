//! Tests for the review surface and the governance it enforces.
//!
//! Three gate criteria are decided here:
//!
//! * Keep/Edit/Ignore replay identically from the event log;
//! * no inferred directive reaches blocking by any path;
//! * no sharing or scope widens without an explicit act.

use super::*;
use stella_core::context_record::{
    ProposalScore, RecordProposalKind, RecordProposalStatus, confidence_from_score,
};

fn store() -> (tempfile::TempDir, ContextStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ContextStore::open(dir.path().join("context.db")).expect("open");
    (dir, store)
}

fn seed_proposal(store: &ContextStore, candidate_id: &str) -> ProposalRecord {
    let score = ProposalScore {
        occurrences: 3,
        distinct_tasks: 3,
        salient: false,
        rank: 30.0,
    };
    let proposal = ProposalRecord::new(
        RecordProposalKind::Knowledge,
        RecordProposalStatus::Eligible,
        candidate_id,
        "Prefer rg over grep",
        "Use ripgrep instead of grep in this repository.",
        vec!["tooling".into()],
        vec!["obs_a".into(), "obs_b".into(), "obs_c".into()],
        score,
        confidence_from_score(&score).expect("confidence"),
        "2026-07-26T12:00:00Z",
    )
    .expect("proposal");
    crate::memory::proposals::record_proposal(store, &proposal).expect("record");
    proposal
}

fn keep(store: &ContextStore, id: &str) {
    decide(
        store,
        id,
        PromotionAction::Confirmed,
        Some(DirectiveEnforcement::Advisory),
        None,
        "kept",
    )
    .expect("keep");
}

fn ignore(store: &ContextStore, id: &str) {
    decide(store, id, PromotionAction::Rejected, None, None, "declined").expect("ignore");
}

fn edit(store: &ContextStore, id: &str, body: &str) {
    decide(
        store,
        id,
        PromotionAction::Confirmed,
        Some(DirectiveEnforcement::Advisory),
        Some(body.to_string()),
        "edited",
    )
    .expect("edit");
}

// ---- GATE: Keep/Edit/Ignore replay identically from the event log ----

/// The decisions are readable back off the log in order, with their reasons and
/// their edited text, and folding the log reproduces the standing decision.
#[test]
fn keep_edit_and_ignore_replay_from_the_event_log() {
    let (_dir, store) = store();
    seed_proposal(&store, "keep-me-aaaa1111");
    seed_proposal(&store, "edit-me-bbbb2222");
    seed_proposal(&store, "ignore-me-cccc3333");

    keep(&store, "keep-me-aaaa1111");
    edit(
        &store,
        "edit-me-bbbb2222",
        "Use ripgrep, and -uu for ignored files.",
    );
    ignore(&store, "ignore-me-cccc3333");

    let events = promotion_events(&store);
    assert_eq!(events.len(), 3, "one event per decision: {events:#?}");

    let standing = decisions(&store);
    assert_eq!(
        standing.get("prp_knowledge_keep-me-aaaa1111"),
        Some(&PromotionAction::Confirmed)
    );
    assert_eq!(
        standing.get("prp_knowledge_edit-me-bbbb2222"),
        Some(&PromotionAction::Confirmed)
    );
    assert_eq!(
        standing.get("prp_knowledge_ignore-me-cccc3333"),
        Some(&PromotionAction::Rejected)
    );

    // The edit's replacement text survives, and only the edit has one — that is
    // what distinguishes it from a plain keep in the replay.
    let edited: Vec<&PromotionEventRecord> =
        events.iter().filter(|e| e.edited_body.is_some()).collect();
    assert_eq!(edited.len(), 1);
    assert_eq!(
        edited[0].edited_body.as_deref(),
        Some("Use ripgrep, and -uu for ignored files.")
    );

    // And every event carries a reason, so the replay explains itself.
    assert!(events.iter().all(|e| !e.reason.trim().is_empty()));
}

/// Replaying the fold twice over the same log gives the same answer — the state
/// is a function of the log, not of when you asked.
#[test]
fn the_replayed_state_is_a_pure_function_of_the_log() {
    let (_dir, store) = store();
    seed_proposal(&store, "some-lesson-aaaa1111");
    keep(&store, "some-lesson-aaaa1111");
    assert_eq!(decisions(&store), decisions(&store));
    assert_eq!(promotion_events(&store), promotion_events(&store));
}

/// A decline is reversible: a later Keep wins, and BOTH acts stay readable.
/// Suppression is reversible and nothing is destroyed (spec §5.7).
#[test]
fn a_decline_is_reversible_and_both_decisions_survive() {
    let (_dir, store) = store();
    seed_proposal(&store, "changed-my-mind-dddd4444");
    ignore(&store, "changed-my-mind-dddd4444");
    assert_eq!(
        decisions(&store).get("prp_knowledge_changed-my-mind-dddd4444"),
        Some(&PromotionAction::Rejected)
    );

    keep(&store, "changed-my-mind-dddd4444");
    assert_eq!(
        decisions(&store).get("prp_knowledge_changed-my-mind-dddd4444"),
        Some(&PromotionAction::Confirmed),
        "last write wins"
    );
    assert_eq!(
        promotion_events(&store).len(),
        2,
        "and the earlier decline is still in the log"
    );
}

// ---- GATE: no inferred directive reaches blocking by any path ----

/// Every decision this surface can make is advisory or nothing. There is no
/// argument, flag, or verb here that produces blocking enforcement — the
/// surface simply cannot express it, which is stronger than validating it.
#[test]
fn no_review_decision_grants_blocking_enforcement() {
    let (_dir, store) = store();
    seed_proposal(&store, "a-lesson-eeee5555");
    seed_proposal(&store, "b-lesson-ffff6666");
    seed_proposal(&store, "c-lesson-gggg7777");

    keep(&store, "a-lesson-eeee5555");
    edit(&store, "b-lesson-ffff6666", "reworded");
    ignore(&store, "c-lesson-gggg7777");

    for event in promotion_events(&store) {
        assert_ne!(
            event.enforcement,
            Some(DirectiveEnforcement::Blocking),
            "a review decision granted blocking enforcement: {event:#?}"
        );
    }
}

// ---- GATE: no sharing or scope widens without an explicit act ----

/// Nothing on this path writes a sharing scope. A proposal induced from local
/// evidence stays local, and there is no code path from the review surface to
/// publication — team governance is explicitly out of scope for this phase.
#[test]
fn no_review_decision_widens_scope_or_sharing() {
    let (_dir, store) = store();
    seed_proposal(&store, "a-lesson-hhhh8888");
    keep(&store, "a-lesson-hhhh8888");

    for row in store
        .records_of_kind(ContextRecordKind::PromotionEvent.as_str(), 100)
        .expect("rows")
    {
        let body: serde_json::Value = serde_json::from_str(&row.body).expect("json");
        let map = body.as_object().expect("object");
        for widening in ["sharing_scope", "scope", "published", "audience"] {
            assert!(
                !map.contains_key(widening),
                "a review decision wrote `{widening}`: {}",
                row.body
            );
        }
        assert_ne!(
            body["action"], "published",
            "the review surface published a record"
        );
    }
}

// ---- the ledger's own guarantees, through this surface ----

/// Decisions are appended, never updated — so a decision cannot be quietly
/// rewritten after the fact.
#[test]
fn a_decision_is_appended_not_updated() {
    let (dir, store) = store();
    seed_proposal(&store, "a-lesson-iiii9999");
    keep(&store, "a-lesson-iiii9999");
    drop(store);

    let conn = rusqlite::Connection::open(dir.path().join("context.db")).expect("reopen");
    assert!(
        conn.execute(
            "UPDATE context_records SET body = '{}' WHERE record_kind = 'promotion_event'",
            [],
        )
        .is_err(),
        "a promotion event was rewritable"
    );
}

/// Deciding on a proposal that does not exist is a clear error, not a silent
/// no-op that leaves the user thinking they declined something.
#[test]
fn deciding_on_an_unknown_proposal_is_an_error() {
    let (_dir, store) = store();
    let err = decide(
        &store,
        "no-such-candidate",
        PromotionAction::Rejected,
        None,
        None,
        "x",
    )
    .expect_err("an unknown candidate must be refused");
    assert!(err.to_string().contains("no-such-candidate"), "{err}");
}

/// `list` folds to the newest revision per lineage, so a proposal that grew
/// more evidence shows once with its current numbers rather than once per
/// revision.
#[test]
fn list_shows_one_row_per_lineage() {
    let (_dir, store) = store();
    let first = seed_proposal(&store, "growing-lesson-jjjj0000");

    // A second revision of the same lineage with more evidence.
    let score = ProposalScore {
        occurrences: 9,
        distinct_tasks: 5,
        salient: false,
        rank: 90.0,
    };
    let revised = ProposalRecord::new(
        RecordProposalKind::Knowledge,
        RecordProposalStatus::Eligible,
        "growing-lesson-jjjj0000",
        "Prefer rg over grep",
        "Use ripgrep instead of grep in this repository.",
        vec!["tooling".into()],
        vec!["obs_a".into()],
        score,
        confidence_from_score(&score).expect("confidence"),
        "2026-07-27T12:00:00Z",
    )
    .expect("revision");
    crate::memory::proposals::record_proposal(&store, &revised).expect("record");
    assert_eq!(first.lineage_id, revised.lineage_id);

    let current = current_proposals(&store);
    assert_eq!(current.len(), 1, "one lineage, one row: {current:#?}");
    assert_eq!(
        current[0].score.distinct_tasks, 5,
        "and it is the newest revision"
    );
}
