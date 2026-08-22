//! `drive` — the loop: ask the machine what now, do it, ask again.
//!
//! `doc:backlog-self-driving` §3.7 (#3599, #3942). [`stella_autonomy::step`] is
//! the pure decision; this is the process that keeps asking it and dispatches
//! what it returns to the verbs. Until this existed, `step` had no caller — the
//! machine was correct and unreachable.
//!
//! # It does not stop
//!
//! A perpetual loop that exits on a transient failure is not perpetual, and the
//! failures it will actually meet are almost all transient: a forge that
//! rate-limits, a network that drops, a `gh` that times out. **Every step below
//! treats a failure as a reason to wait and re-ask, never as a reason to
//! return.** What ends a run is reaching its bound, or the machine asking for a
//! step this build cannot perform.
//!
//! That is not a robustness nicety. It is the difference between a loop an
//! operator can walk away from and one that quietly died at 3am and left the
//! backlog untouched until somebody noticed.
//!
//! # On a block it parks, and it clears itself
//!
//! Self-resume is structural — the machine holds no latch, so a block is
//! recomputed on every call and stops being returned the moment its cause is
//! gone. But a host that exits on a block makes resumption impossible whatever
//! the machine returns. Nobody should ever have to tell this loop it may
//! resume: a human who raises the ceiling, restores the grant, drops the stop
//! flag or merges the escalated pull request has already said everything that
//! needs saying.
//!
//! # It resumes by looking, not by remembering
//!
//! A crashed process loses what it held in memory, so the loop keeps as little
//! there as it can. On start it **re-derives what it was carrying from the
//! forge**: every open pull request on a branch with this workspace's prefix is
//! work it began and has not finished.
//!
//! Re-deriving beats persisting because the forge is the source of truth about
//! a pull request and a remembered list can only be wrong — stale after a human
//! merges one by hand, or after a run died between opening and recording. Same
//! reasoning `deliver merge` uses when it re-observes rather than trusting that
//! the caller already ran `next`.
//!
//! # Everything it does is said out loud and written down
//!
//! Every action goes through [`super::audit`]: a line on screen for whoever is
//! watching, and one JSON object in `audit.jsonl` for whoever reconstructs the
//! run afterwards. A scrollback buffer cannot answer *what did it do, to what,
//! when, and how did that turn out* — it is the least durable surface in the
//! system and it is gone when the terminal closes.
//!
//! # What this slice performs, and what it only reports
//!
//! It drives `claim → work → deliver`, the path that produces pull requests.
//! `Sweep` and `Curate` are returned by the machine and **reported rather than
//! performed** — B4 and B6 build them. Each says out loud that it is not built
//! and stops, rather than spinning on a step it cannot take: a driver that did
//! nothing for a step it was handed would look healthy forever.

use std::collections::HashMap;

use stella_autonomy::{
    Action, Attempts, CarriedPr, CiConclusion, Contention, ContentionPolicy, ContentionVerdict,
    DeliverPolicy, IssueRef, LoopObservation, LoopState, LoopStep, PrDisposition, PrRef, PrState,
    UnblockAttempt, contention_verdict, deliver_next, step,
};
use stella_fleet::issue_claim_key;

use super::audit::{self, Action as Audit};
use super::state::LoopState as Durable;

/// Attempts spent on one pull request, carried across polls.
///
/// The pure machine *reads* the counts; something has to hold them, and for the
/// length of one run that is this process.
#[derive(Debug, Default, Clone, Copy)]
struct Spent {
    fixes: u32,
    rebases: u32,
    /// Whether the stale-red re-run has already been spent on this pull
    /// request. Exactly one is allowed — see [`advance`].
    rerun: bool,
}

