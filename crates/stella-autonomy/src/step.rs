//! "What now" — the loop's top-level move, decided deterministically.
//!
//! `doc:backlog-self-driving` §3.7. The policy loop that drives a cycle needs
//! somewhere deterministic to ask what to do next, or every host re-derives the
//! cycle and they drift — which is the exact failure this crate was extracted
//! to end (#1613: the Observatory and `stella self-driving metrics` carried two
//! `fold_runs` and disagreed about whether the loop was `NOISY`).
//!
//! So the step function is pure and lives beside the governor, and a host —
//! the shell driver today, the plugin after B2 — asks it rather than deciding.
//!
//! # Blocking is a first-class outcome, and it clears itself
//!
//! **A loop that cannot stop is not autonomous, it is unsupervised**, and those
//! are different things. [`LoopStep::Blocked`] is reachable for a spent budget,
//! a revoked grant, an operator stop, and an [`PrDisposition::Escalated`] PR
//! the policy declines to abandon. It is tested before anything else, so no
//! amount of available work can outvote a stop.
//!
//! **But a block is a park, not a death.** The loop resumes the moment the
//! thing blocking it clears, and *nobody has to tell it that it may*. There is
//! no resume input, no acknowledgement, and no operator gesture in the
//! contract: a human who raises the ceiling, restores the grant, drops the stop
//! flag, or merges the escalated PR has already said everything that needs
//! saying, and asking them to also say "go" is asking twice.
//!
//! This is enforced structurally rather than promised: **the machine holds no
//! latch.** [`step`] is a total function of the current state and the current
//! observation, so a block is recomputed from scratch on every poll and simply
//! stops being returned when its cause is gone. There is no `blocked` flag that
//! could be left set. `a_cleared_block_resumes_with_no_resume_signal` is the
//! witness.
//!
//! The one obligation this puts on a host is the one thing the machine cannot
//! do for it: **keep polling.** A host that exits the process on
//! [`LoopStep::Blocked`] makes resumption impossible no matter what this
//! function returns, which is why the variant carries a [`Clearance`] naming
//! the observable to watch instead of reading as a terminal state.
//!
//! # Finishing outranks starting
//!
//! The precedence below puts `Deliver` above `Claim` deliberately. A loop that
//! claims whenever the queue is non-empty accumulates open PRs it never drives
//! green, and the backlog it is measured against grows by its own work in
//! flight. Finish first.
//!
//! # No workspace dependency, deliberately
//!
//! The design sketch writes `IssueKey` and `PrId`, which live in
//! `stella-protocol`. This crate depends on no workspace crate — that is what
//! lets `stella-observatory` link its folds without linking the machinery it
//! observes — so the references here are local newtypes and the caller maps.
//! Exactly the trade [`crate::rank_defects`] already makes with
//! [`crate::QueueIssue`].

use serde::{Deserialize, Serialize};

use crate::doctrine::{
    Contention, ContentionVerdict, Doctrine, ForeignBreakage, contention_verdict,
};

/// A tracker's issue key, as the tracker spells it.
///
/// Opaque: produced by a caller, compared for equality, handed back. See the
/// module docs for why this is not `stella_protocol::IssueKey`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IssueRef(pub String);

/// A forge's pull-request identifier, as the forge spells it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PrRef(pub String);

/// Why the loop is parked.
///
/// Typed because a human reading a block needs to know whether to add money,
/// re-grant, drop a stop flag, or go look at a pull request — four different
/// next moves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "block")]
pub enum BlockReason {
    /// The cycle's spend ceiling was reached. Budget is the only hard bound.
    BudgetSpent,
    /// The `[driver]` grant this loop runs under is no longer valid.
    ///
    /// Checked every step rather than at start-up: a grant revoked mid-cycle
    /// must stop the loop at the next boundary, not at the next restart — and
    /// a grant restored mid-park must resume it at the next poll.
    GrantRevoked,
    /// A human asked it to stop.
    OperatorStop,
    /// A pull request escalated to a human and policy declines to abandon it.
    ///
    /// Carries the reference, because "something escalated" is not actionable
    /// and "PR 412 escalated" is.
    EscalatedPr {
        /// Which one.
        pr: PrRef,
    },
}

