//! Whether the winning candidate's work is delivered into the real tree —
//! and why that question is not the verdict's to answer.
//!
//! # The rule
//!
//! **A verdict is a claim about the work; it is not what decides whether the
//! work exists.** This is the same principle #2881 established one layer down
//! (a post-completion *check* may downgrade a claim, never discard a
//! deliverable), applied at the delivery gate itself. Adoption used to require
//! `verdict.passed`, so an unproven or failed run had its entire candidate
//! workspace deleted — which makes the verdict self-fulfilling: nothing
//! reaches the tree, so nothing can ever be judged correct there.
//!
//! Measured cost of the old gate (#2927): over the twenty pipeline trials of
//! ArenaBench run `w10p`, every trial with `passed=true` wrote to the graded
//! tree after its verdict and *no* trial without one did. Eight of fifteen
//! pipeline losses never had their work delivered at all, including a trial
//! whose last recorded event, four seconds before the wall-clock kill, was
//! `18 passed in 1.44s`.
//!
//! # Why "deliver anyway" is the correct posture, rung by rung
//!
//! The decisive argument is not the bench number, it is that **isolation is a
//! mechanism, not a quality gate.** A candidate workspace exists so that
//! losing candidates can be rolled back and so a witness author cannot reach
//! the user's tree. With isolation switched off, a run that fails
//! verification still leaves its edits in the working tree for the operator to
//! read, keep, or `git checkout`. Turning isolation *on* must not make the
//! same run strictly worse off. `pipeline.rs` already reached that conclusion
//! for one caller (the `create_worktrees` single-shot path keeps its
//! unadopted snapshot and says where it is); this module reaches it for the
//! rest.
//!
//! Per [`crate::verify::LadderDecision`]:
//!
//! * `SubmitFast` — delivered, as before.
//! * `Unverifiable`, `Unverified`, `WitnessUnsatisfiable` — these publish
//!   `passed: true` (a run is not failed by the absence of a way to check it),
//!   so they were already delivered. Named here because the *claim* they carry
//!   is unproven and stays unproven: delivery adds no proof.
//! * `NothingAttempted` — delivered. The rung's own premise is that no channel
//!   saw anything change, so the patch is empty and adoption is a no-op.
//!   Excluding it would be a branch that can only ever be wrong.
//! * `Revise` (terminal, `passed: false`) — delivered, and this is the real
//!   change. A red touched test is a deterministic fact about **one
//!   instrument**, not a finding that the whole candidate is worthless, and a
//!   *terminal* `Revise` means the repair budget ran out mid-repair, so the
//!   candidate is the best state the run reached. The alternative delivers
//!   zero bytes with certainty.
//! * No verdict at all, because the worker's engine turn aborted (budget
//!   exhausted, stuck loop, step cap, a user's stop) — delivered. This is
//!   #2651's shape: the stop is a decision about *spending*, taken between
//!   tool calls, and it says nothing whatever about the bytes already written.
//!
//! # What is still withheld, and why that is a different question
//!
//! An **integrity** refusal is not a claim about the work's quality; it is a
//! statement that the bytes in the workspace are not the bytes any stage
//! examined. Delivering those would ship something nothing ever looked at —
//! strictly worse than shipping nothing, and the one case where "deliver
//! anyway" is wrong. These keep the discard they already had: a candidate that
//! wrote through its isolation (#1538), a witness artifact mutated after its
//! baseline was pinned, a worktree that changed after verification, a seal
//! that could not be taken, and a mutation audit that could not restore the
//! tree. Every one of them is raised by the pipeline itself, never by the
//! worker's engine, which is exactly the distinction
//! [`AbortFacts::from_turn`] carries.
//!
//! # Honesty
//!
//! Delivery never implies proof. The verdict, the `PipelineStatus`, and the
//! candidate's score are untouched by this module — a delivered `Revise` still
//! reports `VerificationFailed`, and a delivered abstention still reports
//! `Unverified`. [`Delivery::Deliver::proven`] exists only so the caller can
//! say on the rail that what it just wrote is unproven.

use stella_core::AbortKind;
use stella_protocol::{AgentEvent, DeliveryDecline, DeliveryOutcome, FileChangeKind};

use super::Pipeline;
use super::candidate_result::CandidateResult;
use crate::ports::{AdoptedChange, CandidateWorkspace, WorkspaceError};

/// What the pipeline knows about the winning candidate at the moment it
/// decides delivery. Owned plain data, so [`decide`] is a pure function of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct WinnerFacts {
    /// `Some(passed)` when verification reached a verdict, `None` when the
    /// candidate never got one (it aborted, or it was a lookup with nothing to
    /// verify).
    pub(super) verdict_passed: Option<bool>,
    /// `Some` when the candidate aborted.
    pub(super) abort: Option<AbortFacts>,
}

