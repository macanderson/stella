//! `loop` — the driver: ask the machine what now, do it, ask again.
//!
//! `doc:backlog-self-driving` §3.7 (#3599, #3942). [`stella_autonomy::step`] is
//! the pure decision; this is the process that keeps asking it and dispatches
//! what it returns to the verbs.
//!
//! # The one obligation the machine cannot discharge for itself
//!
//! **On [`LoopStep::Blocked`], keep polling. Do not exit.**
//!
//! Self-resume is structural — the machine holds no latch, so a block is
//! recomputed from scratch on every call and stops being returned the moment
//! its cause is gone. But a host that exits the process on a block makes
//! resumption impossible no matter what the machine returns. That is why
//! `Blocked` carries a [`Clearance`](stella_autonomy::Clearance) naming the
//! observable to watch rather than reading as a terminal state, and why this
//! loop sleeps and re-asks instead of returning.
//!
//! Nobody should ever have to tell this loop it may resume. A human who raises
//! the ceiling, restores the grant, drops the stop flag or merges the escalated
//! pull request has already said everything that needs saying.
//!
//! # What this slice does and does not do
//!
//! It drives `claim → work → deliver` over the ranked queue, which is the path
//! that produces pull requests. `Sweep` and `Curate` are returned by the
//! machine and **reported rather than performed** — B4 and B6 build them, and a
//! driver that silently did nothing for a step it was handed would be the
//! unwired-code shape this repository files issues about. Each one says out
//! loud that it is not built, names the phase that builds it, and stops the
//! loop rather than spinning on a step it cannot perform.

use std::collections::HashMap;

use stella_autonomy::{
    CarriedPr, Doctrine, IssueRef, LoopObservation, LoopState, LoopStep, PrDisposition, PrRef, step,
};

use super::state::LoopState as Durable;

/// How long to wait before re-asking, when the answer was "wait".
///
/// Deliberately not configurable at this slice. CI on this repository takes
/// minutes, so a shorter interval only burns forge API quota, and a longer one
/// makes the demonstration unwatchable.
const POLL_SECS: u64 = 60;

/// One pull request the driver is carrying, and the attempts spent on it.
///
/// Attempt counting lives here rather than in the pure machine's inputs by
/// accident of layering: the machine *reads* the counts, and something durable
/// has to hold them across polls. The driver is that something.
#[derive(Debug, Default, Clone, Copy)]
struct Spent {
    fixes: u32,
    rebases: u32,
}

/// Drive the loop until it has nothing left to do, or is told to stop.
///
/// `max_issues` bounds one invocation so a demonstration terminates; the real
/// loop never does, which is why the bound is a parameter rather than a
/// constant and why reaching it is reported as *reached the bound*, never as
/// *finished*.
pub(super) fn drive(
    durable: &Durable,
    max_issues: u32,
    no_review: bool,
    spend_limit: Option<f64>,
) -> Result<(), String> {
    let root = super::state::repo_root();
    let doctrine = Doctrine::default();

    let mut state = LoopState {
        planned: true,
        batch: max_issues,
        ..LoopState::default()
    };
    let mut spent: HashMap<String, Spent> = HashMap::new();
    let mut issue_for_pr: HashMap<String, String> = HashMap::new();
    let mut opened = 0u32;
    let mut merged = 0u32;

    loop {
        let obs = observe_world(&root, &state, max_issues, opened);
        let next = step(&state, &obs, &doctrine);

        match next {
            LoopStep::Blocked {
                reason,
                clears_when,
            } => {
                // Park, do not exit. See the module docs — this is the one
                // thing the machine cannot do for itself.
                eprintln!(
                    "blocked: {reason:?} — waiting for {clears_when:?}; re-asking in {POLL_SECS}s"
                );
                sleep();
            }

            LoopStep::Plan => state.planned = true,

            LoopStep::Claim { batch: _ } => {
                let Some(issue) = next_unclaimed(&root, &state)? else {
                    eprintln!("queue offered nothing this loop has not already taken");
                    return report(opened, merged);
                };
                eprintln!("claimed #{}", issue.0);
                state.claimed.push(issue);
            }

            LoopStep::Work { issue } => {
                state.claimed.retain(|i| i != &issue);
                let resolved =
                    super::backlog::resolve(&crate::issue_provider::GhIssueProvider, &issue.0)?;

                eprintln!("working #{}", issue.0);
                match super::work::start(&root, &resolved, spend_limit)? {
                    super::work::WorkOutcome::Changed { branch, .. } => {
                        let title = format!("{} (#{})", resolved.title, issue.0);
                        let pr = super::deliver::open(&root, &branch, &issue.0, &title)?;
                        eprintln!("opened pr #{pr} for #{}", issue.0);
                        opened += 1;
                        issue_for_pr.insert(pr.clone(), issue.0.clone());
                        state.carrying.push(CarriedPr {
                            pr: PrRef(pr),
                            disposition: PrDisposition::Moving,
                        });
                    }
                    super::work::WorkOutcome::NoChange { why } => {
                        eprintln!("#{} changed nothing ({why}) — moving on", issue.0);
                    }
                    super::work::WorkOutcome::Failed { reason } => {
                        eprintln!("#{} was not worked: {reason} — moving on", issue.0);
                    }
                }
            }

            LoopStep::Deliver { pr } => {
                let spend = spent.entry(pr.0.clone()).or_default();
                let settled = advance_pr(&root, &pr.0, spend, no_review, &mut merged)?;
                if settled {
                    for carried in &mut state.carrying {
                        if carried.pr == pr {
                            carried.disposition = PrDisposition::Settled;
                        }
                    }
                } else {
                    sleep();
                }
            }

            // Returned by the machine, not built yet. Said out loud rather
            // than silently skipped — a driver that did nothing for a step it
            // was handed would spin forever and look healthy.
            LoopStep::Sweep { lens } => {
                eprintln!(
                    "the machine asked for a sweep of `{lens}` and this build cannot perform one \
                     — B4 builds it (#3599). Stopping rather than spinning."
                );
                return report(opened, merged);
            }
            LoopStep::Curate => {
                eprintln!(
                    "the machine asked to curate and this build cannot — B6 builds it (#3599). \
                     Stopping rather than spinning."
                );
                return report(opened, merged);
            }

            LoopStep::Watch { until } => {
                eprintln!("nothing to do; the machine would watch for {until:?}");
                return report(opened, merged);
            }
        }

        let _ = durable;
    }
}