/// What would unblock the loop — the observable a parked host watches.
///
/// Every variant is a condition the loop can detect for itself. That is the
/// point: **none of them is an operator telling it to resume.** A human who
/// clears the cause has already said everything that needs saying.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "clears_when")]
pub enum Clearance {
    /// The stop flag is no longer set.
    StopFlagDropped,
    /// The `[driver]` grant validates again.
    GrantRestored,
    /// A budget axis has room again — a refreshed window or a raised ceiling.
    BudgetAvailable,
    /// That pull request reached a settled disposition, by any hand.
    PrSettled {
        /// Which one to watch.
        pr: PrRef,
    },
}

impl BlockReason {
    /// The observable that would clear this block.
    ///
    /// Total by construction, so a new [`BlockReason`] cannot be added without
    /// answering "and what would un-block it?" — a block with no clearance is a
    /// block that needs a human to say "go", which is the thing this design
    /// refuses.
    #[must_use]
    pub fn clearance(&self) -> Clearance {
        match self {
            Self::BudgetSpent => Clearance::BudgetAvailable,
            Self::GrantRevoked => Clearance::GrantRestored,
            Self::OperatorStop => Clearance::StopFlagDropped,
            Self::EscalatedPr { pr } => Clearance::PrSettled { pr: pr.clone() },
        }
    }
}

/// What the loop is waiting for when there is nothing to do.
///
/// Watch is not sleep: each variant names an event a cheap sentinel can
/// detect, so a watching loop costs nothing until the world changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeCondition {
    /// The base branch moved, so a dry lens may not be dry any more (§4.2).
    BaseMoved,
    /// Somebody filed something.
    DefectFiled,
    /// A check on a PR this loop owns concluded.
    CiChanged,
}

/// The one move to make next.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "step")]
pub enum LoopStep {
    /// Size the cycle to the machine it is running on.
    Plan,
    /// Draw from the ranked queue, up to the sized batch.
    Claim {
        /// How many the plan permits this cycle.
        batch: u32,
    },
    /// Run one issue through Stella's turn loop.
    Work {
        /// Which issue.
        issue: IssueRef,
    },
    /// Advance one open pull request by one deterministic transition.
    Deliver {
        /// Which pull request.
        pr: PrRef,
    },
    /// Run the open lens's tooling and emit what has not been seen.
    Sweep {
        /// Which lens is open.
        lens: String,
    },
    /// Emit proposals from ledger evidence.
    Curate,
    /// Act on the thing blocking the loop, rather than waiting for someone else
    /// to.
    ///
    /// The loop exhausts the channels it is permitted before it parks: a
    /// blockage it can make visible, it files; a blockage its doctrine lets it
    /// adopt, it fixes. Parking is what is left when neither applies.
    Unblock {
        /// Which remedy.
        attempt: UnblockAttempt,
    },
    /// Nothing to do until the world changes.
    Watch {
        /// What would make it worth waking.
        until: WakeCondition,
    },
    /// Parked. **Not terminal** — see the module docs.
    ///
    /// A host keeps polling and resumes automatically when `clears_when` is
    /// satisfied. Exiting the process here is the one way to break the
    /// self-resume property, because a machine that is never asked cannot
    /// answer.
    Blocked {
        /// Why it parked.
        reason: BlockReason,
        /// The observable that will let it go again, with no operator gesture.
        clears_when: Clearance,
    },
}

/// A remedy the loop may apply to its own blockage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "attempt")]
pub enum UnblockAttempt {
    /// File the base breakage so it is visible and attributable.
    ///
    /// Happens under every policy that files at all, and **before** any attempt
    /// to fix it, because the ticket is the part that survives a failed fix.
    FileBaseBreakage,
    /// Fix breakage this loop did not cause.
    ///
    /// Only under [`ForeignBreakage::FileAndAdopt`], only once filed, and only
    /// when nothing else appears to be on it.
    AdoptBaseBreakage {
        /// The issue to work, so the fix is attributable to the filing.
        issue: IssueRef,
    },
    /// Somebody else is already fixing it.
    ///
    /// Not a no-op: it is a *recorded* decision not to duplicate work, carrying
    /// what was seen. A loop that silently declined would be indistinguishable
    /// from one that never looked.
    DeferToExistingFix {
        /// The signals that caused the deferral.
        evidence: Vec<String>,
    },
}