/// The three things about an abort that decide whether its work survives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AbortFacts {
    /// The abort came out of the worker's own engine turn — a budget stop, a
    /// stuck-loop escalation, a step cap, a user's stop. The engine stops
    /// *between* tool calls (invariant #6), so the workspace holds exactly the
    /// bytes the last completed tool wrote.
    pub(super) from_turn: bool,
    /// A degradable **infrastructure** setup failure: no workspace was ever
    /// created and no model call was ever made, so there is nothing to
    /// deliver.
    pub(super) degradable: bool,
    /// Whether this stop was a decision or a failure.
    pub(super) kind: AbortKind,
}

/// What to do with the winner's workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Delivery {
    /// Adopt the candidate's work into the real tree.
    Deliver {
        /// `false` when the run reached no passing verdict. The work is
        /// delivered all the same; the caller says so on the rail rather than
        /// letting the write read as a proof.
        proven: bool,
    },
    /// Withhold the work: the candidate's own integrity is what failed, or it
    /// never produced any.
    Withhold {
        /// Which of the two it was. Typed rather than a bare `Withhold`,
        /// because the decision is now reported on the wire (#2942) and a
        /// reader that has to tell an integrity refusal from "there was never
        /// a workspace" cannot get there from a unit variant.
        ///
        /// [`DeliveryDecline`] is the protocol's own type, reused rather than
        /// mirrored: one definition cannot drift from its projection, and a
        /// private twin plus a mapping function is two places to change.
        reason: DeliveryDecline,
    },
}

/// The rail line that keeps an unproven delivery honest. Fixed text over no
/// model output, like every other deterministic notice on this surface.
pub(super) const UNPROVEN_DELIVERY_NOTICE: &str = "this run did not prove its changes, so they are UNPROVEN — but the work was still \
     delivered into your working tree rather than discarded; review it (`git diff`) before \
     relying on it";

/// The delivery decision for the winning candidate. Pure.
pub(super) fn decide(facts: WinnerFacts) -> Delivery {
    match facts.abort {
        // Nothing was ever dispatched: the isolation port was missing or the
        // tree could not be snapshotted. There is no candidate to adopt.
        Some(abort) if abort.degradable => Delivery::Withhold {
            reason: DeliveryDecline::NothingCreated,
        },
        // The pipeline refused this candidate on an integrity finding. The
        // workspace no longer agrees with anything that was examined, so its
        // bytes are not a deliverable — see the module doc.
        Some(abort) if !abort.from_turn && abort.kind == AbortKind::Failure => Delivery::Withhold {
            reason: DeliveryDecline::IntegrityRefusal,
        },
        // Every other abort — the engine stopping the worker's turn, and a
        // pipeline-side *deliberate* stop — is a decision about spending, not
        // a finding about the tree. The work stands; the claim does not.
        Some(_) => Delivery::Deliver { proven: false },
        None => Delivery::Deliver {
            proven: facts.verdict_passed == Some(true),
        },
    }
}

impl WinnerFacts {
    /// Read the facts off the winning candidate.
    pub(super) fn of(result: &CandidateResult) -> Self {
        Self {
            verdict_passed: result.verdict.as_ref().map(|verdict| verdict.passed),
            abort: result.aborted.as_ref().map(|abort| AbortFacts {
                from_turn: abort.from_turn,
                degradable: result.degradable,
                kind: abort.kind,
            }),
        }
    }
}

/// The wire projection of a completed adoption. Pure over the rows adoption
/// itself measured, so the counts on the stream and the `FileChange` rows
/// beside them can never disagree — they are the same list, read twice.
fn delivered(adopted: &[AdoptedChange], proven: bool) -> DeliveryOutcome {
    let mut created = 0;
    let mut modified = 0;
    let mut deleted = 0;
    let mut lines_added = 0u64;
    let mut lines_removed = 0u64;
    for change in adopted {
        match change.kind {
            FileChangeKind::Created => created += 1,
            FileChangeKind::Modified => modified += 1,
            FileChangeKind::Deleted => deleted += 1,
            // Adoption reports what a patch DID, and a patch cannot read.
            // Counted nowhere rather than folded into `modified`, so an
            // impossible row stays visibly absent instead of inflating a real
            // number.
            FileChangeKind::Read => {}
        }
        lines_added += u64::from(change.added);
        lines_removed += u64::from(change.removed);
    }
    DeliveryOutcome::Delivered {
        created,
        modified,
        deleted,
        lines_added,
        lines_removed,
        proven,
    }
}

