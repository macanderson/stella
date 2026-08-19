//! The PR rhythm, as arithmetic.
//!
//! `doc:backlog-self-driving` §3.3. Once the loop has a verified diff it has to
//! *ship* it, and shipping is a sequence of decisions over observed facts:
//! CI concluded, a review asked for something, the base moved, the same fix
//! failed twice. None of that needs a model, and this module is where the
//! deciding lives so that `deliver next` **buys no model call** — only *doing*
//! the thing it decided (writing the fix for a red build) is a `work`
//! invocation.
//!
//! That mirrors the terminality rule the deleted staged pipeline held for
//! `ladder_decision`: every arm of this machine ends in an action, and no arm
//! escalates to a model to ask what it should have concluded.
//!
//! # Two commitments, both paid for already
//!
//! **[`PrState::CiRed`] and [`PrState::BaseBroken`] are different states.** A
//! failure that reproduces on the base branch is not this PR's failure, and
//! treating it as one burns every cycle on somebody else's breakage. That is
//! not hypothetical here: it is the first diagnostic question
//! `/self-driving:evolve` asks under `STUCK`, and `main-canary.yml` exists
//! because this repository's `main` does go red on its own. The machine splits
//! them structurally, so the loop cannot get it wrong by forgetting to ask.
//! `a_red_build_that_reproduces_on_the_base_is_not_this_prs_fault` is the
//! witness, and `the_same_red_build_on_a_green_base_does_earn_a_fix` is its
//! other half — either assertion alone would pass on a machine that always
//! answered one way.
//!
//! **[`PrState::Escalated`] is terminal and reachable from everywhere.** A loop
//! that cannot give up is a loop that pushes the same broken fix forever.
//! Repeat-failure counting is here, in the pure machine; the ceiling is policy,
//! because how many attempts are worth buying is a spend question and spend is
//! the operator's.

use serde::{Deserialize, Serialize};

/// What a CI run concluded, collapsed to what the loop decides on.
///
/// Three, not a forge's full vocabulary: a cancelled run, a skipped run and a
/// run that never started are all "no answer yet" as far as the next action is
/// concerned, and flattening them here keeps every forge adapter from having to
/// invent a mapping into a richer enum nothing branches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CiConclusion {
    /// No conclusion yet. Waiting is the only honest action.
    Pending,
    /// Every required check passed.
    Green,
    /// At least one required check failed.
    Red,
}

/// Whether the branch can merge as-is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mergeability {
    /// No conflict with the base.
    Clean,
    /// The base moved under it.
    Conflicted,
    /// The forge has not computed it yet. Distinct from [`Self::Clean`] on
    /// purpose: GitHub reports `null` while it computes, and reading that as
    /// "clean" is how a merge is attempted into a conflict.
    Unknown,
}

/// What human review has said, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    /// Nobody has reviewed it.
    None,
    /// A reviewer asked for changes. Outranks a green build.
    ChangesRequested,
    /// A reviewer approved it.
    Approved,
}

/// One read of the forge. Facts only — no decisions, per §3.3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    /// What this PR's checks concluded.
    pub ci: CiConclusion,
    /// What the *same* checks concluded on the base branch.
    ///
    /// The load-bearing field, and the reason `deliver observe` reads twice.
    /// Without it a red build is indistinguishable from an inherited one, and
    /// the loop spends its whole budget fixing `main`.
    pub base_ci: CiConclusion,
    /// Whether the branch still merges cleanly.
    pub mergeable: Mergeability,
    /// What review has said.
    pub review: ReviewState,
    /// Whether the PR is still a draft.
    pub draft: bool,
}

/// How many times the loop has already tried each remedy on this PR.
///
/// Counting lives in the pure machine so that "we have been here before" is a
/// fact the transition can read, rather than a judgement the caller makes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempts {
    /// Pushes made in response to a red build on this PR.
    pub fixes: u32,
    /// Rebases made in response to a conflict.
    pub rebases: u32,
}

/// The operator's ceilings. Policy, not arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliverPolicy {
    /// How many fix pushes to buy before escalating to a human.
    pub fix_ceiling: u32,
    /// How many rebases to buy before escalating.
    pub rebase_ceiling: u32,
    /// Whether a human approval is required before merging.
    ///
    /// **Deliberately explicit rather than implied by the forge.** A loop
    /// running unattended against a repository whose branch protection does not
    /// require review will merge its own work, and that is a decision an
    /// operator should make in a config file they can read — not one they
    /// discover from the fact that it happened.
    pub require_approval: bool,
}

