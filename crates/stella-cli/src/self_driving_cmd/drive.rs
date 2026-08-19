//! `drive` — the loop: ask the machine what now, do it, ask again.
//!
//! `doc:backlog-self-driving` §3.7 (#3599, #3942). [`stella_autonomy::step`] is
//! the pure decision; this is the process that keeps asking it and dispatches
//! what it returns to the verbs. Until this existed, `step` had no caller — the
//! machine was correct and unreachable.
//!
//! # The one obligation the machine cannot discharge for itself
//!
//! **On [`LoopStep::Blocked`], keep polling. Do not exit.**
//!
//! Self-resume is structural — the machine holds no latch, so a block is
//! recomputed from scratch on every call and stops being returned the moment
//! its cause is gone. But a host that exits on a block makes resumption
//! impossible no matter what the machine returns. That is why `Blocked` carries
//! a `Clearance` naming the observable to watch rather than reading as terminal,
//! and why this loop sleeps and re-asks instead of returning.
//!
//! Nobody should ever have to tell this loop it may resume. A human who raises
//! the ceiling, restores the grant, drops the stop flag or merges the escalated
//! pull request has already said everything that needs saying.
//!
//! # What this slice performs, and what it only reports
//!
//! It drives `claim → work → deliver`, which is the path that produces pull
//! requests. `Sweep` and `Curate` are returned by the machine and **reported
//! rather than performed** — B4 and B6 build them. Each says out loud that it
//! is not built and stops, rather than spinning on a step it cannot take: a
//! driver that silently did nothing for a step it was handed would look healthy
//! forever.

use std::collections::HashMap;

use stella_autonomy::{
    Action, Attempts, CarriedPr, Contention, DeliverPolicy, Doctrine, IssueRef, LoopObservation,
    LoopState, LoopStep, PrDisposition, PrRef, PrState, UnblockAttempt, deliver_next, step,
};

/// Attempts spent on one pull request, carried across polls.
///
/// The pure machine *reads* the counts; something durable has to hold them, and
/// for the length of one run that is this process.
#[derive(Debug, Default, Clone, Copy)]
struct Spent {
    fixes: u32,
    rebases: u32,
}

/// What one run of the loop did.
#[derive(Debug, Default)]
struct Tally {
    opened: u32,
    merged: u32,
    escalated: u32,
}

/// Drive until there is nothing left to do, or a bound is reached.
///
/// `max_issues` bounds one invocation so a demonstration terminates; the real
/// loop never does, which is why the bound is a parameter rather than a
/// constant and why reaching it is reported as *reached the bound*, never as
/// *finished*.
pub(super) fn drive(
    max_issues: u32,
    no_review: bool,
    spend_limit: Option<f64>,
    poll_secs: u64,
) -> Result<(), String> {
    let root = super::state::repo_root();
    let cfg = super::config::load(&root);
    let doctrine = Doctrine::default();
    let provider = crate::issue_provider::GhIssueProvider;

    let mut state = LoopState {
        planned: true,
        batch: max_issues,
        ..LoopState::default()
    };
    let mut spent: HashMap<String, Spent> = HashMap::new();
    let mut tally = Tally::default();

    loop {
        let obs = observe(max_issues, tally.opened);

        match step(&state, &obs, &doctrine) {
            LoopStep::Blocked {
                reason,
                clears_when,
            } => {
                // Park, do not exit. See the module docs — this is the one
                // thing the machine cannot do for itself.
                eprintln!(
                    "blocked: {reason:?}; waiting for {clears_when:?}. Re-asking in {poll_secs}s."
                );
                sleep(poll_secs);
            }

            LoopStep::Plan => state.planned = true,

            LoopStep::Claim { .. } => {
                let ranked = super::backlog::ranked_keys(&provider)?;
                let Some(key) = ranked.into_iter().find(|k| {
                    !state.claimed.iter().any(|c| &c.0 == k)
                        && !spent.keys().any(|worked| worked == k)
                }) else {
                    eprintln!("the queue offered nothing this run has not already taken");
                    return report(&tally);
                };
                eprintln!("claimed #{key}");
                state.claimed.push(IssueRef(key));
            }

            LoopStep::Work { issue } => {
                state.claimed.retain(|i| i != &issue);
                // Recorded before the work starts, so a failure cannot make the
                // loop re-claim the same issue forever.
                spent.entry(issue.0.clone()).or_default();

                let resolved = super::backlog::resolve(&provider, &issue.0)?;
                eprintln!("working #{} — {}", issue.0, resolved.title);

                // A `work` that cannot start — a leftover branch from a killed
                // run, an unreachable worktree — is one issue's problem, not
                // the loop's. Propagating it would let a single stale branch
                // halt a run that has three other issues it could be getting
                // on with, and the operator would have to intervene for
                // something the loop can simply route around.
                //
                // Reported at full volume rather than swallowed: the branch is
                // still there, still holds whatever the dead run left, and
                // still needs a human eventually.
                let outcome =
                    match super::work::start(&root, &resolved, spend_limit, &cfg.attribution) {
                        Ok(outcome) => outcome,
                        Err(reason) => {
                            eprintln!("  could not start: {reason}");
                            eprintln!("  moving on to the next issue");
                            continue;
                        }
                    };

                match outcome {
                    super::work::WorkOutcome::Changed { branch, stat, .. } => {
                        eprintln!("  changed: {stat}");
                        let title = format!("{} (#{})", resolved.title, issue.0);
                        let pr = super::deliver::open(
                            &root,
                            &branch,
                            &issue.0,
                            &title,
                            &cfg.attribution.pull_request,
                        )?;
                        eprintln!("  opened pr #{pr}");
                        tally.opened += 1;
                        state.carrying.push(CarriedPr {
                            pr: PrRef(pr),
                            disposition: PrDisposition::Moving,
                        });
                    }
                    super::work::WorkOutcome::NoChange { why } => {
                        eprintln!("  changed nothing ({why}) — moving on");
                    }
                    super::work::WorkOutcome::Failed { reason } => {
                        eprintln!("  not worked: {reason} — moving on");
                    }
                }
            }

            LoopStep::Deliver { pr } => {
                let settled = advance(&root, &pr.0, &mut spent, no_review, &mut tally)?;
                if settled {
                    for carried in &mut state.carrying {
                        if carried.pr == pr {
                            carried.disposition = PrDisposition::Settled;
                        }
                    }
                } else {
                    sleep(poll_secs);
                }
            }

            LoopStep::Unblock { attempt } => match attempt {
                // Filing is the half that is always worth doing, under every
                // doctrine that files at all: the ticket makes the breakage
                // visible and attributable even if no fix is ever attempted.
                UnblockAttempt::FileBaseBreakage => {
                    eprintln!(
                        "the base is broken and unfiled. This build does not compose the report \
                         yet — that is `sweep` (B4, #3599). Stopping rather than proceeding \
                         while every pull request it opens would fail CI."
                    );
                    return report(&tally);
                }
                UnblockAttempt::AdoptBaseBreakage { issue } => {
                    eprintln!("adopting the base breakage as #{}", issue.0);
                    state.claimed.push(issue);
                }
                UnblockAttempt::DeferToExistingFix { evidence } => {
                    eprintln!(
                        "someone else is already fixing the base ({}). Waiting rather than \
                         duplicating — two workers on one broken base produce a conflict on top \
                         of an outage.",
                        evidence.join(", ")
                    );
                    sleep(poll_secs);
                }
            },

            LoopStep::Sweep { lens } => {
                eprintln!(
                    "the machine asked to sweep `{lens}`, and this build cannot — B4 builds it \
                     (#3599). Stopping rather than spinning on a step it cannot take."
                );
                return report(&tally);
            }
            LoopStep::Curate => {
                eprintln!(
                    "the machine asked to curate, and this build cannot — B6 builds it (#3599). \
                     Stopping rather than spinning."
                );
                return report(&tally);
            }

            LoopStep::Watch { until } => {
                eprintln!("nothing to do; the machine would watch for {until:?}");
                return report(&tally);
            }
        }
    }
}