impl Pipeline<'_> {
    /// Deliver the winning candidate's work, clean up every workspace, and say
    /// on the stream what was decided and why (#2942).
    ///
    /// Lives here rather than inline in `pipeline.rs` for two reasons that
    /// point the same way: the decision this executes is defined in this
    /// module, and `pipeline.rs` is a god file closed to growth — new logic
    /// lands in a sibling submodule (AGENTS.md § God files).
    ///
    /// Returns the adoption failure, if any, for the caller to fold into the
    /// winner's abort. Deliberately returned rather than applied here: the
    /// caller owns the `CandidateResult` and this method owns the workspaces,
    /// and keeping that split is what lets this be tested through the port
    /// alone.
    pub(super) async fn deliver_winner(
        &self,
        workspaces: Vec<Result<Box<dyn CandidateWorkspace>, String>>,
        best_idx: usize,
        delivery: Delivery,
        witness_paths: &[String],
        single_shot_isolation: bool,
    ) -> Option<WorkspaceError> {
        let mut adopt_failure: Option<WorkspaceError> = None;
        // The winner's root, before the loop consumes the slots. `None` is a
        // real answer — a candidate whose workspace was never created has no
        // path to name — and is why the event's `root` is optional.
        let winner_root = workspaces
            .get(best_idx)
            .and_then(|slot| slot.as_ref().ok())
            .map(|ws| ws.root().to_string());
        // Said once, up front, for the arms that never reach an adoption: a
        // withheld candidate emits nothing else, so without this the stream
        // would carry the same silence as an empty delivery — the exact
        // ambiguity this event exists to remove.
        if let Delivery::Withhold { reason } = delivery {
            self.emit(AgentEvent::CandidateDelivery {
                root: winner_root.clone(),
                delivery: DeliveryOutcome::Declined { reason },
            });
        }
        for (i, slot) in workspaces.into_iter().enumerate() {
            let Ok(ws) = slot else {
                continue;
            };
            if i == best_idx
                && let Delivery::Deliver { proven } = delivery
            {
                // Delivery is never proof. The verdict, the status and the
                // score are untouched by this module; this line is what stops
                // the write itself from reading as a pass.
                if !proven {
                    self.warn(UNPROVEN_DELIVERY_NOTICE.to_string());
                }
                // The witness has already done its whole job by now — it armed
                // the flip oracle and the flip was observed — so withholding it
                // cannot change the verdict, only what lands in the tree.
                let withhold: &[String] = if self.config.keep_witness {
                    &[]
                } else {
                    witness_paths
                };
                match ws.adopt(withhold).await {
                    Ok(adopted) => {
                        // The declared signal first, then the per-file rows it
                        // summarizes. Order is contract: a reader that sees the
                        // counts before the rows can tell a truncated stream
                        // (killed mid-burst) from a genuinely small delivery,
                        // which reading the rows alone cannot.
                        self.emit(AgentEvent::CandidateDelivery {
                            root: Some(ws.root().to_string()),
                            delivery: delivered(&adopted, proven),
                        });
                        // Surface the adopted changes on the event stream: the
                        // winner's edits happened inside the snapshot, so no
                        // FileChange was emitted for the real tree yet. The
                        // adoption measured them (git's numstat + patch), so
                        // these rows carry a real delta — they used to arrive
                        // with `diff: None` and no counts, which the Files tab
                        // rendered as `+0 -0` for every adopted file.
                        for change in adopted {
                            self.emit(AgentEvent::FileChange {
                                path: change.path,
                                kind: change.kind,
                                added: change.added,
                                removed: change.removed,
                                diff: change.diff,
                            });
                        }
                        ws.remove().await;
                    }
                    // Keep the workspace: the error names its path and the
                    // conflicting files; the winning work stays recoverable.
                    // A sanctioned delivery that git refused is its own decline
                    // reason, distinct from the two the decision can return —
                    // the pipeline meant to deliver and the tree said no.
                    Err(e) => {
                        self.emit(AgentEvent::CandidateDelivery {
                            root: Some(ws.root().to_string()),
                            delivery: DeliveryOutcome::Declined {
                                reason: DeliveryDecline::AdoptFailed,
                            },
                        });
                        adopt_failure = Some(e);
                    }
                }
            } else if single_shot_isolation {
                // The `create_worktrees` run whose work was withheld — since
                // #2927 that means an integrity refusal (the candidate wrote
                // through its isolation, its witness was tampered with, its
                // tree moved after verification) or an allowance it never
                // got, never merely "it did not verify". Discarding is right
                // for best-of-N — the operator asked for the best of several
                // and none was good, and the losers were never meant to
                // survive — and `witness_isolation`'s tests pin removal for a
                // poisoned authored-witness candidate.
                //
                // It is wrong for this third caller. That operator asked where
                // the work should *happen*, not that it be thrown away — and
                // without this they end up strictly worse off than with
                // isolation off, where a failed run at least leaves its
                // changes in the tree to look at. So keep the snapshot and
                // name it: the same posture as the adopt-failure arm just
                // above, and for the same reason — undelivered is not
                // worthless. The typed reason is on the stream above; the prose
                // here stays unspecific because this arm has several causes and
                // each was already stated by the stage that raised it.
                self.warn(format!(
                    "this run's changes were not adopted into your working tree — they are \
                     kept at {} for you to inspect or salvage",
                    ws.root()
                ));
            } else {
                ws.remove().await;
            }
        }
        adopt_failure
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aborted(from_turn: bool, degradable: bool, kind: AbortKind) -> WinnerFacts {
        WinnerFacts {
            verdict_passed: None,
            abort: Some(AbortFacts {
                from_turn,
                degradable,
                kind,
            }),
        }
    }

    #[test]
    fn a_passing_verdict_delivers_proven_work() {
        assert_eq!(
            decide(WinnerFacts {
                verdict_passed: Some(true),
                abort: None,
            }),
            Delivery::Deliver { proven: true }
        );
    }

    #[test]
    fn a_failed_verdict_still_delivers_the_work_it_judged() {
        assert_eq!(
            decide(WinnerFacts {
                verdict_passed: Some(false),
                abort: None,
            }),
            Delivery::Deliver { proven: false },
            "a red verdict is a claim about the work, not a licence to destroy it"
        );
    }

    #[test]
    fn a_candidate_that_never_reached_a_verdict_still_delivers() {
        assert_eq!(
            decide(WinnerFacts::default()),
            Delivery::Deliver { proven: false }
        );
    }

    #[test]
    fn a_turn_abort_delivers_what_the_worker_had_already_written() {
        for kind in [AbortKind::DeliberateStop, AbortKind::Failure] {
            assert_eq!(
                decide(aborted(true, false, kind)),
                Delivery::Deliver { proven: false },
                "the engine stops between tool calls; {kind:?} says nothing about the bytes"
            );
        }
    }

    #[test]
    fn a_pipeline_integrity_refusal_withholds_the_work() {
        assert_eq!(
            decide(aborted(false, false, AbortKind::Failure)),
            Delivery::Withhold {
                reason: DeliveryDecline::IntegrityRefusal,
            },
            "an escaped, tampered or drifted workspace is not the tree anything examined"
        );
    }

    #[test]
    fn a_pipeline_side_deliberate_stop_is_not_an_integrity_finding() {
        assert_eq!(
            decide(aborted(false, false, AbortKind::DeliberateStop)),
            Delivery::Deliver { proven: false }
        );
    }

    #[test]
    fn a_setup_failure_has_nothing_to_deliver() {
        assert_eq!(
            decide(aborted(false, true, AbortKind::Failure)),
            Delivery::Withhold {
                reason: DeliveryDecline::NothingCreated,
            },
            "a setup failure never created a workspace, which is a different \
             answer from refusing the one it created"
        );
    }

    /// The two withhold paths must not collapse into one reason. Without this,
    /// a future edit could hand both the same label and the wire would stop
    /// distinguishing "nothing was ever built" from "we refused what was" —
    /// the exact ambiguity #2942 exists to remove.
    #[test]
    fn the_two_withhold_paths_report_different_reasons() {
        let setup = decide(aborted(false, true, AbortKind::Failure));
        let integrity = decide(aborted(false, false, AbortKind::Failure));
        assert_ne!(setup, integrity);
    }

    /// The projection counts by kind and sums the line deltas — and reports an
    /// empty adoption as a delivery of nothing, never as a decline.
    #[test]
    fn the_projection_counts_what_adoption_measured() {
        let rows = [
            ("a", FileChangeKind::Created, 10, 0),
            ("b", FileChangeKind::Modified, 4, 7),
            ("c", FileChangeKind::Deleted, 0, 9),
            ("d", FileChangeKind::Modified, 1, 1),
        ]
        .map(|(path, kind, added, removed)| AdoptedChange {
            path: path.into(),
            kind,
            added,
            removed,
            diff: None,
        });
        assert_eq!(
            delivered(&rows, true),
            DeliveryOutcome::Delivered {
                created: 1,
                modified: 2,
                deleted: 1,
                lines_added: 15,
                lines_removed: 17,
                proven: true,
            }
        );
        assert_eq!(
            delivered(&[], false),
            DeliveryOutcome::Delivered {
                created: 0,
                modified: 0,
                deleted: 0,
                lines_added: 0,
                lines_removed: 0,
                proven: false,
            },
            "an empty patch is a delivery that moved nothing, not a refusal to deliver"
        );
    }
}
