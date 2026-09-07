// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! One fixed step for one pull request.
//!
//! [`super::drive`] holds the poll loop and the queue walk. This holds the arm
//! that acts. Read the forge. Run the pure machine over what it said. Do the
//! one thing it returned.

use std::collections::HashMap;

use stella_autonomy::{Action, Attempts, CiConclusion, DeliverPolicy, PrState, deliver_next};
use stella_protocol::pull_request::PullRequestProvider;

use super::{Audit, Durable, Settlement, Spent, Tally, audit, notify};

/// What a delivery pass carries for the whole run.
///
/// Grouped so the per-pull-request arguments stand out. The rest never change
/// between them.
pub(super) struct Pass<'a> {
    /// The forge, behind its port.
    pub(super) forge: &'a dyn PullRequestProvider,
    /// Which checks may block a merge.
    pub(super) policy_blocking: &'a stella_autonomy::BlockingPolicy,
    /// Whether a human's approval is asked for before a merge.
    pub(super) no_review: bool,
    /// Where the audit line and the run's statistics go.
    pub(super) durable: &'a Durable,
    /// The workspace's settings, for the notifications a transition raises.
    pub(super) settings: &'a crate::settings::Settings,
}

/// Move one pull request by exactly one fixed step.
pub(super) fn advance(
    pass: &Pass<'_>,
    pr: &str,
    spent: &mut HashMap<String, Spent>,
    tally: &mut Tally,
) -> Result<Settlement, String> {
    let Pass {
        forge,
        policy_blocking,
        no_review,
        durable,
        settings,
    } = *pass;
    let entry = spent.entry(pr.to_owned()).or_default();
    let reading = crate::self_driving_cmd::deliver::observe(forge, pr, policy_blocking)?;
    if reading.settled {
        // Nothing left to decide. The merge below may have worked while the
        // cleanup after it did not. Or a human got there first. Either way,
        // this pull request is done.
        audit::record(
            durable,
            Audit::PrMerged,
            Some(pr),
            "already settled on the forge — carrying it no further",
        );
        // Not `Merged`. This arm is reached when a human merged it first, and
        // also when the merge below worked and the pass after it did not. Only
        // one of those is this loop's merge to report. Closing on it would credit the loop with a closure it did
        // not make, and the human who merged is the one who knows whether the
        // issue is finished.
        return Ok(Settlement::Otherwise);
    }
    // Said out loud every time. A loop that merges past a check the repository
    // marks required, without naming which one and on what grounds, is
    // indistinguishable from one that is simply broken.
    if !reading.waived.is_empty() {
        audit::record(
            durable,
            Audit::Waived,
            Some(pr),
            &format!(
                "not blocking on {} — {}{}",
                reading.waived.len(),
                reading.waived.join("; "),
                if reading.verified_locally {
                    ". This change was proved here instead"
                } else {
                    ". This change has no local proof either"
                }
            ),
        );
    }

    let obs = reading.observation;
    let policy = DeliverPolicy {
        require_approval: !no_review,
        ..DeliverPolicy::default()
    };

    let transition = deliver_next(
        PrState::CiPending,
        &obs,
        Attempts {
            fixes: entry.fixes,
            rebases: entry.rebases,
        },
        &policy,
    );

    audit::record(
        durable,
        Audit::PrObserved,
        Some(pr),
        &format!(
            "ci={:?} base={:?} mergeable={:?} review={:?} -> {:?}",
            obs.ci, obs.base_ci, obs.mergeable, obs.review, transition.action
        ),
    );

    // Counted whether or not it changes the action. Waiting on somebody
    // else's broken base is time this loop spent. A high count says something
    // about the repository, not about the loop.
    if obs.base_ci == CiConclusion::Red {
        durable.update_stats(|s| s.base_broken_waits += 1);
    }

    notify::observed(&durable.repo_root, settings, entry, transition.state, pr);

    match transition.action {
        Action::Merge => {
            crate::self_driving_cmd::deliver::merge(forge, pr)?;
            tally.merged += 1;
            durable.update_stats(|s| s.prs_merged += 1);
            audit::record(durable, Audit::PrMerged, Some(pr), "merged");
            // After the merge, so the event means it landed. The
            // already-settled arm above fires none: a human merged that one.
            notify::performed(
                &durable.repo_root,
                settings,
                crate::self_driving_cmd::hooks::HookEvent::PullRequestMerged,
                pr,
                None,
            );
            Ok(Settlement::Merged)
        }
        Action::Escalate { reason } => {
            tally.escalated += 1;
            durable.update_stats(|s| s.prs_escalated += 1);
            audit::record(
                durable,
                Audit::PrEscalated,
                Some(pr),
                &format!("handed to a human: {reason:?}"),
            );
            Ok(Settlement::Otherwise)
        }
        Action::MarkReady => {
            crate::self_driving_cmd::deliver::mark_ready(forge, pr)?;
            audit::record(
                durable,
                Audit::PrObserved,
                Some(pr),
                "ci is green — taken out of draft",
            );
            notify::performed(
                &durable.repo_root,
                settings,
                crate::self_driving_cmd::hooks::HookEvent::PullRequestReadyForReview,
                pr,
                None,
            );
            Ok(Settlement::Pending)
        }
        Action::PushFix => {
            // A red the base already explains is not this pull request's to
            // fix. `base_conclusion` cannot see it. It compares what is
            // failing *now*, and a check that ran against a base somebody has
            // since repaired keeps its old verdict. The pull request looks
            // guilty of a failure that happens nowhere.
            //
            // Asking the forge to run it again is the cheapest way to find
            // out. When the answer is "still red" it costs only CI minutes,
            // and the fix path below runs on the next poll with the re-run
            // already spent. One per pull request: a loop that
            // re-ran on every red would never reach the fix, and would spin on
            // a genuine failure for as long as the operator left it up.
            if !entry.rerun {
                entry.rerun = true;
                match crate::self_driving_cmd::deliver::refresh_against_base(forge, pr) {
                    Ok(()) => {
                        audit::record(
                            durable,
                            Audit::PrObserved,
                            Some(pr),
                            "red against a base that has since gone green — merged the base in for a \
                             fresh verdict before treating it as ours",
                        );
                        return Ok(Settlement::Pending);
                    }
                    Err(error) => audit::record(
                        durable,
                        Audit::Transient,
                        Some(pr),
                        &format!("could not ask for a re-run ({error}); treating the red as ours"),
                    ),
                }
            }

            // Counted even though this slice writes no fix. The ceiling is
            // what stops a loop pushing the same broken change for ever. A
            // counter that moved only on success would never reach it.
            entry.fixes += 1;
            // Both counters move, because two things are true and a reader of
            // either number alone would be misled. A fix was owed — that is
            // `fixes_pushed`, and it is what the attempt ceiling reads. And
            // the pull request was handed to a human — that is
            // `prs_escalated`, and it is what a dashboard reads as "needs
            // someone".
            //
            // Counting only the first left the audit log recording
            // `pr_escalated` twice while `prs_escalated` stayed at zero: two
            // records of one event that disagreed about what it was.
            durable.update_stats(|s| {
                s.fixes_pushed += 1;
                s.prs_escalated += 1;
            });
            tally.escalated += 1;
            audit::record(
                durable,
                Audit::PrEscalated,
                Some(pr),
                "needs a fix, which this build does not author — leaving it for a human",
            );
            Ok(Settlement::Otherwise)
        }
        Action::Rebase => {
            entry.rebases += 1;
            durable.update_stats(|s| s.rebases += 1);
            audit::record(
                durable,
                Audit::PrEscalated,
                Some(pr),
                "needs a rebase, which this build does not perform — leaving it",
            );
            Ok(Settlement::Otherwise)
        }
        Action::Wait => Ok(Settlement::Pending),
    }
}
