//! Phase 4 deliverable 5: retirement is derived, reversible, reasoned, and
//! provably never touches a protected record.

use stella_context::ContextStore;
use stella_core::context_record::{
    DirectiveEnforcement, PromotionAction, PromotionActor, PromotionEventRecord, SelectionHealth,
};

use super::*;

const AT: &str = "2026-07-26T00:00:00Z";

fn store() -> (tempfile::TempDir, ContextStore) {
    let dir = tempfile::tempdir().expect("workspace");
    let store = ContextStore::open(dir.path().join("context.db")).expect("context.db");
    (dir, store)
}

/// A record whose evidence says it is failing.
fn failing(record_id: &str) -> SelectionHealth {
    SelectionHealth {
        context_record_id: record_id.into(),
        uses: 10,
        distinct_tasks: 4,
        assessed_uses: 5,
        helpful: 0,
        not_helpful: 5,
        neutral: 0,
        not_helpful_ratio: 1.0,
        eligible_assessed: 5,
        eligible_not_helpful: 5,
        attributable: true,
        failing: true,
    }
}

/// A record that is doing fine.
fn healthy(record_id: &str) -> SelectionHealth {
    SelectionHealth {
        context_record_id: record_id.into(),
        uses: 10,
        distinct_tasks: 4,
        assessed_uses: 5,
        helpful: 5,
        not_helpful: 0,
        neutral: 0,
        not_helpful_ratio: 0.0,
        eligible_assessed: 5,
        eligible_not_helpful: 0,
        attributable: true,
        failing: false,
    }
}

/// Put a standing on the log so the protection check has something to read.
fn given_standing(
    store: &ContextStore,
    record_id: &str,
    action: PromotionAction,
    actor: PromotionActor,
    enforcement: Option<DirectiveEnforcement>,
) {
    let event = PromotionEventRecord::new(
        record_id,
        action,
        actor,
        enforcement,
        None,
        "test standing",
        AT,
    )
    .expect("event");
    let body = serde_json::to_string(&event).expect("body");
    store
        .append_record(stella_context::LedgerAppend {
            record_id: &event.record_id,
            lineage_id: &event.lineage_id,
            record_kind: stella_core::context_record::ContextRecordKind::PromotionEvent.as_str(),
            record_hash: &event.record_hash,
            schema_version: stella_core::context_record::LIFECYCLE_SCHEMA_VERSION,
            body: &body,
            observed_at: &event.occurred_at,
            supersedes: None,
        })
        .expect("append");
}

#[test]
fn a_failing_record_is_retired_with_a_legible_reason() {
    let (_dir, store) = store();
    let sweep = sweep(&store, &[failing("nod_a")], AT);

    assert_eq!(sweep.retired, vec!["nod_a".to_string()]);
    assert!(retired_ids(&store).contains("nod_a"));

    // The gate: retirement decisions carry a human-readable reason.
    let standing = standings(&store);
    let reason = &standing.get("nod_a").expect("standing").reason;
    assert!(!reason.trim().is_empty());
    assert!(
        reason.contains("not helpful in 5 of 5"),
        "the reason must say what the evidence was, got: {reason}"
    );
}

#[test]
fn a_healthy_record_is_never_retired() {
    let (_dir, store) = store();
    let sweep = sweep(&store, &[healthy("nod_a")], AT);

    assert!(sweep.retired.is_empty());
    assert!(retired_ids(&store).is_empty());
}

#[test]
fn a_record_nobody_assessed_is_never_retired() {
    let (_dir, store) = store();
    // Rendered constantly, judged never. Disuse is not a negative.
    let silent = SelectionHealth {
        context_record_id: "nod_a".into(),
        uses: 500,
        distinct_tasks: 40,
        assessed_uses: 0,
        helpful: 0,
        not_helpful: 0,
        neutral: 0,
        not_helpful_ratio: 0.0,
        eligible_assessed: 0,
        eligible_not_helpful: 0,
        attributable: false,
        failing: false,
    };

    assert!(sweep(&store, &[silent], AT).retired.is_empty());
}

#[test]
fn a_user_confirmed_record_is_provably_never_retired() {
    let (_dir, store) = store();
    given_standing(
        &store,
        "nod_a",
        PromotionAction::Confirmed,
        PromotionActor::User,
        None,
    );

    let sweep = sweep(&store, &[failing("nod_a")], AT);
    assert!(sweep.retired.is_empty());
    assert_eq!(
        sweep.refused,
        vec![("nod_a".to_string(), RetirementProtection::UserConfirmed)]
    );
    assert!(!retired_ids(&store).contains("nod_a"));
}

#[test]
fn a_published_record_is_provably_never_retired() {
    let (_dir, store) = store();
    given_standing(
        &store,
        "nod_a",
        PromotionAction::Published,
        PromotionActor::User,
        None,
    );

    let sweep = sweep(&store, &[failing("nod_a")], AT);
    assert!(sweep.retired.is_empty());
    assert_eq!(sweep.refused[0].1, RetirementProtection::Published);
}