/// Advance one pull request by exactly one deterministic transition.
///
/// Returns whether it settled — merged, escalated, or handed back to a human —
/// so the caller can stop carrying it.
fn advance(
    root: &std::path::Path,
    pr: &str,
    spent: &mut HashMap<String, Spent>,
    no_review: bool,
    tally: &mut Tally,
) -> Result<bool, String> {
    let entry = spent.entry(pr.to_owned()).or_default();
    let obs = super::deliver::observe(root, pr)?;
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

    eprintln!(
        "pr #{pr}: ci={:?} base={:?} mergeable={:?} -> {:?}",
        obs.ci, obs.base_ci, obs.mergeable, transition.action
    );

    match transition.action {
        Action::Merge => {
            super::deliver::merge(pr)?;
            tally.merged += 1;
            eprintln!("  merged #{pr}");
            Ok(true)
        }
        Action::Escalate { reason } => {
            tally.escalated += 1;
            eprintln!("  escalated to a human: {reason:?}");
            Ok(true)
        }
        Action::MarkReady => {
            super::deliver::mark_ready(pr)?;
            eprintln!("  taken out of draft");
            Ok(false)
        }
        Action::PushFix => {
            // Counted even though this slice does not author the fix: the
            // ceiling is what stops a loop pushing the same broken change
            // forever, and a counter that only moved on success would never
            // reach it.
            entry.fixes += 1;
            eprintln!(
                "  needs a fix, which this build does not author — that is `work` on the \
                 existing branch, and it is the next slice. Leaving it for a human."
            );
            Ok(true)
        }
        Action::Rebase => {
            entry.rebases += 1;
            eprintln!("  needs a rebase, which this build does not perform. Leaving it.");
            Ok(true)
        }
        Action::Wait => Ok(false),
    }
}

/// What the world looks like, in the shape the machine decides over.
///
/// The bound is expressed as an exhausted queue rather than as a special case
/// in the machine: "there is no more work for me" is something it already knows
/// how to handle, and a second way to stop would be a second definition of
/// stopping.
fn observe(max_issues: u32, opened: u32) -> LoopObservation {
    LoopObservation {
        grant_valid: true,
        stop_requested: false,
        budget_exhausted: false,
        queue_depth: max_issues.saturating_sub(opened),
        base_broken: false,
        base_breakage_filed: false,
        base_breakage_issue: None,
        base_fix_contention: Contention::default(),
    }
}

fn sleep(secs: u64) {
    std::thread::sleep(std::time::Duration::from_secs(secs));
}

fn report(tally: &Tally) -> Result<(), String> {
    println!(
        "loop stopped: {} opened, {} merged, {} escalated",
        tally.opened, tally.merged, tally.escalated
    );
    Ok(())
}