/// Where one pull request the loop owns has got to.
///
/// A projection of [`crate::PrState`] onto the only question the top-level
/// machine asks: is this one finished, stuck, or still moving? Keeping it
/// separate means a new `PrState` variant does not silently change what the
/// loop does with it — the mapping is a caller's explicit choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrDisposition {
    /// Still has transitions available.
    Moving,
    /// Handed to a human.
    Escalated,
    /// Landed or abandoned; nothing left to do.
    Settled,
}

/// One pull request the loop is carrying.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarriedPr {
    /// Which one.
    pub pr: PrRef,
    /// Where it has got to.
    pub disposition: PrDisposition,
}

/// The loop's own durable state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopState {
    /// Whether this cycle has been sized yet.
    #[serde(default)]
    pub planned: bool,
    /// How many issues the plan permits this cycle.
    #[serde(default)]
    pub batch: u32,
    /// Issues claimed but not yet carried to a pull request.
    #[serde(default)]
    pub claimed: Vec<IssueRef>,
    /// Pull requests this loop opened and has not settled.
    #[serde(default)]
    pub carrying: Vec<CarriedPr>,
    /// The aperture lens currently open, if the ladder has not been exhausted.
    #[serde(default)]
    pub lens: Option<String>,
    /// Proposals waiting for a human.
    #[serde(default)]
    pub pending_proposals: u32,
}

/// What the world looks like right now. Facts only — every policy question the
/// machine asks is answered by [`Doctrine`], never by this type.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopObservation {
    /// Whether any budget axis is exhausted.
    ///
    /// A bool rather than an amount: which axis and how much is the governor's
    /// arithmetic, and this machine only needs to know whether it may spend.
    pub budget_exhausted: bool,
    /// Whether the `[driver]` grant is still valid.
    ///
    /// Note the polarity: `false` blocks. [`Default`] therefore yields an
    /// *invalid* grant, which is the safe direction — a caller that forgets to
    /// set this gets a parked loop, not an ungoverned one.
    pub grant_valid: bool,
    /// Whether a human has asked it to stop.
    pub stop_requested: bool,
    /// How many issues the ranked queue is offering.
    pub queue_depth: u32,
    /// Whether the base branch is currently broken.
    ///
    /// A red base blocks *everyone*, and it is the blockage a loop is most
    /// likely to meet that it did not cause. §6.5's `foreign_breakage` decides
    /// what to do about it.
    pub base_broken: bool,
    /// Whether the base breakage already has an issue filed against it.
    ///
    /// Filing is separate from fixing on purpose: the ticket is what makes the
    /// breakage visible and attributable **even if the fix never lands**, so it
    /// happens first and under every policy that files at all.
    pub base_breakage_filed: bool,
    /// The issue tracking the base breakage, once filed.
    pub base_breakage_issue: Option<IssueRef>,
    /// What else appears to be fixing that breakage already.
    ///
    /// Gathered from the forge and from **this machine** — the local worktrees
    /// are the signal nobody remembers to check, and two self-driving sessions
    /// against one clone see each other only there.
    #[serde(default)]
    pub base_fix_contention: Contention,
}