/// What one run of the loop did, for the closing line.
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
/// constant and why reaching it reports *reached the bound*, never *finished*.
pub(super) fn drive(
    durable: &Durable,
    max_issues: u32,
    no_review: bool,
    spend_limit: Option<f64>,
    poll_secs: u64,
) -> Result<(), String> {
    let root = super::state::repo_root();
    let cfg = super::config::load(&root);
    let doctrine = cfg.doctrine;
    let provider = crate::issue_provider::GhIssueProvider;
    // The branch every pull request targets, and the one whose health
    // decides whether there is any point opening more.
    // `base_ref` yields `origin/<branch>`; the forge wants the branch
    // itself — see `deliver::checks_for_branch` on why a remote-tracking
    // spelling must never reach it.
    let base_branch = super::work::base_ref(&root)
        .rsplit('/')
        .next()
        .unwrap_or("main")
        .to_owned();

    audit::record(
        durable,
        Audit::SessionStarted,
        None,
        &format!(
            "session began — up to {max_issues} issue(s), {}, polling every {poll_secs}s",
            if no_review {
                "merging without waiting for review"
            } else {
                "waiting for review before any merge"
            }
        ),
    );

    // Installed before anything is claimed. The loop cannot record an
    // escalation against a label the tracker does not carry, and discovering
    // that at the first failure — rather than here — would lose the one record
    // that stops the next run repeating the attempt.
    crate::issue_provider::ensure_label(
        stella_autonomy::ESCALATION_LABEL,
        // Under 100 characters, which is GitHub's cap on a label description.
        "Attempted by the self-driving loop and unresolved. Still wanted — remove to requeue.",
    )
    .map_err(|error| format!("could not install the escalation label: {error}"))?;

    // Resolved once: auto-detection reads the filesystem, and the answer
    // cannot change while the loop runs.
    let verify_command = cfg
        .verify_command
        .clone()
        .or_else(|| super::work::default_verify_command(&root));
    match verify_command.as_deref() {
        Some(command) => audit::record(
            durable,
            Audit::SessionStarted,
            None,
            &format!("changes will be proved here with `{command}` before delivery"),
        ),
        None => audit::record(
            durable,
            Audit::SessionStarted,
            None,
            "no local verification command — delivery rests on the forge's checks alone",
        ),
    }

    // Is the gate winnable at all?
    //
    // Measured once, on the base, before any change is judged by it. A
    // repository whose own test suite already fails on `main` has a local gate
    // exactly as unwinnable as a suspended CI account, and blocking on it
    // would be the same mistake one layer down — every pull request rejected
    // for a failure that was there before it existed.
    //
    // When the baseline is red the verification still runs and is still
    // recorded, but it stops being a veto. That is the honest reading: the
    // signal cannot distinguish this change's fault from the tree's, so it is
    // evidence rather than a verdict.
    let baseline_green = match verify_command.as_deref() {
        Some(command) => {
            audit::record(
                durable,
                Audit::VerifyStarted,
                None,
                &format!(
                    "measuring the baseline — `{command}` on origin/{base_branch}, in a clean worktree"
                ),
            );
            match super::work::verify_base(
                &root,
                &format!("origin/{base_branch}"),
                command,
                cfg.verify_timeout_secs,
            ) {
                Ok(()) => {
                    audit::record(
                        durable,
                        Audit::Verified,
                        None,
                        "the baseline is green, so local verification can veto a change",
                    );
                    true
                }
                Err(reason) => {
                    audit::record(
                        durable,
                        Audit::Waived,
                        None,
                        &format!(
                            "the baseline is already red ({reason}) — local verification \
                             becomes advisory, because it cannot tell this change's fault \
                             from the tree's"
                        ),
                    );
                    false
                }
            }
        }
        None => false,
    };

    let mut state = LoopState {
        planned: true,
        batch: max_issues,
        ..LoopState::default()
    };
    // Best effort: a tracker that refuses the label costs the loop its record
    // of local verification, which is worth a warning and not a refusal to run.
    if let Err(error) = crate::issue_provider::ensure_label(
        super::deliver::VERIFIED_LOCALLY_LABEL,
        "Proved by running this project's own checks locally, not by remote CI.",
    ) {
        audit::record(
            durable,
            Audit::Transient,
            None,
            &format!("could not install the local-verification label: {error}"),
        );
    }

    // The loop's own vocabulary, installed before it needs it.
    //
    // Triage answers in the configured ladder and then writes that label; a
    // tracker that does not carry the label rejects the write, and the issue
    // comes back unassessed forever. A fresh repository has GitHub's stock
    // labels and nothing else, so this is the ordinary case rather than the
    // exotic one — it is what lets the loop be pointed at a repository nobody
    // prepared for it.
    for (index, rung) in cfg.triage.ladder.rungs.iter().enumerate() {
        let _ = crate::issue_provider::ensure_label(
            rung,
            &format!("Priority {index} — installed by the self-driving loop."),
        );
    }
    for kind in &cfg.triage.defect_kinds {
        let _ = crate::issue_provider::ensure_label(kind, "Work the self-driving loop may take.");
    }

    let mut spent: HashMap<String, Spent> = HashMap::new();
    // The ledger claims this run holds, one per issue taken and not yet
    // worked. Keyed by issue so the Work arm can take back the one it is
    // about to run; dropped wholesale if the run ends holding any, which
    // releases them rather than leaving a peer to wait out the TTL.
    let mut leases: HashMap<String, super::claim::Lease> = HashMap::new();
    // Issues this process already offered to triage. The escalation label is
    // the durable half; this covers the window before it lands, and stops a
    // provider that failed to accept the label costing a turn every pass.
    let mut triaged: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut tally = Tally::default();

    resume(durable, &cfg, &mut state);

    loop {
        let obs = observe(&root, &provider, &base_branch, max_issues, tally.opened);

        match step(&state, &obs, &doctrine) {
            LoopStep::Blocked {
                reason,
                clears_when,
            } => {
                audit::record(
                    durable,
                    Audit::Waited,
                    None,
                    &format!(
                        "blocked: {reason:?}. Waiting for {clears_when:?}; re-asking in \
                         {poll_secs}s — nobody needs to tell it to resume."
                    ),
                );
                sleep(poll_secs);
            }

            LoopStep::Plan => state.planned = true,

            LoopStep::Claim { .. } => {
                // Judged before ranked, every single time.
                //
                // An issue nobody has labelled is not low-priority, it is
                // unplaced — and the queue is re-read from the tracker on
                // every claim precisely so that a P0 filed a minute ago, or an
                // issue somebody just re-labelled, is taken next rather than
                // whenever this run happens to come back round. Assessing
                // first is what makes that true for an issue that arrived
                // with no labels at all: otherwise it would sit invisible
                // while the loop worked an older, lesser, already-labelled
                // one.
                //
                // One per pass. Triage is a model call, and draining a
                // hundred unlabelled issues before touching any work would
                // turn a delivery loop into a labelling bot.
                //
                // Ranked is then filtered by contention as well as by what
                // this run already holds — see [`next_claimable`]. Before
                // that the branch-collision guard in `work::start` was the
                // only thing that noticed a peer, which is a git failure
                // after git state exists rather than a look before it (#4002).
                //
                // The two guards see different things and the order matters:
                // a claim-time probe reads names, while `work::start` has a
                // specific git error and asks the forge, which is what lets
                // it tell the loop's own dead run from a live peer. An
                // operator whose clone is full of their own leftovers wants
                // `ContentionPolicy::ClaimsOnly`, which is what that policy
                // is for (`stella_autonomy::ContentionPolicy`).
                if let Err(error) =
                    assess_one(durable, &root, &provider, &cfg, spend_limit, &mut triaged)
                {
                    audit::record(
                        durable,
                        Audit::Transient,
                        None,
                        &format!("could not triage ({error}); ranking what is already placed"),
                    );
                }

                // A queue read is a network call. Failing it is a reason to
                // wait, never a reason to end a run meant to be perpetual.
                let ranked = match super::backlog::ranked_keys(&provider, &cfg.triage) {
                    Ok(ranked) => ranked,
                    Err(error) => {
                        audit::record(
                            durable,
                            Audit::Transient,
                            None,
                            &format!(
                                "could not read the queue ({error}); re-asking in {poll_secs}s"
                            ),
                        );
                        sleep(poll_secs);
                        continue;
                    }
                };

                let mut deferrals = 0_u32;
                let pick = next_claimable(
                    ranked,
                    |k| state.claimed.iter().any(|c| c.0 == k) || spent.contains_key(k),
                    doctrine.contention,
                    |k| super::contention::for_issue(&root, k),
                    |k| super::claim::acquire(&root, k),
                    |k, evidence| {
                        deferrals += 1;
                        audit::record(
                            durable,
                            Audit::Deferred,
                            Some(k),
                            &format!(
                                "somebody else is already on it ({}); taking the next candidate \
                                 rather than duplicating",
                                evidence.join(", ")
                            ),
                        );
                        durable.update_stats(|s| s.issues_deferred += 1);
                    },
                );

                let Pick::Take(key, lease) = pick else {
                    // An empty queue is a state, not an ending.
                    //
                    // Returning here made "there is nothing to do right now"
                    // indistinguishable from "this run is over", and the two
                    // want opposite responses: somebody files an issue, or a
                    // human removes an escalation label, and a loop that
                    // exited is not there to notice. It drained a backlog on
                    // oxagen-platform at 05:37 and stopped, with the
                    // repository still red on a failure nobody had filed yet.
                    //
                    // Waiting costs one queue read per poll and is the whole
                    // difference between a batch job and a loop somebody can
                    // walk away from.
                    //
                    // The deferral count is on this line because without it an
                    // exhausted queue and a queue whose every candidate went to
                    // a peer read identically, and the second is the one where
                    // something is happening.
                    audit::record(
                        durable,
                        Audit::Waited,
                        None,
                        &format!(
                            "the queue offers nothing this run has not already taken \
                             ({deferrals} deferred to another actor this pass); \
                             re-asking in {poll_secs}s — a new issue, or an escalation \
                             label removed, is all it takes"
                        ),
                    );
                    sleep(poll_secs);
                    continue;
                };

                audit::record(
                    durable,
                    Audit::Claimed,
                    Some(&key),
                    "taken off the ranked queue",
                );
                durable.update_stats(|s| s.issues_claimed += 1);
                if let Some(lease) = lease {
                    leases.insert(key.clone(), lease);
                }
                state.claimed.push(IssueRef(key));
            }

            LoopStep::Work { issue } => {
                state.claimed.retain(|i| i != &issue);

                // Held for the whole arm and released by dropping it, so every
                // way out of here — the outcome, an early `continue`, the run
                // reaching its bound — frees the key for the next pass instead
                // of leaving it to expire. Taken out of the map rather than
                // borrowed from it because *this* is the scope the lease
                // belongs to: the window where a turn is in flight and no pull
                // request exists yet for `open_prs` to speak for.
                let _lease = leases.remove(&issue.0);

                let resolved = match super::backlog::resolve(&provider, &issue.0) {
                    Ok(resolved) => resolved,
                    Err(error) => {
                        audit::record(
                            durable,
                            Audit::Transient,
                            Some(&issue.0),
                            &format!("could not read it ({error}); putting it back"),
                        );
                        // Put it back: the read failing says nothing about the
                        // issue, so dropping it would silently skip real work.
                        state.claimed.push(issue);
                        sleep(poll_secs);
                        continue;
                    }
                };

                // Recorded before the work starts, so a failure cannot make the
                // loop re-claim the same issue forever.
                spent.entry(issue.0.clone()).or_default();

                audit::record(
                    durable,
                    Audit::WorkStarted,
                    Some(&issue.0),
                    &format!("began work — {}", resolved.title),
                );

                // What the turn is about to learn, measured either side of it.
                // The turn is a child process writing into the repository's
                // own state root, so a delta over that state is the only
                // producer these counters can have — see `learning`.
                let learned_before = super::learning::tally(&root);

                // A `work` that cannot start is one issue's problem, not the
                // loop's: a single leftover branch must not halt a run that has
                // other issues to get on with.
                let outcome =
                    match super::work::start(&root, &resolved, spend_limit, &cfg.attribution) {
                        Ok(outcome) => outcome,
                        Err(reason) => {
                            audit::record(
                                durable,
                                Audit::Deferred,
                                Some(&issue.0),
                                &format!("could not start ({reason}); moving on"),
                            );
                            durable.update_stats(|s| s.issues_deferred += 1);
                            continue;
                        }
                    };
                let learned = super::learning::tally(&root).since(learned_before);

                durable.update_stats(|s| {
                    s.issues_attempted += 1;
                    s.turns_run += 1;
                    learned.add_to(s);
                });

                match outcome {
                    super::work::WorkOutcome::Changed {
                        branch, stat, path, ..
                    } => {
                        audit::record(
                            durable,
                            Audit::WorkChanged,
                            Some(&issue.0),
                            &format!("the turn left changes — {stat}"),
                        );
                        durable.update_stats(|s| s.issues_changed += 1);

                        // Proved here, in the worktree that holds the change,
                        // before anyone is asked to look at it. On a repository
                        // whose CI cannot run this is the only evidence there
                        // will ever be; on one whose CI works it is a cheap
                        // early read that costs a merge nothing.
                        let verified = match verify_command.as_deref() {
                            Some(command) => {
                                audit::record(
                                    durable,
                                    Audit::VerifyStarted,
                                    Some(&issue.0),
                                    &format!("proving it here — `{command}`"),
                                );
                                match super::work::verify_locally(
                                    &path,
                                    command,
                                    cfg.verify_timeout_secs,
                                ) {
                                    Ok(()) => {
                                        durable.update_stats(|s| s.verified_locally += 1);
                                        audit::record(
                                            durable,
                                            Audit::Verified,
                                            Some(&issue.0),
                                            "the project's own checks passed here",
                                        );
                                        true
                                    }
                                    Err(reason) => {
                                        audit::record(
                                            durable,
                                            Audit::VerifyFailed,
                                            Some(&issue.0),
                                            &format!(
                                                "{reason}{}",
                                                if baseline_green {
                                                    " — and the baseline was green, so this \
                                                     is this change's doing"
                                                } else {
                                                    " — but the baseline was already red, so \
                                                     this is not evidence against the change"
                                                }
                                            ),
                                        );
                                        // Advisory when the gate was already
                                        // unwinnable: see `baseline_green`.
                                        !baseline_green
                                    }
                                }
                            }
                            None => false,
                        };

                        let title = cfg
                            .attribution
                            .title(&format!("{} (#{})", resolved.title, issue.0));
                        let pr = match super::deliver::open(
                            &root,
                            &branch,
                            &issue.0,
                            &title,
                            &cfg.attribution.pull_request,
                        ) {
                            Ok(pr) => pr,
                            Err(error) => {
                                // The work is committed on its branch and is not
                                // lost: the next start re-derives open pull
                                // requests from the forge.
                                audit::record(
                                    durable,
                                    Audit::Transient,
                                    Some(&issue.0),
                                    &format!(
                                        "could not open a pull request ({error}); the branch \
                                         `{branch}` keeps the work"
                                    ),
                                );
                                continue;
                            }
                        };

                        if verified {
                            // On the pull request, not in this process, so it
                            // survives a restart and a human can see which
                            // merges rested on local evidence.
                            if let Err(error) = super::deliver::mark_verified_locally(&pr) {
                                audit::record(
                                    durable,
                                    Audit::Transient,
                                    Some(&pr),
                                    &format!("could not record local verification: {error}"),
                                );
                            }
                        }

                        audit::record(
                            durable,
                            Audit::PrOpened,
                            Some(&pr),
                            &format!("opened for #{} — ci in progress", issue.0),
                        );
                        tally.opened += 1;
                        durable.update_stats(|s| s.prs_opened += 1);
                        state.carrying.push(CarriedPr {
                            pr: PrRef(pr),
                            // Recorded so a still-red base does not get adopted
                            // again once its fix is in flight as this PR: the
                            // idempotency guard compares issue-to-issue, which
                            // a PR number could never satisfy.
                            issue: Some(issue.clone()),
                            disposition: PrDisposition::Moving,
                        });
                    }

                    super::work::WorkOutcome::NoChange { why } => {
                        audit::record(
                            durable,
                            Audit::WorkNoChange,
                            Some(&issue.0),
                            &format!("the turn changed nothing ({why}); worktree released"),
                        );
                        durable.update_stats(|s| s.issues_no_change += 1);
                    }

                    super::work::WorkOutcome::Failed { reason } => {
                        audit::record(
                            durable,
                            Audit::WorkFailed,
                            Some(&issue.0),
                            &format!("the turn did not complete: {reason}"),
                        );
                        durable.update_stats(|s| {
                            s.issues_failed += 1;
                            // A turn stopped by its ceiling is a budget fact,
                            // not a sign the issue is unworkable; conflating
                            // them would make a too-small allowance look like a
                            // hard backlog.
                            if reason.contains("budget exceeded") {
                                s.turns_over_budget += 1;
                            }
                        });
                        escalate(durable, &provider, &cfg, &resolved.key, &reason);
                    }
                }
            }

            LoopStep::Deliver { pr } => {
                let settled = match advance(
                    &cfg.merge, &pr.0, &mut spent, no_review, &mut tally, durable,
                ) {
                    Ok(settled) => settled,
                    Err(error) => {
                        audit::record(
                            durable,
                            Audit::Transient,
                            Some(&pr.0),
                            &format!("could not advance it ({error}); re-asking in {poll_secs}s"),
                        );
                        sleep(poll_secs);
                        false
                    }
                };

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
                UnblockAttempt::FileBaseBreakage => {
                    // Filing is the half worth doing under every doctrine that
                    // files at all: the ticket makes the breakage visible and
                    // attributable even if this loop never fixes it. It is
                    // also what makes the next step possible — `step` will
                    // return `AdoptBaseBreakage` once an issue exists, and the
                    // ordinary work path takes it from there.
                    match super::backlog::file_base_breakage(
                        &provider,
                        &base_branch,
                        &cfg.attribution,
                    ) {
                        Ok(key) => audit::record(
                            durable,
                            Audit::FiledBaseBreakage,
                            Some(&key),
                            "the base is broken and nobody had filed it",
                        ),
                        Err(error) => {
                            audit::record(
                                durable,
                                Audit::Transient,
                                None,
                                &format!(
                                    "the base is broken and this could not file it ({error}); \
                                     re-asking in {poll_secs}s"
                                ),
                            );
                            sleep(poll_secs);
                        }
                    }
                }
                UnblockAttempt::AdoptBaseBreakage { issue } => {
                    audit::record(
                        durable,
                        Audit::Claimed,
                        Some(&issue.0),
                        "adopting the base breakage — it blocks everyone, not just its author",
                    );
                    state.claimed.push(issue);
                }
                UnblockAttempt::DeferToExistingFix { evidence } => {
                    audit::record(
                        durable,
                        Audit::Deferred,
                        None,
                        &format!(
                            "somebody else is already fixing the base ({}); waiting rather \
                             than duplicating",
                            evidence.join(", ")
                        ),
                    );
                    sleep(poll_secs);
                }
            },

            LoopStep::Sweep { lens } => {
                audit::record(
                    durable,
                    Audit::SessionStopped,
                    None,
                    &format!(
                        "the machine asked to sweep `{lens}` and this build cannot — B4 builds \
                         it (#3599). Stopping rather than spinning on a step it cannot take."
                    ),
                );
                return report(durable, &tally);
            }

            LoopStep::Curate => {
                audit::record(
                    durable,
                    Audit::SessionStopped,
                    None,
                    "the machine asked to curate and this build cannot — B6 builds it (#3599).",
                );
                return report(durable, &tally);
            }

            LoopStep::Watch { until } => {
                audit::record(
                    durable,
                    Audit::SessionStopped,
                    None,
                    &format!("nothing left to do; the machine would watch for {until:?}"),
                );
                return report(durable, &tally);
            }
        }
    }
}

