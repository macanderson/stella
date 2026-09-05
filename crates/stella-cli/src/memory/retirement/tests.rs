//! Phase 4 deliverable 5: retirement is derived, reversible, reasoned, and
//! provably never touches a protected record.

use stella_context::ContextStore;
use stella_learn::skills::appraisal::{AppraisalConfig, SkillTrial, appraise, decide_demotion};
use stella_records::context_record::{
    DirectiveEnforcement, PromotionAction, PromotionActor, PromotionEventRecord,
};

use super::*;

const AT: &str = "2026-07-26T00:00:00Z";

/// Turns per arm. Above the five-per-arm evidence floor, and the same count
/// `stella-learn`'s own `Harms` fixture uses.
const TURNS_PER_ARM: usize = 8;

fn store() -> (tempfile::TempDir, ContextStore) {
    let dir = tempfile::tempdir().expect("workspace");
    let store = ContextStore::open(dir.path().join("context.db")).expect("context.db");
    (dir, store)
}

/// One recorded turn: whether the record was shown, and how the turn ended.
fn turn(shown: bool, succeeded: bool) -> SkillTrial {
    SkillTrial {
        selected: shown,
        ..super::super::trials::live_trial(succeeded)
    }
}

/// A window whose turns say withholding the record won.
fn harming_window() -> Vec<SkillTrial> {
    (0..TURNS_PER_ARM)
        .flat_map(|_| [turn(true, false), turn(false, true)])
        .collect()
}

/// A window whose turns say showing it won.
fn helping_window() -> Vec<SkillTrial> {
    (0..TURNS_PER_ARM)
        .flat_map(|_| [turn(true, true), turn(false, false)])
        .collect()
}

/// What the shared sweep hands [`sweep`]: the appraisal of a window and the
/// keep-or-demote decision the record's origin allows.
fn decided(
    record_id: &str,
    origin: SkillOrigin,
    trials: Vec<SkillTrial>,
) -> (SkillAppraisal, DemotionDecision) {
    let appraisal = appraise(record_id, &trials, &AppraisalConfig::default());
    let decision = decide_demotion(origin, &appraisal);
    (appraisal, decision)
}

/// A mined record whose recorded turns earn its retirement.
fn demoting(record_id: &str) -> (SkillAppraisal, DemotionDecision) {
    let decided = decided(record_id, SkillOrigin::AutoCreated, harming_window());
    assert!(
        decided.1.is_demotion(),
        "the fixture must be a real demotion, got {:?}",
        decided.1
    );
    decided
}

/// A mined record the measurement leaves alone.
fn keeping(record_id: &str) -> (SkillAppraisal, DemotionDecision) {
    decided(record_id, SkillOrigin::AutoCreated, helping_window())
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
            record_kind: stella_records::context_record::ContextRecordKind::PromotionEvent.as_str(),
            record_hash: &event.record_hash,
            schema_version: stella_records::context_record::LIFECYCLE_SCHEMA_VERSION,
            body: &body,
            observed_at: &event.occurred_at,
            supersedes: None,
        })
        .expect("append");
}

#[test]
fn a_failing_record_is_retired_with_a_legible_reason() {
    let (_dir, store) = store();
    let sweep = sweep(&store, &[demoting("nod_a")], AT);

    assert_eq!(sweep.retired, vec!["nod_a".to_string()]);
    assert!(retired_ids(&store).contains("nod_a"));

    // The gate: retirement decisions carry a human-readable reason.
    let standing = standings(&store);
    let reason = &standing.get("nod_a").expect("standing").reason;
    assert!(!reason.trim().is_empty());
    assert!(
        reason.contains("lift"),
        "the reason must say what the measurement was, got: {reason}"
    );
}

#[test]
fn a_healthy_record_is_never_retired() {
    let (_dir, store) = store();
    let sweep = sweep(&store, &[keeping("nod_a")], AT);

    assert!(sweep.retired.is_empty());
    assert!(retired_ids(&store).is_empty());
}

