//! Keep / Edit / Ignore over ingested proposals, as an immutable event log.
//!
//! `stella ingest` writes proposals nobody could act on. This module is the review
//! semantics: what each of the three decisions means, how the log folds back into
//! current state, and what stops a declined claim from being proposed again next
//! week.
//!
//! # Why events rather than mutation
//!
//! The DB proposal loop (`stella-cli/src/proposals_cmd.rs`) already records every
//! decision as an immutable event so that replay reproduces state exactly, and the
//! file-based surface mirrors it deliberately. Mutating a proposal in place would
//! destroy the one thing review is *for*: a decline is evidence. Overwriting a
//! proposal's status with `ignored` loses who declined it, when, and why — which is
//! exactly the information the next ingest needs in order not to ask again.
//!
//! # Two substrates, deliberately not merged
//!
//! This is the **file-based** review: proposals extracted from a document, living in
//! `.stella/proposals/*.toml`. The DB `ProposalRecord` loop reviews **behavioral**
//! proposals mined from what the agent did. They have different authority models —
//! an explicit instruction in a tracked file needs no distinct-task threshold, while
//! a mined pattern does — and folding them into one review surface would mean one of
//! the two gets the wrong gate. See `docs/context-pr.md` §7.
//!
//! # The cooldown is the point
//!
//! Without it, `stella ingest AGENTS.md` re-surfaces every claim the user already
//! declined, every run. Review then costs the same as it did the first time and the
//! user learns that declining accomplishes nothing, which is how a review surface
//! becomes a thing people dismiss without reading. [`Ignore`][Decision::Ignore]
//! therefore records negative evidence *and* a deadline, and
//! [`should_repropose`] is the one function ingest asks before offering a candidate.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::clock;

/// How long a declined claim stays declined, unless the caller says otherwise.
///
/// Ninety days is the same order as the record surface's own `review_every`
/// default: long enough that a decline is not busywork, short enough that a claim
/// which became true in the meantime gets another hearing.
pub const DEFAULT_COOLDOWN: &str = "P90D";

/// What a reviewer decided about one proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Publish it: the proposal becomes a record the engine loads.
    Keep,
    /// Publish an amended version. The reviewer's wording supersedes the
    /// extractor's, and the candidate's evidence is preserved — an edit is a
    /// user-authored revision of the same claim, not a new claim.
    Edit,
    /// Decline it. Records negative evidence and starts the re-proposal cooldown.
    Ignore,
}

impl Decision {
    /// The canonical `snake_case` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::Edit => "edit",
            Self::Ignore => "ignore",
        }
    }

    /// Whether this decision publishes a record.
    pub fn publishes(self) -> bool {
        matches!(self, Self::Keep | Self::Edit)
    }
}

/// One immutable review decision. Append-only: nothing ever edits or deletes one of
/// these, and the current state is always a fold over the whole log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionEvent {
    /// The proposal's stable `<slug>-<hash8>` candidate id — the same claim
    /// re-extracted from the same source lands on the same id, which is what makes a
    /// decline stick across runs.
    pub candidate_id: String,
    /// The lineage the proposal would publish into.
    pub lineage_id: String,
    /// What was decided.
    pub decision: Decision,
    /// When, RFC-3339.
    pub at: String,
    /// Who. A local username is enough for solo mode; team mode carries a real
    /// identity.
    pub actor: String,
    /// Why — required in spirit for a decline, since a decline with no reason is
    /// evidence nobody can act on later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// For [`Decision::Ignore`]: when this claim may be proposed again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_until: Option<String>,
    /// For [`Decision::Keep`]/[`Decision::Edit`]: the file the record was published
    /// to, so the log alone explains where a published record came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_to: Option<String>,
    /// Whether the reviewer approved this record blocking tool calls. Separate from
    /// the decision itself: keeping a record and authorizing it to deny a call are
    /// different grants, and conflating them would let a reviewer arm a Tier-2 guard
    /// by pressing "keep" (§11).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub approved_blocking: bool,
}

impl DecisionEvent {
    /// A keep: publish `lineage_id` to `published_to`.
    pub fn keep(
        candidate_id: impl Into<String>,
        lineage_id: impl Into<String>,
        actor: impl Into<String>,
        at: impl Into<String>,
        published_to: impl Into<String>,
    ) -> Self {
        Self {
            candidate_id: candidate_id.into(),
            lineage_id: lineage_id.into(),
            decision: Decision::Keep,
            at: at.into(),
            actor: actor.into(),
            reason: None,
            cooldown_until: None,
            published_to: Some(published_to.into()),
            approved_blocking: false,
        }
    }

