//! Reversible retirement — Phase 4 deliverable 5 (#715).
//!
//! Context that repeatedly fails to help stops being selected — visibly, with
//! reasons, and reversibly.
//!
//! # Retirement is derived, not stored
//!
//! A retirement is an immutable `promotion_event` carrying
//! [`PromotionAction::Retired`]; a reaffirmation is one carrying
//! [`PromotionAction::Reverted`]. What a record's standing *is* comes from
//! folding those events last-write-wins — exactly the state machine
//! [`crate::proposals_cmd::decisions`] already runs for keep/ignore, and for
//! the same reason: spec §5.2 forbids storing `stale` as state, and a fold over
//! append-only events is reversible by construction. Reaffirming is not an undo
//! that erases anything; it is a later fact that outranks an earlier one, and
//! both stay readable.
//!
//! Reusing `promotion_event` rather than minting a record kind is deliberate.
//! It already requires a human-readable reason at the constructor
//! (`PromotionEventError::MissingReason`), which is one of the gate criteria,
//! and `Retired`/`Reverted` were already in [`PromotionAction`] with no writer.
//! The one stretch is the field name: `proposal_lineage_id` holds the retired
//! record's id here, because the field is "the lineage this decision is about"
//! and a retirement is a decision about a lineage. That widening is recorded
//! here rather than left for a reader to infer.
//!
//! # Why this is not a second suppression mechanism
//!
//! Spec §5.7 says a second suppression mechanism is a defect. Retirement adds
//! no new one: it joins the union [`crate::memory::suppression`] already reads
//! fresh on every recall, beside derived quarantine and stored tombstones. It
//! writes no `superseded_at`, which is what keeps a retired record **explicitly
//! retrievable** — `node_by_public_id` filters on that column, so routing
//! retirement through `supersede_node` would have made retired records
//! un-fetchable by id and failed the gate outright.
//!
//! Nothing here deletes anything, ever.

use std::collections::{HashMap, HashSet};

use stella_context::ContextStore;
use stella_core::context_record::{
    ContextRecordKind, LIFECYCLE_SCHEMA_VERSION, PromotionAction, PromotionActor,
    PromotionEventRecord, SelectionHealth,
};

/// How many promotion events one replay reads. Matches the proposals surface's
/// own bound — the ledger grows monotonically and a full read is unbounded work
/// on the turn path.
const READ_LIMIT: usize = 5_000;

/// Why a record may never be retired automatically.
///
/// The gate criterion is "protected categories are provably never retired",
/// and *provably* is only meaningful if there is exactly one predicate — a
/// check a caller can forget to run leaves the other paths open. Every
/// retirement in this module goes through [`protection_for`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetirementProtection {
    /// A person confirmed this record. Their judgement outranks the loop's.
    UserConfirmed,
    /// Published to a team. Retiring it locally would diverge shared state.
    Published,
    /// Carries blocking enforcement — it steers, and §5.4 puts blocking
    /// exclusively under human control in both directions.
    Blocking,
    /// Already retired; retiring again would be a no-op event that muddies the
    /// replay.
    AlreadyRetired,
}

impl RetirementProtection {
    /// A human-readable phrase for a report.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::UserConfirmed => "user-confirmed",
            Self::Published => "published",
            Self::Blocking => "blocking",
            Self::AlreadyRetired => "already retired",
        }
    }
}

/// The standing of every record the promotion log has an opinion about.
///
/// Append order, not `observed_at` — the same reasoning as
/// [`crate::proposals_cmd::promotion_events`]: two events in the same second
/// share a timestamp and would otherwise be ordered by content hash, which is a
/// deterministic wrong answer rather than a flake.
pub(crate) fn standings(store: &ContextStore) -> HashMap<String, PromotionEventRecord> {
    let mut out = HashMap::new();
    let rows = store
        .records_of_kind_in_append_order(ContextRecordKind::PromotionEvent.as_str(), READ_LIMIT)
        .unwrap_or_default();
    for row in rows {
        let Ok(event) = serde_json::from_str::<PromotionEventRecord>(&row.body) else {
            continue;
        };
        out.insert(event.proposal_lineage_id.clone(), event);
    }
    out
}

/// Records currently retired — the set the suppression reader unions in.
///
/// A record whose latest event is `Reverted` (or `Confirmed`, or anything
/// else) is **not** here: reaffirmation restores it by outranking the
/// retirement, without erasing it.
pub(crate) fn retired_ids(store: &ContextStore) -> HashSet<String> {
    standings(store)
        .into_iter()
        .filter(|(_, event)| event.action == PromotionAction::Retired)
        .map(|(lineage, _)| lineage)
        .collect()
}