/// Pick up pull requests a previous run left open.
///
/// Not fatal if the forge cannot be read: that is the same transient condition
/// the loop is built to wait through, and a later pass picks them up.
fn resume(durable: &Durable, cfg: &super::config::LoopConfig, state: &mut LoopState) {
    match super::deliver::open_prs_for_prefix(cfg.attribution.branch_prefix()) {
        Ok(carried) if !carried.is_empty() => {
            audit::record(
                durable,
                Audit::Resumed,
                None,
                &format!(
                    "{} pull request(s) from an earlier run are still open",
                    carried.len()
                ),
            );
            for pr in carried {
                audit::record(durable, Audit::Resumed, Some(&pr), "carrying it");
                state.carrying.push(CarriedPr {
                    pr: PrRef(pr),
                    // The forge tells us the PR is open, not which issue it
                    // addresses; the association is unknown until re-derived.
                    issue: None,
                    disposition: PrDisposition::Moving,
                });
            }
        }
        Ok(_) => {}
        Err(error) => audit::record(
            durable,
            Audit::Transient,
            None,
            &format!("could not read open pull requests to resume ({error}); continuing"),
        ),
    }
}

/// Mark an issue the loop tried and could not resolve.
fn escalate(
    durable: &Durable,
    provider: &crate::issue_provider::GhIssueProvider,
    cfg: &super::config::LoopConfig,
    key: &stella_protocol::issue::IssueKey,
    why: &str,
) {
    match runtime().block_on(super::backlog::escalate(
        provider,
        key,
        why,
        &cfg.attribution.issue_comment,
    )) {
        Ok(()) => {
            durable.update_stats(|s| s.issues_escalated += 1);
            audit::record(
                durable,
                Audit::Escalated,
                Some(key.as_str()),
                &format!(
                    "labelled `{}` — unresolved, later runs will skip it",
                    stella_autonomy::ESCALATION_LABEL
                ),
            );
        }
        Err(error) => audit::record(
            durable,
            Audit::Transient,
            Some(key.as_str()),
            &format!("could not label it as escalated: {error}"),
        ),
    }
}