/// The loop's next move.
///
/// Total over its inputs, with no clock, no randomness and no I/O, so the same
/// state and observation always yield the same move. That is what makes a
/// perpetual loop's behaviour reviewable after the fact.
///
/// # Precedence
///
/// The order **is** the policy, so it is stated rather than reverse-engineered:
///
/// 1. **A stop outranks everything.** Operator stop, revoked grant, spent
///    budget — checked before any available work, so no amount of work can
///    outvote a stop. Each is recomputed rather than latched, so it stops
///    applying the moment its cause is gone.
/// 2. **An escalated PR parks the loop**, unless doctrine says to abandon it.
/// 3. **Plan before acting**, so a cycle is sized to its machine first.
/// 4. **Unblock before working.** A broken base makes every PR the loop
///    produces fail CI, so it is dealt with before more are made — filed first,
///    then adopted if doctrine permits and nothing else is on it.
/// 5. **Finishing outranks starting** — deliver a carried PR before claiming
///    anything new. See the module docs.
/// 6. **Work what is already claimed** before claiming more.
/// 7. **Claim** while the queue offers something and the batch has room.
/// 8. **Sweep** when the queue is dry and a lens is still open.
/// 9. **Curate** when proposals have accumulated and nothing else is pressing.
/// 10. **Watch** otherwise — the ladder is exhausted and the queue is empty.
#[must_use]
pub fn step(state: &LoopState, obs: &LoopObservation, doctrine: &Doctrine) -> LoopStep {
    // 1. A stop outranks everything. Recomputed every call and never latched,
    //    so each of these stops being returned the moment its cause is gone.
    if obs.stop_requested {
        return blocked(BlockReason::OperatorStop);
    }
    if !obs.grant_valid {
        return blocked(BlockReason::GrantRevoked);
    }
    if obs.budget_exhausted {
        return blocked(BlockReason::BudgetSpent);
    }

    // 2. An escalated PR parks the loop unless doctrine abandons it.
    if !doctrine.abandon_escalated
        && let Some(carried) = state
            .carrying
            .iter()
            .find(|c| c.disposition == PrDisposition::Escalated)
    {
        return blocked(BlockReason::EscalatedPr {
            pr: carried.pr.clone(),
        });
    }

    // 3. Size the cycle before acting in it.
    if !state.planned {
        return LoopStep::Plan;
    }

    // 4. Exhaust the permitted channels on the loop's own blockage before
    //    making more work that the same blockage will stall.
    if obs.base_broken
        && let Some(attempt) = unblock_base(obs, doctrine)
    {
        return LoopStep::Unblock { attempt };
    }

    // 5. Finishing outranks starting.
    if let Some(carried) = state
        .carrying
        .iter()
        .find(|c| c.disposition == PrDisposition::Moving)
    {
        return LoopStep::Deliver {
            pr: carried.pr.clone(),
        };
    }

    // 5. Work what is already claimed before claiming more.
    if let Some(issue) = state.claimed.first() {
        return LoopStep::Work {
            issue: issue.clone(),
        };
    }

    // 6. Claim, while the queue offers and the batch has room.
    if obs.queue_depth > 0 && state.batch > 0 {
        return LoopStep::Claim { batch: state.batch };
    }

    // 7. The queue is dry. A lens still open is where the next question comes
    //    from (§4.1: only the queue supply terminates).
    if let Some(lens) = &state.lens {
        return LoopStep::Sweep { lens: lens.clone() };
    }

    // 8. Nothing to fix and nothing to look for — spend the idle step on
    //    proposals that have already accumulated evidence.
    if state.pending_proposals > 0 {
        return LoopStep::Curate;
    }

    // 9. The ladder is exhausted and the queue is empty. Wake when the base
    //    moves, because a lens dry at commit X says nothing about X+200 (§4.2).
    LoopStep::Watch {
        until: WakeCondition::BaseMoved,
    }
}

/// What, if anything, the loop may do about a broken base right now.
///
/// `None` means "nothing left to try" — the breakage is filed, doctrine does
/// not permit adopting it, or someone else has it — and the caller carries on
/// with ordinary work rather than parking, because a red base stops merges, not
/// authoring.
///
/// The order is the argument:
///
/// 1. **File first, always.** The ticket is what makes the breakage visible and
///    attributable *even if the fix never lands*, so it is never contingent on
///    the fix being attempted.
/// 2. **Then check contention**, before adopting. Two workers on one broken
///    `main` produce a merge conflict on top of the outage.
/// 3. **Then adopt**, only if doctrine says to.
fn unblock_base(obs: &LoopObservation, doctrine: &Doctrine) -> Option<UnblockAttempt> {
    if doctrine.foreign_breakage == ForeignBreakage::Ignore {
        return None;
    }

    // 1. Make it visible first.
    if !obs.base_breakage_filed {
        return Some(UnblockAttempt::FileBaseBreakage);
    }

    if doctrine.foreign_breakage != ForeignBreakage::FileAndAdopt {
        return None;
    }

    // 2. Do not duplicate a fix already in flight.
    if let ContentionVerdict::Defer { evidence } =
        contention_verdict(doctrine.contention, &obs.base_fix_contention)
    {
        return Some(UnblockAttempt::DeferToExistingFix { evidence });
    }

    // 3. Adopt it, attributed to the filing.
    obs.base_breakage_issue
        .clone()
        .map(|issue| UnblockAttempt::AdoptBaseBreakage { issue })
}