/// Why this record may not be retired, or `None` if it may.
///
/// # The five named categories, and why only three are checked
///
/// The deliverable names five: blocking, critical, user-confirmed, published,
/// and pinned. Three are checked here. The other two are **not expressible in
/// this tree today**, and stating that is more useful than a check that only
/// looks like protection:
///
/// - **critical.** [`DirectivePriority::Critical`] exists as an enum variant
///   and is carried on no record, set by no code path, and read by nothing. A
///   record therefore cannot *be* critical, so "critical records are never
///   retired" holds vacuously.
/// - **pinned.** No pin field, flag, or command exists for memory or context
///   records anywhere (`RoleTable::pin` is model routing, unrelated).
///
/// Both are guarded by the same thing instead: this function is the single
/// predicate every retirement passes through, so when either concept becomes
/// real its check goes here and nowhere else. Inventing a mechanism now purely
/// to have something to protect would have been the worse answer — it would
/// make the gate criterion *look* satisfied by code that never fires.
///
/// [`DirectivePriority::Critical`]: stella_core::context_record::DirectivePriority
pub(crate) fn protection_for(
    standing: Option<&PromotionEventRecord>,
) -> Option<RetirementProtection> {
    let standing = standing?;
    if standing.enforcement == Some(stella_core::context_record::DirectiveEnforcement::Blocking) {
        return Some(RetirementProtection::Blocking);
    }
    match standing.action {
        // A user's confirmation is the strongest signal in the system. Note
        // this checks the ACTION, not the actor: a system auto-activation is
        // not a confirmation, and must stay retirable.
        PromotionAction::Confirmed if standing.actor == PromotionActor::User => {
            Some(RetirementProtection::UserConfirmed)
        }
        PromotionAction::Published => Some(RetirementProtection::Published),
        PromotionAction::Retired => Some(RetirementProtection::AlreadyRetired),
        _ => None,
    }
}

/// The retirement a failing record has earned, or `None`.
///
/// Separated from the write so the decision is testable without a store, and
/// so the reason is built in one place — a retirement with no legible cause is
/// exactly what the gate criterion forbids.
pub(crate) fn retirement_reason(health: &SelectionHealth) -> Option<String> {
    if !health.failing {
        return None;
    }
    Some(format!(
        "not helpful in {} of {} assessed uses across {} task(s) — \
         auto-retired by the efficacy loop; reaffirm to restore",
        health.not_helpful, health.assessed_uses, health.distinct_tasks
    ))
}

/// Outcome of one retirement sweep, for reporting.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct RetirementSweep {
    /// Records newly retired.
    pub(crate) retired: Vec<String>,
    /// Records that were failing but are protected, with why. Reported rather
    /// than silently skipped — a sweep that quietly declines to act on half its
    /// candidates reads as "nothing was failing", which is a different claim.
    pub(crate) refused: Vec<(String, RetirementProtection)>,
}

/// Retire every failing, unprotected record. Returns what it did.
///
/// The actor is [`PromotionActor::System`] and the enforcement is `None`: a
/// system event may never carry blocking enforcement, and the constructor
/// refuses one that tries.
pub(crate) fn sweep(
    store: &ContextStore,
    health: &[SelectionHealth],
    occurred_at: &str,
) -> RetirementSweep {
    let standings = standings(store);
    let mut sweep = RetirementSweep::default();
    for record in health {
        let Some(reason) = retirement_reason(record) else {
            continue;
        };
        if let Some(protection) = protection_for(standings.get(&record.context_record_id)) {
            sweep
                .refused
                .push((record.context_record_id.clone(), protection));
            continue;
        }
        if record_decision(
            store,
            &record.context_record_id,
            PromotionAction::Retired,
            PromotionActor::System,
            &reason,
            occurred_at,
        ) {
            sweep.retired.push(record.context_record_id.clone());
        }
    }
    sweep
}

/// Retire a record on a person's explicit judgement.
///
/// The actor is [`PromotionActor::User`], which is what distinguishes this from
/// the sweep's system decision in the replay — "who decided this" is the first
/// question anyone asks of a retirement, and the log answers it without
/// inference.
pub(crate) fn retire_explicitly(
    store: &ContextStore,
    record_id: &str,
    reason: &str,
    occurred_at: &str,
) -> bool {
    record_decision(
        store,
        record_id,
        PromotionAction::Retired,
        PromotionActor::User,
        reason,
        occurred_at,
    )
}

/// Reaffirm a retired record, restoring it to automatic selection.
///
/// Returns whether the event was written. This is the gate's "restorable", and
/// it is a plain append: the retirement stays in the log, outranked.
pub(crate) fn reaffirm(
    store: &ContextStore,
    record_id: &str,
    reason: &str,
    occurred_at: &str,
) -> bool {
    record_decision(
        store,
        record_id,
        PromotionAction::Reverted,
        PromotionActor::User,
        reason,
        occurred_at,
    )
}

/// Append one retirement-plane decision. `false` on any failure.
fn record_decision(
    store: &ContextStore,
    record_id: &str,
    action: PromotionAction,
    actor: PromotionActor,
    reason: &str,
    occurred_at: &str,
) -> bool {
    let Ok(event) = PromotionEventRecord::new(
        record_id,
        action,
        actor,
        // Never an enforcement grant: retiring and reaffirming change whether a
        // record is selected, never what authority it carries.
        None,
        None,
        reason,
        occurred_at,
    ) else {
        return false;
    };
    let Ok(body) = serde_json::to_string(&event) else {
        return false;
    };
    store
        .append_record(stella_context::LedgerAppend {
            record_id: &event.record_id,
            lineage_id: &event.lineage_id,
            record_kind: ContextRecordKind::PromotionEvent.as_str(),
            record_hash: &event.record_hash,
            schema_version: LIFECYCLE_SCHEMA_VERSION,
            body: &body,
            observed_at: &event.occurred_at,
            supersedes: None,
        })
        .is_ok()
}

#[cfg(test)]
mod tests;