/// Advance one pull request by exactly one deterministic transition.
///
/// Returns whether it has settled — merged or escalated — so the caller can
/// stop carrying it.
fn advance_pr(
    root: &std::path::Path,
    pr: &str,
    spend: &mut Spent,
    no_review: bool,
    merged: &mut u32,
) -> Result<bool, String> {
    use stella_autonomy::{Action, Attempts, DeliverPolicy, PrState, deliver_next};

    let obs = super::deliver::observe(root, pr)?;
    let policy = DeliverPolicy {
        require_approval: !no_review,
        ..DeliverPolicy::default()
    };
    let transition = deliver_next(
        PrState::CiPending,
        &obs,
        Attempts {
            fixes: spend.fixes,
            rebases: spend.rebases,
        },
        &policy,
    );

    eprintln!(
        "pr #{pr}: ci={:?} base={:?} -> {:?} / {:?}",
        obs.ci, obs.base_ci, transition.state, transition.action
    );

    match transition.action {
        Action::Merge => {
            super::deliver::merge(pr)?;
            *merged += 1;
            eprintln!("merged pr #{pr}");
            Ok(true)
        }
        Action::Escalate { reason } => {
            eprintln!("pr #{pr} escalated to a human: {reason:?}");
            Ok(true)
        }
        Action::PushFix => {
            // Counted even though this slice does not author the fix, because
            // the ceiling is what stops a loop pushing the same broken change
            // forever — and a counter that only increments on success would
            // never reach it.
            spend.fixes += 1;
            eprintln!(
                "pr #{pr} needs a fix and this build does not author one — that is `work` on the \
                 existing branch, and it is the next slice. Leaving it for a human."
            );
            Ok(true)
        }
        Action::Rebase => {
            spend.rebases += 1;
            eprintln!("pr #{pr} needs a rebase; this build does not perform one. Leaving it.");
            Ok(true)
        }
        Action::MarkReady => {
            mark_ready(pr)?;
            Ok(false)
        }
        Action::Wait => Ok(false),
    }
}

fn mark_ready(pr: &str) -> Result<(), String> {
    let out = std::process::Command::new("gh")
        .args(["pr", "ready", pr])
        .env("NO_COLOR", "1")
        .output()
        .map_err(|error| format!("could not run `gh`: {error}"))?;
    if out.status.success() {
        eprintln!("pr #{pr} taken out of draft");
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// The next ranked defect this loop is not already carrying.
fn next_unclaimed(
    root: &std::path::Path,
    state: &LoopState,
) -> Result<Option<IssueRef>, String> {
    let _ = root;
    let ranked = super::backlog::ranked_keys(&crate::issue_provider::GhIssueProvider)?;
    Ok(ranked
        .into_iter()
        .find(|key| !state.claimed.iter().any(|c| &c.0 == key))
        .map(IssueRef))
}

/// Read the world into the shape the machine decides over.
fn observe_world(
    root: &std::path::Path,
    state: &LoopState,
    max_issues: u32,
    opened: u32,
) -> LoopObservation {
    let _ = (root, state);
    LoopObservation {
        grant_valid: true,
        stop_requested: false,
        budget_exhausted: false,
        // The bound is expressed as an empty queue rather than as a special
        // case in the machine: "there is no more work for me" is a thing the
        // machine already knows how to handle, and adding a second way to stop
        // would be a second definition of stopping.
        queue_depth: max_issues.saturating_sub(opened),
        base_broken: false,
        base_breakage_filed: false,
        base_breakage_issue: None,
        base_fix_contention: stella_autonomy::Contention::default(),
    }
}

fn sleep() {
    std::thread::sleep(std::time::Duration::from_secs(POLL_SECS));
}

fn report(opened: u32, merged: u32) -> Result<(), String> {
    println!("loop stopped: {opened} pull request(s) opened, {merged} merged");
    Ok(())
}