    /// An ignore, with the cooldown computed from `at`.
    ///
    /// `cooldown` is an ISO-8601 duration; [`DEFAULT_COOLDOWN`] when `None`. An
    /// unparseable one leaves `cooldown_until` empty, and
    /// [`should_repropose`] treats a decline with no deadline as **permanent** —
    /// erring toward respecting the decline rather than toward asking again.
    pub fn ignore(
        candidate_id: impl Into<String>,
        lineage_id: impl Into<String>,
        actor: impl Into<String>,
        at: impl Into<String>,
        reason: Option<String>,
        cooldown: Option<&str>,
    ) -> Self {
        let at = at.into();
        let cooldown_until = clock::plus_duration(&at, cooldown.unwrap_or(DEFAULT_COOLDOWN));
        Self {
            candidate_id: candidate_id.into(),
            lineage_id: lineage_id.into(),
            decision: Decision::Ignore,
            at,
            actor: actor.into(),
            reason,
            cooldown_until,
            published_to: None,
            approved_blocking: false,
        }
    }
}

/// The current state of one candidate, folded from the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateState {
    /// The most recent decision.
    pub decision: Decision,
    /// When it was made.
    pub at: String,
    /// The cooldown deadline, for a decline.
    pub cooldown_until: Option<String>,
    /// Where a published record landed.
    pub published_to: Option<String>,
    /// Whether blocking was approved.
    pub approved_blocking: bool,
    /// How many decisions this candidate has accumulated — a claim declined three
    /// times is stronger negative evidence than one declined once.
    pub decisions: usize,
}

/// Fold the log into current state, keyed by candidate id.
///
/// Last write wins per candidate, so a reviewer who declined a claim and later kept
/// it gets the keep — while both events remain in the log as the evidence trail. A
/// `BTreeMap` so iteration order is the candidate id's, not a hash seed's: this
/// feeds terminal output, and output that reorders between runs is unreadable.
pub fn fold(events: &[DecisionEvent]) -> BTreeMap<String, CandidateState> {
    let mut states: BTreeMap<String, CandidateState> = BTreeMap::new();
    for event in events {
        let decisions = states
            .get(&event.candidate_id)
            .map_or(0, |state| state.decisions)
            + 1;
        states.insert(
            event.candidate_id.clone(),
            CandidateState {
                decision: event.decision,
                at: event.at.clone(),
                cooldown_until: event.cooldown_until.clone(),
                published_to: event.published_to.clone(),
                // Approval is not sticky across a re-decision: a reviewer who edits
                // a record has changed what they were approving.
                approved_blocking: event.approved_blocking,
                decisions,
            },
        );
    }
    states
}

/// Whether ingest may offer this candidate again.
///
/// `false` when the claim was already published (it is a record now, not a
/// proposal) or was declined and the cooldown has not lapsed. A decline whose
/// cooldown could not be computed is permanent — see [`DecisionEvent::ignore`].
pub fn should_repropose(
    states: &BTreeMap<String, CandidateState>,
    candidate_id: &str,
    now: &str,
) -> bool {
    let Some(state) = states.get(candidate_id) else {
        // Never reviewed. Offer it.
        return true;
    };
    match state.decision {
        Decision::Keep | Decision::Edit => false,
        Decision::Ignore => match state.cooldown_until.as_deref() {
            Some(deadline) => !clock::is_before(now, deadline),
            None => false,
        },
    }
}