/// Place the oldest issue nobody has judged, if there is one.
///
/// Returns `Ok(())` when there was nothing to do as well as when something was
/// placed — a queue with no questions in it is the normal case, not an
/// exception.
///
/// # Why a refusal escalates rather than retries
///
/// A turn that answers outside the declared vocabulary, or declines to answer
/// at all, has told us it cannot place this issue. Leaving it unplaced would
/// mean meeting it again on the very next pass, paying for the same turn, and
/// getting the same refusal — forever, and at the cost of never claiming any
/// work. So it gets the escalation label, which takes it out of the queue and
/// puts it in front of a human. `triaged` is the process-local half of the
/// same guard, covering the window before the label lands.
fn assess_one(
    durable: &Durable,
    root: &std::path::Path,
    provider: &crate::issue_provider::GhIssueProvider,
    cfg: &super::config::LoopConfig,
    spend_limit: Option<f64>,
    triaged: &mut std::collections::HashSet<String>,
) -> Result<(), String> {
    let unassessed = super::backlog::unassessed(provider, &cfg.triage)?;
    let Some(issue) = unassessed.into_iter().find(|u| !triaged.contains(&u.key)) else {
        return Ok(());
    };
    triaged.insert(issue.key.clone());

    audit::record(
        durable,
        Audit::TriageStarted,
        Some(&issue.key),
        &format!("nobody has placed this — assessing it: {}", issue.title),
    );

    // The body, so the turn judges the report rather than the headline. An
    // unreadable body is not a reason to skip the issue; a title-only
    // judgement is still better than leaving it unplaced forever.
    let body = super::backlog::resolve(provider, &issue.key)
        .map(|resolved| resolved.body)
        .unwrap_or_default();

    let prompt = super::triage::prompt(&issue, &body, &cfg.triage);
    let output = super::work::run_turn(root, root, &prompt, spend_limit)?;

    let Some(assessment) = super::triage::parse(&output, &cfg.triage) else {
        super::backlog::escalate_blocking(
            provider,
            &issue.key,
            "triage could not place this issue in the configured vocabulary — \
             it needs a human to label it",
            &cfg.attribution.issue_comment,
        )?;
        audit::record(
            durable,
            Audit::Escalated,
            Some(&issue.key),
            "could not be placed — labelled for a human",
        );
        return Ok(());
    };

    super::triage::apply(
        provider,
        &issue.key,
        &assessment,
        &cfg.attribution.issue_comment,
    )?;
    durable.update_stats(|s| s.issues_triaged += 1);
    audit::record(
        durable,
        Audit::Triaged,
        Some(&issue.key),
        &assessment.reason(),
    );
    Ok(())
}