fn blocked(reason: BlockReason) -> LoopStep {
    let clears_when = reason.clearance();
    LoopStep::Blocked {
        reason,
        clears_when,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs() -> LoopObservation {
        LoopObservation {
            grant_valid: true,
            queue_depth: 5,
            ..LoopObservation::default()
        }
    }

    /// A red base, already filed, with a doctrine that adopts it.
    fn base_red_filed() -> LoopObservation {
        LoopObservation {
            base_broken: true,
            base_breakage_filed: true,
            base_breakage_issue: Some(IssueRef("3912".into())),
            ..obs()
        }
    }

    fn adopting() -> Doctrine {
        Doctrine {
            foreign_breakage: ForeignBreakage::FileAndAdopt,
            ..Doctrine::default()
        }
    }

    /// Convenience for the many cases that use the conservative default.
    fn go(state: &LoopState, o: &LoopObservation) -> LoopStep {
        step(state, o, &Doctrine::default())
    }

    fn planned() -> LoopState {
        LoopState {
            planned: true,
            batch: 3,
            ..LoopState::default()
        }
    }

    /// The witness. A loop that cannot halt is not autonomous, it is
    /// unsupervised — so a stop is tested before any amount of available work,
    /// and work is deliberately available here in every form the machine knows.
    #[test]
    fn a_stop_outranks_every_kind_of_available_work() {
        let busy = LoopState {
            claimed: vec![IssueRef("1".into())],
            carrying: vec![CarriedPr {
                pr: PrRef("9".into()),
                disposition: PrDisposition::Moving,
            }],
            lens: Some("rubric".into()),
            pending_proposals: 4,
            ..planned()
        };

        for (label, o) in [
            (
                "operator stop",
                LoopObservation {
                    stop_requested: true,
                    ..obs()
                },
            ),
            (
                "revoked grant",
                LoopObservation {
                    grant_valid: false,
                    ..obs()
                },
            ),
            (
                "spent budget",
                LoopObservation {
                    budget_exhausted: true,
                    ..obs()
                },
            ),
        ] {
            assert!(
                matches!(go(&busy, &o), LoopStep::Blocked { .. }),
                "{label} must park a loop with work available in every form"
            );
        }
    }

    /// The self-resume witness. Every block clears **without any resume input**:
    /// the same state, observed after the cause is gone, yields work again. The
    /// machine holds no latch, so there is nothing an operator could have to
    /// acknowledge.
    ///
    /// Asserted for all four block reasons, because a latch on any one of them
    /// would strand the loop exactly there.
    #[test]
    fn a_cleared_block_resumes_with_no_resume_signal() {
        let carrying_escalated = LoopState {
            carrying: vec![CarriedPr {
                pr: PrRef("412".into()),
                disposition: PrDisposition::Escalated,
            }],
            ..planned()
        };

        // Each case: (label, the blocked observation, the state, the state once
        // a human cleared the cause — and nothing else changed).
        let cases: Vec<(&str, LoopObservation, LoopState, LoopState)> = vec![
            (
                "operator dropped the stop flag",
                LoopObservation {
                    stop_requested: true,
                    ..obs()
                },
                planned(),
                planned(),
            ),
            (
                "grant restored",
                LoopObservation {
                    grant_valid: false,
                    ..obs()
                },
                planned(),
                planned(),
            ),
            (
                "budget refreshed",
                LoopObservation {
                    budget_exhausted: true,
                    ..obs()
                },
                planned(),
                planned(),
            ),
            (
                "a human settled the escalated PR",
                obs(),
                carrying_escalated,
                planned(),
            ),
        ];

        for (label, blocked_obs, blocked_state, cleared_state) in cases {
            assert!(
                matches!(go(&blocked_state, &blocked_obs), LoopStep::Blocked { .. }),
                "{label}: expected a block first"
            );

            // The cause is gone. Nothing told the loop it may resume.
            assert_eq!(
                go(&cleared_state, &obs()),
                LoopStep::Claim { batch: 3 },
                "{label}: must resume on its own, with no resume signal"
            );
        }
    }

    /// A grant revoked mid-cycle stops the loop at the next boundary; a grant
    /// restored mid-park resumes it at the next poll.
    #[test]
    fn a_revoked_grant_names_itself_rather_than_looking_like_a_stop() {
        let o = LoopObservation {
            grant_valid: false,
            ..obs()
        };
        assert_eq!(
            go(&planned(), &o),
            LoopStep::Blocked {
                reason: BlockReason::GrantRevoked,
                clears_when: Clearance::GrantRestored,
            }
        );
    }

    /// "Something escalated" is not actionable; "PR 412 escalated" is — and the
    /// clearance names the same PR, so a parked host knows exactly what to watch.
    #[test]
    fn an_escalated_pr_parks_and_names_which_one_to_watch() {
        let stuck = LoopState {
            carrying: vec![CarriedPr {
                pr: PrRef("412".into()),
                disposition: PrDisposition::Escalated,
            }],
            ..planned()
        };

        assert_eq!(
            go(&stuck, &obs()),
            LoopStep::Blocked {
                reason: BlockReason::EscalatedPr {
                    pr: PrRef("412".into())
                },
                clears_when: Clearance::PrSettled {
                    pr: PrRef("412".into())
                },
            }
        );
    }

    /// Every block names an observable that clears it. A block with no
    /// clearance would be one needing a human to say "go".
    #[test]
    fn every_block_reason_names_a_clearance() {
        for reason in [
            BlockReason::BudgetSpent,
            BlockReason::GrantRevoked,
            BlockReason::OperatorStop,
            BlockReason::EscalatedPr {
                pr: PrRef("1".into()),
            },
        ] {
            let clearance = reason.clearance();
            assert!(
                !matches!(
                    (&reason, &clearance),
                    (BlockReason::EscalatedPr { pr }, Clearance::PrSettled { pr: watched })
                        if pr != watched
                ),
                "{reason:?} must watch the PR it named, got {clearance:?}"
            );
        }
    }

    /// ...unless the operator said to keep going past one.
    #[test]
    fn policy_can_abandon_an_escalated_pr_instead_of_parking() {
        let stuck = LoopState {
            carrying: vec![CarriedPr {
                pr: PrRef("412".into()),
                disposition: PrDisposition::Escalated,
            }],
            ..planned()
        };
        let abandoning = Doctrine {
            abandon_escalated: true,
            ..Doctrine::default()
        };

        assert_eq!(
            step(&stuck, &obs(), &abandoning),
            LoopStep::Claim { batch: 3 }
        );
    }

    /// The blockage-resolution witness. A base the loop did not break is filed
    /// first — under every doctrine that files at all — because the ticket is
    /// what makes the breakage visible and attributable even if no fix lands.
    #[test]
    fn a_base_the_loop_did_not_break_is_filed_before_anything_else() {
        let red = LoopObservation {
            base_broken: true,
            ..obs()
        };

        for doctrine in [Doctrine::default(), adopting()] {
            assert_eq!(
                step(&planned(), &red, &doctrine),
                LoopStep::Unblock {
                    attempt: UnblockAttempt::FileBaseBreakage
                },
                "{:?} must make the breakage visible first",
                doctrine.foreign_breakage
            );
        }
    }

    /// The other half: with the breakage filed, doctrine decides whether the
    /// loop fixes somebody else's code. Both answers are legitimate, which is
    /// exactly why it is configuration and not a hardcoded rule.
    #[test]
    fn doctrine_decides_whether_foreign_breakage_is_adopted() {
        // FileAndAdopt: a red base blocks this loop as surely as its author.
        assert_eq!(
            step(&planned(), &base_red_filed(), &adopting()),
            LoopStep::Unblock {
                attempt: UnblockAttempt::AdoptBaseBreakage {
                    issue: IssueRef("3912".into())
                }
            }
        );

        // FileAndWait: filed already, so nothing more to try — and the loop
        // carries on with ordinary work rather than parking, because a red base
        // stops merges, not authoring.
        assert_eq!(
            step(&planned(), &base_red_filed(), &Doctrine::default()),
            LoopStep::Claim { batch: 3 }
        );
    }

    /// The collision witness. A fix already in flight — including one in a
    /// worktree on this very machine — defers instead of duplicating, and the
    /// deferral carries what was seen so it is reviewable.
    #[test]
    fn an_adopted_fix_defers_to_one_already_in_flight() {
        let contended = LoopObservation {
            base_fix_contention: Contention {
                local_worktrees: vec!["fix-ci-3915".into()],
                ..Contention::default()
            },
            ..base_red_filed()
        };

        assert_eq!(
            step(&planned(), &contended, &adopting()),
            LoopStep::Unblock {
                attempt: UnblockAttempt::DeferToExistingFix {
                    evidence: vec!["local worktree: fix-ci-3915".into()]
                }
            },
            "two workers on one broken main produce a conflict on top of an outage"
        );
    }

    /// `Ignore` is reachable and does nothing — present for the operator who
    /// wants a silent loop, and deliberately not the default.
    #[test]
    fn ignore_neither_files_nor_fixes() {
        let red = LoopObservation {
            base_broken: true,
            ..obs()
        };
        let silent = Doctrine {
            foreign_breakage: ForeignBreakage::Ignore,
            ..Doctrine::default()
        };

        assert_eq!(
            step(&planned(), &red, &silent),
            LoopStep::Claim { batch: 3 }
        );
    }

    #[test]
    fn an_unplanned_cycle_is_sized_before_it_acts() {
        let fresh = LoopState {
            claimed: vec![IssueRef("1".into())],
            ..LoopState::default()
        };
        assert_eq!(go(&fresh, &obs()), LoopStep::Plan);
    }

    /// A loop that claims whenever the queue is non-empty accumulates PRs it
    /// never drives green, and the backlog grows by its own work in flight.
    #[test]
    fn delivering_a_carried_pr_outranks_claiming_more_work() {
        let carrying = LoopState {
            carrying: vec![CarriedPr {
                pr: PrRef("77".into()),
                disposition: PrDisposition::Moving,
            }],
            ..planned()
        };

        assert_eq!(
            go(&carrying, &obs()),
            LoopStep::Deliver {
                pr: PrRef("77".into())
            },
            "a queue of 5 must not outrank a PR already in flight"
        );
    }

    #[test]
    fn a_claimed_issue_is_worked_before_more_are_claimed() {
        let claimed = LoopState {
            claimed: vec![IssueRef("3210".into())],
            ..planned()
        };
        assert_eq!(
            go(&claimed, &obs()),
            LoopStep::Work {
                issue: IssueRef("3210".into())
            }
        );
    }

    #[test]
    fn a_settled_pr_does_not_hold_the_loop() {
        let settled = LoopState {
            carrying: vec![CarriedPr {
                pr: PrRef("77".into()),
                disposition: PrDisposition::Settled,
            }],
            ..planned()
        };
        assert_eq!(go(&settled, &obs()), LoopStep::Claim { batch: 3 });
    }

    /// §4.1: only the queue supply terminates. A dry queue is where the sweep
    /// starts, not where the loop stops.
    #[test]
    fn a_dry_queue_sweeps_the_open_lens_rather_than_stopping() {
        let dry = LoopState {
            lens: Some("concurrency".into()),
            ..planned()
        };
        let o = LoopObservation {
            queue_depth: 0,
            ..obs()
        };

        assert_eq!(
            go(&dry, &o),
            LoopStep::Sweep {
                lens: "concurrency".into()
            }
        );
    }

    #[test]
    fn proposals_are_curated_only_once_nothing_else_is_pressing() {
        let idle = LoopState {
            pending_proposals: 2,
            ..planned()
        };
        let o = LoopObservation {
            queue_depth: 0,
            ..obs()
        };
        assert_eq!(go(&idle, &o), LoopStep::Curate);

        // ...and not while the queue still offers work.
        assert_eq!(go(&idle, &obs()), LoopStep::Claim { batch: 3 });
    }

    /// The exhausted ladder wakes on a moved base, because a lens dry at commit
    /// X says nothing about commit X+200.
    #[test]
    fn an_exhausted_ladder_and_an_empty_queue_watch_the_base() {
        let o = LoopObservation {
            queue_depth: 0,
            ..obs()
        };
        assert_eq!(
            go(&planned(), &o),
            LoopStep::Watch {
                until: WakeCondition::BaseMoved
            }
        );
    }

    /// A batch of zero is a plan that permits nothing, and must not be read as
    /// permission to claim.
    #[test]
    fn a_zero_batch_does_not_claim() {
        let no_room = LoopState {
            batch: 0,
            lens: Some("rubric".into()),
            ..planned()
        };
        assert_eq!(
            go(&no_room, &obs()),
            LoopStep::Sweep {
                lens: "rubric".into()
            }
        );
    }

    #[test]
    fn a_step_round_trips_through_json() {
        let s = LoopStep::Blocked {
            reason: BlockReason::EscalatedPr {
                pr: PrRef("412".into()),
            },
            clears_when: Clearance::PrSettled {
                pr: PrRef("412".into()),
            },
        };
        let json = serde_json::to_string(&s).expect("serialize");
        let back: LoopStep = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(s, back);
    }
}