/// Whether a human approved this lineage blocking tool calls — the fact
/// [`super::bridge::guard_for`] needs and `stella-core` cannot know on its own.
pub fn blocking_approved(states: &BTreeMap<String, CandidateState>, lineage_id: &str) -> bool {
    states.values().any(|state| {
        state.approved_blocking
            && state.decision.publishes()
            && state
                .published_to
                .as_deref()
                .is_some_and(|path| path.contains(lineage_id))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-07-20T00:00:00Z";

    fn ignored(candidate: &str, at: &str) -> DecisionEvent {
        DecisionEvent::ignore(
            candidate,
            "ctx.a.b.c",
            "mac",
            at,
            Some("the repo moved off this tool".to_string()),
            None,
        )
    }

    #[test]
    fn an_ignore_carries_a_computed_cooldown_and_a_reason() {
        let event = ignored("pkg-manager-9f3c1a2b", NOW);
        assert_eq!(
            event.cooldown_until.as_deref(),
            Some("2026-10-18T00:00:00Z"),
            "90 days from the decision"
        );
        assert!(
            event.reason.is_some(),
            "a decline with no reason is unusable evidence"
        );
    }

    #[test]
    fn a_declined_claim_is_not_reproposed_during_the_cooldown() {
        let states = fold(&[ignored("cand-1", NOW)]);
        assert!(!should_repropose(&states, "cand-1", "2026-08-01T00:00:00Z"));
        assert!(!should_repropose(&states, "cand-1", "2026-10-17T00:00:00Z"));
        assert!(
            should_repropose(&states, "cand-1", "2026-10-19T00:00:00Z"),
            "once the cooldown lapses the claim gets another hearing"
        );
    }

    #[test]
    fn an_uncomputable_cooldown_makes_the_decline_permanent() {
        let event = DecisionEvent::ignore("cand-1", "ctx.a.b.c", "mac", "not-a-date", None, None);
        assert_eq!(event.cooldown_until, None);
        let states = fold(&[event]);
        assert!(
            !should_repropose(&states, "cand-1", "2099-01-01T00:00:00Z"),
            "respecting the decline beats asking again on a deadline nobody can read"
        );
    }

    #[test]
    fn a_kept_claim_is_never_reproposed() {
        let states = fold(&[DecisionEvent::keep(
            "cand-1",
            "ctx.a.b.c",
            "mac",
            NOW,
            ".stella/rules/ctx.a.b.c.toml",
        )]);
        assert!(!should_repropose(&states, "cand-1", "2099-01-01T00:00:00Z"));
    }

    #[test]
    fn an_unreviewed_candidate_is_offered() {
        assert!(should_repropose(&fold(&[]), "brand-new", NOW));
    }

    #[test]
    fn replay_reproduces_state_and_the_last_decision_wins() {
        let log = vec![
            ignored("cand-1", "2026-01-01T00:00:00Z"),
            ignored("cand-1", "2026-02-01T00:00:00Z"),
            DecisionEvent::keep(
                "cand-1",
                "ctx.a.b.c",
                "mac",
                "2026-03-01T00:00:00Z",
                "f.toml",
            ),
        ];
        let states = fold(&log);
        let state = &states["cand-1"];
        assert_eq!(state.decision, Decision::Keep);
        assert_eq!(state.at, "2026-03-01T00:00:00Z");
        assert_eq!(
            state.decisions, 3,
            "the count is the evidence trail — a claim declined twice is stronger evidence"
        );
        // Folding the same log again is the same state: replay, not accumulation.
        assert_eq!(fold(&log), states);
    }

    #[test]
    fn keeping_a_record_does_not_by_itself_approve_blocking() {
        let states = fold(&[DecisionEvent::keep(
            "cand-1",
            "ctx.a.b.no-force-push",
            "mac",
            NOW,
            ".stella/rules/ctx.a.b.no-force-push.toml",
        )]);
        assert!(
            !blocking_approved(&states, "ctx.a.b.no-force-push"),
            "pressing keep must not arm a Tier-2 guard"
        );
    }

    #[test]
    fn an_explicit_blocking_approval_is_readable_by_lineage() {
        let mut event = DecisionEvent::keep(
            "cand-1",
            "ctx.a.b.no-force-push",
            "mac",
            NOW,
            ".stella/rules/ctx.a.b.no-force-push.toml",
        );
        event.approved_blocking = true;
        let states = fold(&[event]);
        assert!(blocking_approved(&states, "ctx.a.b.no-force-push"));
        assert!(!blocking_approved(&states, "ctx.a.b.something-else"));
    }

    #[test]
    fn a_later_edit_re_asks_for_blocking_approval() {
        let mut kept =
            DecisionEvent::keep("c", "ctx.a.b.c", "mac", NOW, ".stella/rules/ctx.a.b.c.toml");
        kept.approved_blocking = true;
        let edited = DecisionEvent {
            decision: Decision::Edit,
            at: "2026-07-21T00:00:00Z".to_string(),
            approved_blocking: false,
            ..kept.clone()
        };
        assert!(
            !blocking_approved(&fold(&[kept, edited]), "ctx.a.b.c"),
            "editing a record changes what was approved, so the grant does not carry over"
        );
    }

    #[test]
    fn the_event_round_trips_as_json_so_the_ledger_is_replayable() {
        let event = ignored("cand-1", NOW);
        let line = serde_json::to_string(&event).expect("serializes");
        let back: DecisionEvent = serde_json::from_str(&line).expect("deserializes");
        assert_eq!(back, event);
    }
}