/// Advance one pull request by exactly one deterministic transition.
///
/// Returns whether it settled — merged, escalated, or handed back — so the
/// caller can stop carrying it.
fn advance(
    policy_blocking: &stella_autonomy::BlockingPolicy,
    pr: &str,
    spent: &mut HashMap<String, Spent>,
    no_review: bool,
    tally: &mut Tally,
    durable: &Durable,
) -> Result<bool, String> {
    let entry = spent.entry(pr.to_owned()).or_default();
    let reading = super::deliver::observe(pr, policy_blocking)?;
    if reading.settled {
        // Nothing left to decide. Reached either because the merge below
        // succeeded and the cleanup after it did not, or because a human got
        // there first — both are this pull request being done.
        audit::record(
            durable,
            Audit::PrMerged,
            Some(pr),
            "already settled on the forge — carrying it no further",
        );
        return Ok(true);
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

    // Counted whether or not it changes the action: waiting on somebody else's
    // broken base is time this loop spent, and a high count is a fact about the
    // repository rather than about the loop.
    if obs.base_ci == CiConclusion::Red {
        durable.update_stats(|s| s.base_broken_waits += 1);
    }

    match transition.action {
        Action::Merge => {
            super::deliver::merge(pr)?;
            tally.merged += 1;
            durable.update_stats(|s| s.prs_merged += 1);
            audit::record(durable, Audit::PrMerged, Some(pr), "merged");
            Ok(true)
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
            Ok(true)
        }
        Action::MarkReady => {
            super::deliver::mark_ready(pr)?;
            audit::record(
                durable,
                Audit::PrObserved,
                Some(pr),
                "ci is green — taken out of draft",
            );
            Ok(false)
        }
        Action::PushFix => {
            // A red that the base already explains is not this pull request's
            // to fix, and `base_conclusion` cannot see it: it compares what is
            // failing *now*, and a check that ran against a base which has
            // since been repaired keeps its old verdict forever. The pull
            // request looks guilty of a failure that no longer exists
            // anywhere.
            //
            // Asking the forge to run it again is the cheapest way to find
            // out, and costs nothing but CI minutes when the answer is "still
            // red" — at which point the fix path below runs on the next poll
            // with the re-run already spent. One per pull request: a loop that
            // re-ran on every red would never reach the fix, and would spin on
            // a genuine failure for as long as the operator left it up.
            if !entry.rerun {
                entry.rerun = true;
                match super::deliver::refresh_against_base(pr) {
                    Ok(()) => {
                        audit::record(
                            durable,
                            Audit::PrObserved,
                            Some(pr),
                            "red against a base that has since gone green — merged the base in for a \
                             fresh verdict before treating it as ours",
                        );
                        return Ok(false);
                    }
                    Err(error) => audit::record(
                        durable,
                        Audit::Transient,
                        Some(pr),
                        &format!("could not ask for a re-run ({error}); treating the red as ours"),
                    ),
                }
            }

            // Counted even though this slice does not author the fix: the
            // ceiling is what stops a loop pushing the same broken change
            // forever, and a counter that only moved on success would never
            // reach it.
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
            Ok(true)
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
            Ok(true)
        }
        Action::Wait => Ok(false),
    }
}

/// What the ranked queue yielded.
///
/// Deliberately not `PartialEq`: a held lease owns a heartbeat thread, so
/// equality on this would be equality on a running thing. Tests match on the
/// key.
#[derive(Debug)]
enum Pick {
    /// The first candidate that is neither already taken nor contended, and
    /// the lease this run now holds on it — `None` when the ledger could not
    /// answer, which fails open like every other probe here.
    Take(String, Option<super::claim::Lease>),
    /// Every candidate was taken or deferred.
    Exhausted,
}

/// The first issue on the ranked queue this run may actually start.
///
/// Pure: no I/O and no clock, so the decision has a test seam of its own rather
/// than only being reachable through `drive`'s network loop. `seen`, `claim`
/// and `deferred` are the three edges — the caller supplies the probe, the
/// ledger write and the audit line, and this supplies the walk.
///
/// **A deferral advances to the next candidate; it does not end the search.**
/// The queue is priority-ordered, so stopping at the first contended issue would
/// hand one peer's branch the power to stall the whole run — which is exactly
/// what the loop did on #3691 by discovering the collision as a `git worktree
/// add` failure and only then moving on (#4002).
///
/// # Why the claim is inside the walk and not after it
///
/// `seen` reads; two loops polling one clone can both read "free" before either
/// writes. `claim` is the single conditional write that decides between them
/// ([`super::claim`]), so a loser must be able to keep walking — treating it as
/// one more deferral, with the same audit line and the same counter, rather
/// than ending the pass on a candidate somebody else got to first.
///
/// It is also what makes the run *visible* to a peer's `seen`: the claim-time
/// worktree probe deliberately cannot see a peer under the loop's own root, so
/// the lease is the only thing left that can say a turn is in flight (#4300).
fn next_claimable(
    ranked: impl IntoIterator<Item = String>,
    taken: impl Fn(&str) -> bool,
    policy: ContentionPolicy,
    seen: impl Fn(&str) -> Contention,
    claim: impl Fn(&str) -> super::claim::Claim,
    mut deferred: impl FnMut(&str, &[String]),
) -> Pick {
    for key in ranked {
        if taken(&key) {
            continue;
        }
        match contention_verdict(policy, &seen(&key)) {
            ContentionVerdict::Proceed => match claim(&key) {
                super::claim::Claim::Granted(lease) => return Pick::Take(key, Some(lease)),
                super::claim::Claim::Unavailable => return Pick::Take(key, None),
                super::claim::Claim::HeldBy(who) => deferred(
                    &key,
                    &[format!(
                        "ledger claim: {} held by {who}",
                        issue_claim_key(&key)
                    )],
                ),
            },
            ContentionVerdict::Defer { evidence } => deferred(&key, &evidence),
        }
    }
    Pick::Exhausted
}

/// What the world looks like, in the shape the machine decides over.
///
/// The bound is expressed as an exhausted queue rather than as a special case
/// in the machine: "there is no more work for me" is something it already knows
/// how to handle, and a second way to stop would be a second definition of
/// stopping.
fn observe(
    root: &std::path::Path,
    provider: &crate::issue_provider::GhIssueProvider,
    base: &str,
    max_issues: u32,
    opened: u32,
) -> LoopObservation {
    // The base is read on every poll rather than cached, because "somebody
    // repaired main" is exactly the event this loop must notice without being
    // told. A latch here would be the thing that stops it self-resuming.
    let base_broken = super::deliver::base_is_broken(base);

    // Nothing else is worth asking while the base is fine, and each of these
    // is a network call.
    if !base_broken {
        return LoopObservation {
            grant_valid: true,
            stop_requested: false,
            budget_exhausted: false,
            queue_depth: max_issues.saturating_sub(opened),
            base_broken: false,
            base_breakage_filed: false,
            base_breakage_issue: None,
            base_fix_contention: Contention::default(),
        };
    }

    let filed = super::backlog::open_base_breakage(provider);
    let contention = filed
        .as_ref()
        .map(|key| super::contention::for_base_fix(root, key))
        .unwrap_or_default();

    LoopObservation {
        grant_valid: true,
        stop_requested: false,
        budget_exhausted: false,
        queue_depth: max_issues.saturating_sub(opened),
        base_broken: true,
        base_breakage_filed: filed.is_some(),
        base_breakage_issue: filed.map(IssueRef),
        base_fix_contention: contention,
    }
}

/// A runtime for the one awaited call a synchronous arm needs.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime is always constructible")
}

