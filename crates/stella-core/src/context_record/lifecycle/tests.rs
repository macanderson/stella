//! Tests for the lifecycle record bodies.
//!
//! Three of the Phase 3 gate criteria are decided entirely by this module, and
//! are asserted here rather than only end-to-end:
//!
//! * three distinct tasks produce one eligible proposal; thirty repetitions
//!   inside one task produce none;
//! * no inferred directive reaches blocking by any path;
//! * Keep/Edit/Ignore replay identically from the event log.

use super::*;

const AT: &str = "2026-07-26T12:00:00Z";

fn observation(task: &str, text: &str) -> ObservationRecord {
    ObservationRecord::new(
        ObservationSource::ReflectionLesson,
        format!("reflection:{task}"),
        task,
        text,
        vec!["testing".into()],
        false,
        AT,
    )
    .expect("observation")
}

fn score(occurrences: u32, distinct_tasks: u32) -> ProposalScore {
    ProposalScore {
        occurrences,
        distinct_tasks,
        salient: false,
        rank: 30.0,
    }
}

fn proposal(score: ProposalScore) -> ProposalRecord {
    ProposalRecord::new(
        RecordProposalKind::Knowledge,
        RecordProposalStatus::Eligible,
        "prefer-rg-over-grep-abcd1234",
        "Prefer rg over grep",
        "Use ripgrep instead of grep in this repository.",
        vec!["tooling".into()],
        vec!["obs_a".into(), "obs_b".into(), "obs_c".into()],
        score,
        confidence_from_score(&score).expect("confidence"),
        AT,
    )
    .expect("proposal")
}

// ---- identity and hashing ----

/// Replay-idempotence rests entirely on this: identical evidence must produce
/// an identical id, so the ledger's append recognizes the second write as a
/// replay instead of minting a duplicate observation.
#[test]
fn an_observation_id_is_derived_from_its_content() {
    let a = observation("task-1", "Prefer rg over grep.");
    let b = observation("task-1", "Prefer rg over grep.");
    assert_eq!(a.record_id, b.record_id);
    assert_eq!(a.record_hash, b.record_hash);
    assert_eq!(a, b);
}

/// …and different evidence must not collide, including evidence that differs
/// *only* by which task it came from. Task identity is the anti-poisoning axis;
/// if two tasks' observations collapsed onto one id, thirty repetitions in one
/// task would be indistinguishable from thirty tasks.
#[test]
fn observations_from_different_tasks_have_different_ids() {
    let a = observation("task-1", "Prefer rg over grep.");
    let b = observation("task-2", "Prefer rg over grep.");
    assert_ne!(a.record_id, b.record_id);
}

/// The hash covers the id. A record whose hash did not would be re-fileable
/// under another id without the hash noticing.
#[test]
fn the_record_hash_covers_the_record_id() {
    let mut record = observation("task-1", "Prefer rg over grep.");
    let sealed = record.record_hash.clone();
    record.record_id = "obs_forged".into();
    assert_ne!(
        super::record_hash(&record).expect("rehash"),
        sealed,
        "the id is outside the hash preimage — a record could be re-filed \
         under another identity and still verify"
    );
}

/// Every record verifies against its own content, so a reader need not trust
/// whoever wrote the row.
#[test]
fn every_lifecycle_record_verifies_against_its_own_body() {
    let obs = observation("task-1", "Prefer rg over grep.");
    let mut unsealed = obs.clone();
    unsealed.record_hash = String::new();
    assert_eq!(super::record_hash(&obs).expect("hash"), obs.record_hash);
    // The stored hash is exactly the hash of the record with the hash field
    // dropped, which is what `record_hash` does — so an unsealed copy agrees.
    assert_eq!(
        super::record_hash(&unsealed).expect("hash"),
        obs.record_hash
    );
}

/// A proposal's lineage is its candidate id, so re-inducing the same candidate
/// tomorrow lands in the same lineage and a decline can be remembered against
/// it — while a re-induction with different evidence is a new *revision*, not a
/// silent overwrite.
#[test]
fn a_proposal_lineage_is_stable_while_its_revision_tracks_evidence() {
    let first = proposal(score(3, 3));
    let second = proposal(score(9, 5));
    assert_eq!(first.lineage_id, second.lineage_id);
    assert_ne!(first.record_id, second.record_id);
}

// ---- GATE: distinct tasks, never raw events ----

/// The gate criterion, both halves, decided by the field the eligibility check
/// reads.
#[test]
fn three_distinct_tasks_are_eligible_and_thirty_repetitions_in_one_task_are_not() {
    let promotion = (3u32, 3u32); // settings default: min_distinct_tasks, min_observations

    let three_tasks = proposal(score(3, 3));
    assert!(
        three_tasks.is_eligible(promotion.0, promotion.1),
        "three observations across three distinct tasks must be eligible"
    );

    let thirty_in_one_task = proposal(score(30, 1));
    assert!(
        !thirty_in_one_task.is_eligible(promotion.0, promotion.1),
        "thirty repetitions inside ONE task must never satisfy a three-task \
         threshold, however many events back them"
    );

    // Two tasks are still short, even with plenty of events.
    assert!(!proposal(score(20, 2)).is_eligible(promotion.0, promotion.1));
    // And three tasks with too little evidence overall are also short: both
    // axes gate, distinct tasks is simply the one that cannot be gamed by
    // repetition.
    assert!(!proposal(score(2, 3)).is_eligible(promotion.0, promotion.1));
}