impl Default for DeliverPolicy {
    /// Conservative: two fixes, one rebase, and a human on the merge.
    ///
    /// A default that merges without review would make the *safe* choice the
    /// one an operator has to remember to opt into.
    fn default() -> Self {
        Self {
            fix_ceiling: 2,
            rebase_ceiling: 1,
            require_approval: true,
        }
    }
}

/// Where a PR the loop opened currently is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    /// Opened, not yet pushed for checks.
    Draft,
    /// Checks are running.
    CiPending,
    /// This PR's checks failed and the base is green, so it is this PR's fault.
    CiRed,
    /// This PR's checks failed and so does the base. Not this PR's fault.
    BaseBroken,
    /// The base moved under it.
    Conflicted,
    /// Green, out of draft, waiting on review.
    ReadyForReview,
    /// A reviewer asked for changes.
    ReviewChangesRequested,
    /// Cleared to merge.
    Approved,
    /// Landed. Terminal.
    Merged,
    /// Handed to a human. Terminal.
    Escalated,
}

impl PrState {
    /// Whether the machine is done with this PR either way.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Merged | Self::Escalated)
    }
}

/// Why the loop gave up.
///
/// Typed because a human reading an escalation needs to know whether to fix
/// something, wait for something, or change a ceiling — three different next
/// moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationReason {
    /// The same build kept failing after the policy's fix ceiling.
    FixCeilingReached,
    /// The branch kept conflicting after the policy's rebase ceiling.
    RebaseCeilingReached,
    /// A reviewer asked for something the loop is not permitted to decide.
    ReviewNeedsHuman,
}

/// The single next thing to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub enum Action {
    /// Do nothing and observe again later. The correct action far more often
    /// than the others, and naming it beats a caller inventing a sleep.
    Wait,
    /// Run `work` to author a fix for this PR's own failure, then push.
    PushFix,
    /// Rebase onto the moved base and push.
    Rebase,
    /// Take it out of draft.
    MarkReady,
    /// Merge it. Emitted only from [`PrState::Approved`].
    Merge,
    /// Stop, and tell a human why.
    Escalate {
        /// What ran out.
        reason: EscalationReason,
    },
}

/// The state the machine moved to, and what to do there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transition {
    /// Where the PR is now.
    pub state: PrState,
    /// The one next action.
    pub action: Action,
}

/// The deterministic transition: given what was observed and what has already
/// been tried, the single next action.
///
/// Total over its inputs and free of I/O, a clock and randomness, so the same
/// observation always yields the same move. That is what makes a perpetual
/// loop's behaviour reviewable after the fact rather than merely plausible.
///
/// # Precedence
///
/// The order the conditions are tested in **is** the policy, so it is stated
/// rather than left to be reverse-engineered from the `if` chain:
///
/// 1. **Terminal states absorb.** [`PrState::Merged`] and
///    [`PrState::Escalated`] never move again.
/// 2. **A ceiling already reached escalates**, before anything else is tried.
/// 3. **`ChangesRequested` outranks a green build** — a human asked for
///    something, and merging past it because CI is green is the machine
///    overruling a person.
/// 4. **A conflict outranks CI**, because a conflicted branch's checks are
///    describing a merge that cannot happen.
/// 5. **A red base outranks a red build.** This is the commitment in the module
///    docs, and it is tested before the PR is blamed for anything.
#[must_use]
pub fn deliver_next(
    state: PrState,
    obs: &Observation,
    attempts: Attempts,
    policy: &DeliverPolicy,
) -> Transition {
    // 1. Terminal states absorb.
    if state.is_terminal() {
        return Transition {
            state,
            action: Action::Wait,
        };
    }

    // 2. A ceiling already reached escalates before anything else is tried.
    if attempts.fixes >= policy.fix_ceiling {
        return escalate(EscalationReason::FixCeilingReached);
    }
    if attempts.rebases >= policy.rebase_ceiling {
        return escalate(EscalationReason::RebaseCeilingReached);
    }

    // 3. A human asking for changes outranks a green build.
    if obs.review == ReviewState::ChangesRequested {
        return Transition {
            state: PrState::ReviewChangesRequested,
            action: Action::PushFix,
        };
    }

    // 4. A conflicted branch's checks describe a merge that cannot happen.
    if obs.mergeable == Mergeability::Conflicted {
        return Transition {
            state: PrState::Conflicted,
            action: Action::Rebase,
        };
    }

    match obs.ci {
        CiConclusion::Pending => Transition {
            state: PrState::CiPending,
            action: Action::Wait,
        },

        // 5. A red base outranks a red build: this is not the PR's failure, and
        //    pushing a fix for it would be fixing somebody else's breakage on
        //    this PR's budget.
        CiConclusion::Red if obs.base_ci == CiConclusion::Red => Transition {
            state: PrState::BaseBroken,
            action: Action::Wait,
        },
        CiConclusion::Red => Transition {
            state: PrState::CiRed,
            action: Action::PushFix,
        },

        CiConclusion::Green => green(obs, policy),
    }
}

