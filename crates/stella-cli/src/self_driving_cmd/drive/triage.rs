// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Placing an issue, or handing it to a human.
//!
//! The loop's triage half: [`assess_one`] asks a turn where one unassessed
//! issue belongs and applies the answer, and [`escalate`] is what happens when
//! that turn cannot say — the issue leaves the queue and goes in front of a
//! person. The two belong together because they are the same decision's two
//! outcomes, and neither has anything to do with advancing a pull request,
//! which is the rest of [`super`].
//!
//! Split out of `drive.rs` at the gate's 1500-line ceiling (#5225). The seam is
//! the concern, not the line count: `drive.rs` keeps the loop and the pull
//! request transitions, and this file keeps the backlog.

use super::{Audit, Durable, audit, runtime};

/// Mark an issue the loop tried and could not resolve.
pub(super) fn escalate(
    durable: &Durable,
    settings: &crate::settings::Settings,
    provider: &crate::issue_provider::GhIssueProvider,
    cfg: &crate::self_driving_cmd::config::LoopConfig,
    key: &stella_protocol::issue::IssueKey,
    why: &str,
) {
    match runtime().block_on(crate::self_driving_cmd::backlog::escalate(
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
            super::notify::escalated(&durable.repo_root, settings, key.as_str(), why);
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
pub(super) fn assess_one(
    durable: &Durable,
    root: &std::path::Path,
    provider: &crate::issue_provider::GhIssueProvider,
    cfg: &crate::self_driving_cmd::config::LoopConfig,
    budget: &mut crate::self_driving_cmd::budget::RunBudget,
    triaged: &mut std::collections::HashSet<String>,
) -> Result<(), String> {
    let unassessed = crate::self_driving_cmd::backlog::unassessed(provider, &cfg.triage)?;
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
    let resolved = crate::self_driving_cmd::backlog::resolve(provider, &issue.key).ok();
    let body = resolved
        .as_ref()
        .map(|full| full.body.clone())
        .unwrap_or_default();

    // An issue this loop sized, back again as unassessed, was not un-judged.
    // The triage guard stripped its priority label: the guard takes
    // priorities only from logins it allows, and the runner's is not one.
    // Re-triaging would relabel, get stripped, and loop forever. So it
    // escalates once, and a human fixes the guard's login list.
    // `crate::self_driving_cmd::triage::assessment_stripped` says why the
    // size label is the tell.
    if let Some(full) = &resolved
        && crate::self_driving_cmd::triage::assessment_stripped(full, &cfg.triage)
    {
        crate::self_driving_cmd::backlog::escalate_blocking(
            provider,
            &issue.key,
            "this loop placed the issue, and the triage guard stripped the \
             priority it wrote — the drive runner's login is not on the \
             guard's list. Add it to TRIAGE_LOGINS in the guard workflow, or \
             set the priority from an allowed login. Then remove the \
             escalation label to requeue",
            &cfg.attribution.issue_comment,
        )?;
        audit::record(
            durable,
            Audit::Escalated,
            Some(&issue.key),
            "placed before, then stripped by the triage guard — escalated \
             once rather than re-triaged forever",
        );
        return Ok(());
    }

    let prompt = crate::self_driving_cmd::triage::prompt(&issue, &body, &cfg.triage);
    let output = crate::self_driving_cmd::work::run_turn(root, root, &prompt, budget)?;

    let Some(assessment) = crate::self_driving_cmd::triage::parse(&output, &cfg.triage) else {
        crate::self_driving_cmd::backlog::escalate_blocking(
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

    crate::self_driving_cmd::triage::apply(
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

    // The assessment passed, so readiness is decidable now, and deciding it
    // costs no model call. A failure here is transient like any other
    // tracker write. The placed labels already stand, and the next reader
    // re-derives readiness from the same facts.
    match crate::self_driving_cmd::triage::flip_ready(provider, &issue.key, &body) {
        Ok(true) => audit::record(
            durable,
            Audit::Triaged,
            Some(&issue.key),
            "no open blocker remains — flipped `status:blocked` to `status:ready`",
        ),
        Ok(false) => {}
        Err(error) => audit::record(
            durable,
            Audit::Transient,
            Some(&issue.key),
            &format!("could not decide readiness ({error}); the labels stand"),
        ),
    }
    Ok(())
}