#[test]
fn a_record_nobody_assessed_is_never_retired() {
    let (_dir, store) = store();
    // Offered constantly, measured never. Disuse is not a negative, and an
    // empty window is `Insufficient` rather than `Inert`.
    let silent = decided("nod_a", SkillOrigin::AutoCreated, Vec::new());

    assert!(sweep(&store, &[silent], AT).retired.is_empty());
}

#[test]
fn a_hand_written_record_is_never_retired_whatever_the_numbers_say() {
    let (_dir, store) = store();
    // The same window that retires a mined record, against one a person
    // wrote. `decide_demotion` checks the origin before it reads a verdict.
    let hand_written = decided("nod_a", SkillOrigin::Workspace, harming_window());

    assert!(sweep(&store, &[hand_written], AT).retired.is_empty());
    assert!(retired_ids(&store).is_empty());
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

    let sweep = sweep(&store, &[demoting("nod_a")], AT);
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

    let sweep = sweep(&store, &[demoting("nod_a")], AT);
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

    let sweep = sweep(&store, &[demoting("nod_a")], AT);
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

    let sweep = sweep(&store, &[demoting("nod_a")], AT);
    assert_eq!(sweep.retired, vec!["nod_a".to_string()]);
}

#[test]
fn retirement_is_reversible_by_reaffirming() {
    let (_dir, store) = store();
    sweep(&store, &[demoting("nod_a")], AT);
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
    sweep(&store, &[demoting("nod_a")], AT);
    reaffirm(&store, "nod_a", "still needed", AT);

    // Both acts remain readable — the ledger is append-only and nothing was
    // deleted. The standing is the latest, not the only.
    let events = store
        .records_of_kind_in_append_order(
            stella_records::context_record::ContextRecordKind::PromotionEvent.as_str(),
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
    sweep(&store, &[demoting("nod_a")], AT);
    reaffirm(&store, "nod_a", "give it another chance", AT);

    // Reaffirmation is not permanent immunity — the loop stays live. Use a
    // distinct timestamp so the second retirement is a different record.
    let again = sweep(&store, &[demoting("nod_a")], "2026-07-27T00:00:00Z");
    assert_eq!(again.retired, vec!["nod_a".to_string()]);
    assert!(retired_ids(&store).contains("nod_a"));
}

#[test]
fn retiring_an_already_retired_record_is_refused_not_duplicated() {
    let (_dir, store) = store();
    sweep(&store, &[demoting("nod_a")], AT);

    let again = sweep(&store, &[demoting("nod_a")], "2026-07-27T00:00:00Z");
    assert!(again.retired.is_empty());
    // Reported nowhere either. A retired record's trials stay in the ledger,
    // so every later sweep re-earns the same demotion, and reporting it would
    // print one line per turn forever.
    assert!(again.refused.is_empty());
    let events = store
        .records_of_kind_in_append_order(
            stella_records::context_record::ContextRecordKind::PromotionEvent.as_str(),
            100,
        )
        .expect("read");
    assert_eq!(events.len(), 1, "no second retirement event");
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
        &[demoting("nod_protected"), demoting("nod_ordinary")],
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
    sweep(&store, &[demoting("nod_a")], AT);

    // Retirement writes one event and touches nothing else. The ledger is
    // append-only at the database, so this is checked twice — but "never
    // physically deletes" is a gate criterion and deserves its own assertion.
    let events = store
        .records_of_kind_in_append_order(
            stella_records::context_record::ContextRecordKind::PromotionEvent.as_str(),
            100,
        )
        .expect("read");
    assert_eq!(events.len(), 1);
}

#[test]
fn a_retirement_reason_is_only_produced_for_a_failing_record() {
    assert!(retirement_reason(&keeping("nod_a").1).is_none());
    assert!(retirement_reason(&demoting("nod_a").1).is_some());
}