/// Confidence rises with distinct tasks far faster than with repetition, so a
/// poisoning attempt cannot buy auto-activation by repeating itself either.
#[test]
fn confidence_is_driven_by_distinct_tasks_not_repetition() {
    let repeated = confidence_from_score(&score(30, 1)).expect("confidence");
    let spread = confidence_from_score(&score(3, 3)).expect("confidence");
    assert!(
        spread.get() > repeated.get(),
        "three tasks ({}) must outrank thirty repetitions in one ({})",
        spread.get(),
        repeated.get()
    );
    assert!(
        repeated.get() < 85,
        "thirty repetitions in one task reached the auto-activation threshold \
         ({}) — the anti-poisoning rule is bypassable through confidence",
        repeated.get()
    );
}

// ---- GATE: no inferred directive reaches blocking by any path ----

/// There is exactly one constructor, and it refuses. That is what makes "by any
/// path" a checkable claim rather than a hope about call sites.
#[test]
fn no_system_promotion_can_grant_blocking_enforcement() {
    for action in [
        PromotionAction::Proposed,
        PromotionAction::AutoActivated,
        PromotionAction::Confirmed,
        PromotionAction::Published,
        PromotionAction::Rejected,
        PromotionAction::Retired,
        PromotionAction::Reverted,
    ] {
        let attempt = PromotionEventRecord::new(
            "prp_x",
            action,
            PromotionActor::System,
            Some(DirectiveEnforcement::Blocking),
            None,
            "trying it on",
            AT,
        );
        assert!(
            matches!(
                attempt,
                Err(PromotionEventError::SystemGrantedBlocking { .. })
            ),
            "the system reached blocking through {action:?}"
        );
    }
}

/// An auto-activation is advisory whoever triggers it — including a user, who
/// has not confirmed anything by letting the loop run.
#[test]
fn an_auto_activation_can_never_be_blocking() {
    assert!(matches!(
        PromotionEventRecord::new(
            "prp_x",
            PromotionAction::AutoActivated,
            PromotionActor::User,
            Some(DirectiveEnforcement::Blocking),
            None,
            "auto",
            AT,
        ),
        Err(PromotionEventError::AutoActivatedBlocking)
    ));
}

/// The one legitimate path to blocking: a person, confirming explicitly.
#[test]
fn a_user_confirmation_is_the_only_route_to_blocking() {
    let confirmed = PromotionEventRecord::new(
        "prp_x",
        PromotionAction::Confirmed,
        PromotionActor::User,
        Some(DirectiveEnforcement::Blocking),
        None,
        "I want this enforced",
        AT,
    )
    .expect("an explicit human confirmation may grant blocking");
    assert_eq!(confirmed.enforcement, Some(DirectiveEnforcement::Blocking));
}

/// And the enforcement an inferred record starts at is not a settings lookup.
#[test]
fn inferred_records_start_advisory() {
    assert_eq!(
        PromotionEventRecord::initial_enforcement_for_inferred(),
        DirectiveEnforcement::Advisory
    );
}

/// A governance decision with no recorded reason is not auditable.
#[test]
fn a_promotion_event_must_carry_a_reason() {
    assert!(matches!(
        PromotionEventRecord::new(
            "prp_x",
            PromotionAction::Confirmed,
            PromotionActor::User,
            None,
            None,
            "   ",
            AT,
        ),
        Err(PromotionEventError::MissingReason)
    ));
}

// ---- GATE: Keep/Edit/Ignore replay identically from the event log ----

/// The three review outcomes are distinguishable, content-addressed, and
/// reconstructible: replaying the same three decisions produces byte-identical
/// records, so "what did the user decide" is answered from the log rather than
/// from a mutable status column.
#[test]
fn keep_edit_and_ignore_replay_identically() {
    let decisions = || {
        vec![
            PromotionEventRecord::new(
                "prp_a",
                PromotionAction::Confirmed,
                PromotionActor::User,
                Some(DirectiveEnforcement::Advisory),
                None,
                "keep: matches how I actually work",
                AT,
            )
            .expect("keep"),
            PromotionEventRecord::new(
                "prp_b",
                PromotionAction::Confirmed,
                PromotionActor::User,
                Some(DirectiveEnforcement::Advisory),
                Some("Use ripgrep, and pass -uu when searching ignored files.".into()),
                "edit: sharpened the wording",
                AT,
            )
            .expect("edit"),
            PromotionEventRecord::new(
                "prp_c",
                PromotionAction::Rejected,
                PromotionActor::User,
                None,
                None,
                "ignore: not a convention, just a habit",
                AT,
            )
            .expect("ignore"),
        ]
    };
    let first = decisions();
    let second = decisions();
    assert_eq!(
        first, second,
        "the same decisions did not replay identically"
    );

    // The three outcomes are genuinely distinct records, not one shape with a
    // label — a replay that collapsed them would still pass an equality check.
    let ids: Vec<&str> = first.iter().map(|e| e.record_id.as_str()).collect();
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        3
    );
    assert_eq!(
        first[1].edited_body.as_deref().unwrap_or_default().len(),
        55
    );
    assert!(first[2].edited_body.is_none());
}

/// An observation carries no instruction authority: nothing on it can express
/// enforcement, priority, or an effect. This is a type-level assertion rather
/// than a runtime one — if a future field made an observation steerable, this
/// test's premise would need rewriting, which is the point.
#[test]
fn an_observation_has_no_enforcement_field() {
    let obs = observation("task-1", "Prefer rg over grep.");
    let json = serde_json::to_value(&obs).expect("serialize");
    let map = json.as_object().expect("object");
    for forbidden in ["enforcement", "priority", "effect", "directive_kind"] {
        assert!(
            !map.contains_key(forbidden),
            "an observation gained a `{forbidden}` field — observations inform, \
             they never steer (spec §5.4)"
        );
    }
    assert_eq!(map["origin"], "observed");
}