#[test]
fn a_blocking_record_is_provably_never_retired() {
    let (_dir, store) = store();
    // Blocking requires a user actor by construction — the constructor refuses
    // a system-granted one, which is itself the §5.4 guarantee.
    given_standing(
        &store,
        "nod_a",
        PromotionAction::Confirmed,
        PromotionActor::User,
        Some(DirectiveEnforcement::Blocking),
    );

    let sweep = sweep(&store, &[failing("nod_a")], AT);
    assert!(sweep.retired.is_empty());
    assert_eq!(sweep.refused[0].1, RetirementProtection::Blocking);
}

#[test]
fn a_system_auto_activation_is_not_a_confirmation_and_stays_retirable() {
    let (_dir, store) = store();
    // The protection is the user's ACT, not merely the presence of a standing.
    // A record the system activated on its own has no human judgement behind
    // it, so the loop may retire what the loop promoted.
    given_standing(
        &store,
        "nod_a",
        PromotionAction::AutoActivated,
        PromotionActor::System,
        None,
    );

    let sweep = sweep(&store, &[failing("nod_a")], AT);
    assert_eq!(sweep.retired, vec!["nod_a".to_string()]);
}

#[test]
fn retirement_is_reversible_by_reaffirming() {
    let (_dir, store) = store();
    sweep(&store, &[failing("nod_a")], AT);
    assert!(retired_ids(&store).contains("nod_a"));

    assert!(reaffirm(&store, "nod_a", "still needed", AT));
    assert!(
        !retired_ids(&store).contains("nod_a"),
        "a reaffirmed record must return to automatic selection"
    );
}

#[test]
fn reaffirming_outranks_without_erasing() {
    let (_dir, store) = store();
    sweep(&store, &[failing("nod_a")], AT);
    reaffirm(&store, "nod_a", "still needed", AT);

    // Both acts remain readable — the ledger is append-only and nothing was
    // deleted. The standing is the latest, not the only.
    let events = store
        .records_of_kind_in_append_order(
            stella_core::context_record::ContextRecordKind::PromotionEvent.as_str(),
            100,
        )
        .expect("read");
    assert_eq!(events.len(), 2, "the retirement must still be on the log");
    assert_eq!(
        standings(&store).get("nod_a").expect("standing").action,
        PromotionAction::Reverted
    );
}

#[test]
fn a_reaffirmed_record_can_be_retired_again_if_it_keeps_failing() {
    let (_dir, store) = store();
    sweep(&store, &[failing("nod_a")], AT);
    reaffirm(&store, "nod_a", "give it another chance", AT);

    // Reaffirmation is not permanent immunity — the loop stays live. Use a
    // distinct timestamp so the second retirement is a different record.
    let again = sweep(&store, &[failing("nod_a")], "2026-07-27T00:00:00Z");
    assert_eq!(again.retired, vec!["nod_a".to_string()]);
    assert!(retired_ids(&store).contains("nod_a"));
}

#[test]
fn retiring_an_already_retired_record_is_refused_not_duplicated() {
    let (_dir, store) = store();
    sweep(&store, &[failing("nod_a")], AT);

    let again = sweep(&store, &[failing("nod_a")], "2026-07-27T00:00:00Z");
    assert!(again.retired.is_empty());
    assert_eq!(again.refused[0].1, RetirementProtection::AlreadyRetired);
}

#[test]
fn a_refused_retirement_is_reported_rather_than_silently_skipped() {
    let (_dir, store) = store();
    given_standing(
        &store,
        "nod_protected",
        PromotionAction::Confirmed,
        PromotionActor::User,
        None,
    );

    let sweep = sweep(
        &store,
        &[failing("nod_protected"), failing("nod_ordinary")],
        AT,
    );
    assert_eq!(sweep.retired, vec!["nod_ordinary".to_string()]);
    assert_eq!(
        sweep.refused.len(),
        1,
        "the sweep must say what it declined"
    );
}

#[test]
fn retirement_never_deletes_the_record() {
    let (_dir, store) = store();
    sweep(&store, &[failing("nod_a")], AT);

    // Retirement writes one event and touches nothing else. The ledger is
    // append-only at the database, so this is belt and braces — but "never
    // physically deletes" is a gate criterion and deserves its own assertion.
    let events = store
        .records_of_kind_in_append_order(
            stella_core::context_record::ContextRecordKind::PromotionEvent.as_str(),
            100,
        )
        .expect("read");
    assert_eq!(events.len(), 1);
}

#[test]
fn a_retirement_reason_is_only_produced_for_a_failing_record() {
    assert!(retirement_reason(&healthy("nod_a")).is_none());
    assert!(retirement_reason(&failing("nod_a")).is_some());
}