/// The green path, split out because it is the only one with three outcomes.
fn green(obs: &Observation, policy: &DeliverPolicy) -> Transition {
    if obs.draft {
        return Transition {
            state: PrState::ReadyForReview,
            action: Action::MarkReady,
        };
    }

    // Mergeability must be *known* clean. `Unknown` means the forge has not
    // finished computing it, and reading that as clean is how a merge is
    // attempted into a conflict.
    if obs.mergeable != Mergeability::Clean {
        return Transition {
            state: PrState::CiPending,
            action: Action::Wait,
        };
    }

    match (policy.require_approval, obs.review) {
        (true, ReviewState::Approved) | (false, _) => Transition {
            state: PrState::Approved,
            action: Action::Merge,
        },
        (true, _) => Transition {
            state: PrState::ReadyForReview,
            action: Action::Wait,
        },
    }
}

fn escalate(reason: EscalationReason) -> Transition {
    Transition {
        state: PrState::Escalated,
        action: Action::Escalate { reason },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs() -> Observation {
        Observation {
            ci: CiConclusion::Green,
            base_ci: CiConclusion::Green,
            mergeable: Mergeability::Clean,
            review: ReviewState::None,
            draft: false,
        }
    }

    /// The witness for §3.3's first commitment. A build that fails the same way
    /// on the base branch is not this PR's failure, and the loop must not spend
    /// a `work` invocation authoring a fix for it.
    #[test]
    fn a_red_build_that_reproduces_on_the_base_is_not_this_prs_fault() {
        let inherited = Observation {
            ci: CiConclusion::Red,
            base_ci: CiConclusion::Red,
            ..obs()
        };

        let t = deliver_next(
            PrState::CiPending,
            &inherited,
            Attempts::default(),
            &DeliverPolicy::default(),
        );

        assert_eq!(
            t,
            Transition {
                state: PrState::BaseBroken,
                action: Action::Wait,
            },
            "a red base must not cost this PR a fix push"
        );
    }

    /// The other half of the same gate: with a green base, the identical red
    /// build *is* this PR's problem. Either assertion alone is half a witness.
    #[test]
    fn the_same_red_build_on_a_green_base_does_earn_a_fix() {
        let mine = Observation {
            ci: CiConclusion::Red,
            base_ci: CiConclusion::Green,
            ..obs()
        };

        let t = deliver_next(
            PrState::CiPending,
            &mine,
            Attempts::default(),
            &DeliverPolicy::default(),
        );

        assert_eq!(t.state, PrState::CiRed);
        assert_eq!(t.action, Action::PushFix);
    }

    /// A loop that cannot give up pushes the same broken fix forever.
    #[test]
    fn the_fix_ceiling_escalates_and_escalation_is_terminal() {
        let mine = Observation {
            ci: CiConclusion::Red,
            base_ci: CiConclusion::Green,
            ..obs()
        };
        let policy = DeliverPolicy::default();
        // Exactly the ceiling: the operator bought `fix_ceiling` fixes, so
        // having spent that many means the budget is gone — this must escalate
        // rather than buy one more.
        let spent = Attempts {
            fixes: policy.fix_ceiling,
            rebases: 0,
        };

        let t = deliver_next(PrState::CiRed, &mine, spent, &policy);
        assert_eq!(t.state, PrState::Escalated);
        assert_eq!(
            t.action,
            Action::Escalate {
                reason: EscalationReason::FixCeilingReached
            }
        );

        // Terminal means terminal: observing again does not restart it.
        let again = deliver_next(t.state, &obs(), spent, &policy);
        assert_eq!(again.state, PrState::Escalated);
        assert_eq!(again.action, Action::Wait);
    }

    /// Merging past a human who asked for changes because CI is green is the
    /// machine overruling a person.
    #[test]
    fn changes_requested_outranks_a_green_build() {
        let reviewed = Observation {
            review: ReviewState::ChangesRequested,
            ..obs()
        };

        let t = deliver_next(
            PrState::ReadyForReview,
            &reviewed,
            Attempts::default(),
            &DeliverPolicy::default(),
        );

        assert_eq!(t.state, PrState::ReviewChangesRequested);
        assert_eq!(t.action, Action::PushFix);
    }

    /// `Unknown` is the forge still computing. Reading it as clean is how a
    /// merge is attempted into a conflict.
    #[test]
    fn an_uncomputed_mergeability_waits_rather_than_merging() {
        let uncomputed = Observation {
            mergeable: Mergeability::Unknown,
            review: ReviewState::Approved,
            ..obs()
        };

        let t = deliver_next(
            PrState::ReadyForReview,
            &uncomputed,
            Attempts::default(),
            &DeliverPolicy::default(),
        );

        assert_eq!(t.action, Action::Wait, "must not merge on an unknown");
    }

    /// The only state `Merge` is ever emitted from.
    #[test]
    fn merge_is_reachable_only_through_approved() {
        let approved = Observation {
            review: ReviewState::Approved,
            ..obs()
        };

        let t = deliver_next(
            PrState::ReadyForReview,
            &approved,
            Attempts::default(),
            &DeliverPolicy::default(),
        );

        assert_eq!(t.state, PrState::Approved);
        assert_eq!(t.action, Action::Merge);
    }

    /// The unattended case, and the reason the field is explicit: with review
    /// not required the same observation merges, and an operator can read that
    /// choice in a config file rather than discover it from the outcome.
    #[test]
    fn without_required_approval_a_green_pr_merges_itself() {
        let policy = DeliverPolicy {
            require_approval: false,
            ..DeliverPolicy::default()
        };

        let t = deliver_next(
            PrState::ReadyForReview,
            &obs(),
            Attempts::default(),
            &policy,
        );

        assert_eq!(t.action, Action::Merge);

        // And with the conservative default, the identical observation does not.
        let guarded = deliver_next(
            PrState::ReadyForReview,
            &obs(),
            Attempts::default(),
            &DeliverPolicy::default(),
        );
        assert_eq!(guarded.action, Action::Wait);
    }

    /// A draft that is green gets taken out of draft, not merged.
    #[test]
    fn a_green_draft_is_marked_ready_first() {
        let draft = Observation {
            draft: true,
            review: ReviewState::Approved,
            ..obs()
        };

        let t = deliver_next(
            PrState::CiPending,
            &draft,
            Attempts::default(),
            &DeliverPolicy::default(),
        );

        assert_eq!(t.state, PrState::ReadyForReview);
        assert_eq!(t.action, Action::MarkReady);
    }

    /// A conflicted branch's checks describe a merge that cannot happen, so the
    /// conflict is addressed before CI is believed either way.
    #[test]
    fn a_conflict_outranks_ci() {
        let conflicted = Observation {
            ci: CiConclusion::Red,
            base_ci: CiConclusion::Green,
            mergeable: Mergeability::Conflicted,
            ..obs()
        };

        let t = deliver_next(
            PrState::CiPending,
            &conflicted,
            Attempts::default(),
            &DeliverPolicy::default(),
        );

        assert_eq!(t.state, PrState::Conflicted);
        assert_eq!(t.action, Action::Rebase);
    }

    #[test]
    fn pending_ci_waits() {
        let pending = Observation {
            ci: CiConclusion::Pending,
            ..obs()
        };
        let t = deliver_next(
            PrState::Draft,
            &pending,
            Attempts::default(),
            &DeliverPolicy::default(),
        );
        assert_eq!(t.state, PrState::CiPending);
        assert_eq!(t.action, Action::Wait);
    }

    #[test]
    fn a_transition_round_trips_through_json() {
        let t = Transition {
            state: PrState::Escalated,
            action: Action::Escalate {
                reason: EscalationReason::RebaseCeilingReached,
            },
        };
        let json = serde_json::to_string(&t).expect("serialize");
        let back: Transition = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(t, back);
    }
}