fn sleep(secs: u64) {
    std::thread::sleep(std::time::Duration::from_secs(secs));
}

fn report(durable: &Durable, tally: &Tally) -> Result<(), String> {
    audit::record(
        durable,
        Audit::SessionStopped,
        None,
        &format!(
            "{} opened, {} merged, {} escalated — `stella self-driving stats` has the rest",
            tally.opened, tally.merged, tally.escalated
        ),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // Named rather than reached through `super::claim`: inside this module
    // `super` is `drive`, not `self_driving_cmd`, and `crate::ingest_cmd`
    // has a private `Claim` of its own that rustc offers when the path is
    // wrong — a confusing suggestion for a plain scoping slip.
    use crate::self_driving_cmd::claim::Claim;

    /// A ranked queue whose top entry a peer is holding yields the next one,
    /// and says why it skipped.
    ///
    /// The second half is what stops this passing vacuously: the identical
    /// input under [`ContentionPolicy::Proceed`] must take the contended
    /// candidate, so a `next_claimable` that ignored the verdict entirely
    /// would fail the first half rather than satisfy both.
    #[test]
    fn a_candidate_a_peer_is_working_is_skipped_with_evidence() {
        let ranked = || ["3691".to_owned(), "3702".to_owned()];
        let seen = |key: &str| {
            if key == "3691" {
                Contention {
                    local_worktrees: vec!["/tmp/wip-3691-preserved".to_owned()],
                    ..Contention::default()
                }
            } else {
                Contention::default()
            }
        };

        let mut skipped: Vec<(String, Vec<String>)> = Vec::new();
        let pick = next_claimable(
            ranked(),
            |_| false,
            ContentionPolicy::Defer,
            seen,
            |_| Claim::Unavailable,
            |key, evidence| skipped.push((key.to_owned(), evidence.to_vec())),
        );

        assert!(matches!(&pick, Pick::Take(key, _) if key == "3702"));
        assert_eq!(
            skipped,
            vec![(
                "3691".to_owned(),
                vec!["local worktree: /tmp/wip-3691-preserved".to_owned()]
            )]
        );

        let mut skipped_under_proceed: Vec<String> = Vec::new();
        let pick = next_claimable(
            ranked(),
            |_| false,
            ContentionPolicy::Proceed,
            seen,
            |_| Claim::Unavailable,
            |key, _| skipped_under_proceed.push(key.to_owned()),
        );

        assert!(matches!(&pick, Pick::Take(key, _) if key == "3691"));
        assert!(skipped_under_proceed.is_empty());
    }

    /// A peer that took the lease *between* the probe and the write is one
    /// more deferral, not the end of the pass.
    ///
    /// This is the race the read-only probe structurally cannot close — both
    /// loops can read "free" before either writes — so the loser has to keep
    /// walking the queue, with the same audit line and the same counter as any
    /// other contention (#4300).
    #[test]
    fn a_candidate_whose_lease_a_peer_won_is_skipped_like_any_other_deferral() {
        let mut skipped: Vec<(String, Vec<String>)> = Vec::new();
        let pick = next_claimable(
            ["1".to_owned(), "2".to_owned()],
            |_| false,
            ContentionPolicy::Defer,
            |_| Contention::default(),
            |key| {
                if key == "1" {
                    Claim::HeldBy("self-driving:99999".to_owned())
                } else {
                    Claim::Unavailable
                }
            },
            |key, evidence| skipped.push((key.to_owned(), evidence.to_vec())),
        );

        assert!(matches!(&pick, Pick::Take(key, None) if key == "2"));
        assert_eq!(
            skipped,
            vec![(
                "1".to_owned(),
                vec!["ledger claim: issue:1 held by self-driving:99999".to_owned()]
            )]
        );
    }

    /// A queue whose every candidate is contended is exhausted, not merely
    /// stalled on the first one.
    #[test]
    fn a_fully_contended_queue_defers_every_candidate() {
        let mut skipped: Vec<String> = Vec::new();
        let pick = next_claimable(
            ["1".to_owned(), "2".to_owned()],
            |_| false,
            ContentionPolicy::Defer,
            |_| Contention {
                ledger_claims: vec!["fleet-run-7".to_owned()],
                ..Contention::default()
            },
            |_| Claim::Unavailable,
            |key, _| skipped.push(key.to_owned()),
        );

        assert!(matches!(pick, Pick::Exhausted));
        assert_eq!(skipped, vec!["1".to_owned(), "2".to_owned()]);
    }
}
