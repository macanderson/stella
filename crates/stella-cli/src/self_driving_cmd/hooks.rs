// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The loop's own hook dispatch — `PreIssueWork` and `PostIssueWork` (#3599).
//!
//! Every other hook point in this workspace is dispatched by the engine's
//! driver, from inside a turn. These two are dispatched from here, because
//! they bracket a turn rather than sitting inside one: `PreIssueWork` fires
//! before the worktree exists and before any model call, and `PostIssueWork`
//! fires after the whole work unit is done.
//!
//! # Why `PreIssueWork` can veto, and why the veto is a *skip*
//!
//! A person needs a way to keep an agent off work they have not finished
//! thinking about — an issue that is still being specified, one a colleague has
//! started, a release freeze — and the tracker is where that intent already
//! lives (a label, an assignee, a milestone). Without a gate here the only
//! options are editing the backlog query in Rust or stopping the loop
//! entirely.
//!
//! A `deny` therefore skips **this issue** and lets the loop continue to the
//! next. It is not an error and not a stop: the loop working through a backlog
//! and stepping over one held item is the behaviour being asked for. A hook
//! that means "stop everything" has to say so to its operator, because nothing
//! here can tell the two intents apart from a `deny` alone.
//!
//! # It fails closed, which is the opposite of the usual choice
//!
//! A hook that could not be evaluated — it never ran, timed out, or exited
//! non-zero — **skips the issue**. Most gates in this codebase fail open, so
//! this is worth the sentence:
//!
//! The purpose of this hook is to withhold work. If it cannot run, the one
//! thing we know is that we do not know whether this issue is held — and the
//! two outcomes are not symmetric. Skipping an issue that was in fact free
//! costs one cycle and is corrected by the next run. Working an issue that was
//! in fact held is unrecoverable in the way that matters: a branch, a pull
//! request, and a comment on an issue somebody deliberately fenced off.
//!
//! This mirrors `PreToolUse`, where a non-zero exit blocks the tool rather
//! than waving it through.
//!
//! # `require_approval` is a skip, not a prompt
//!
//! The loop is the unattended path by construction; there is no human at the
//! keyboard to answer. Rendering a prompt nobody sees and then proceeding
//! would be strictly worse than declining, so the reason is reported and the
//! issue is skipped — an operator reading the loop's output learns the same
//! thing they would have been asked.

use std::path::Path;

use stella_core::bus::HookDecision;
use stella_core::hooks::{HookEvent, HookPayload, decision::run_decision_hooks};
pub(super) use stella_core::hooks::{HookIssueInfo, HookIssueOutcome};
use stella_tools::hook_runner::ShellHookRunner;

use crate::settings::Settings;

/// Whether the loop may work this issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WorkGate {
    /// No hook objected — or none is registered, which is the common case.
    Allow,
    /// Skip this issue and move to the next one, for this reason.
    Skip {
        /// What to tell the operator. Always non-empty: a skip with no stated
        /// reason is indistinguishable from a bug in the loop.
        reason: String,
    },
}

/// Run the `PreIssueWork` chain and fold it into a gate.
///
/// Returns [`WorkGate::Allow`] immediately when nothing is registered, so a
/// workspace with no hooks pays no runtime at all — not even a runtime start.
pub(super) fn before_issue_work(
    root: &Path,
    settings: &Settings,
    issue: &HookIssueInfo,
) -> WorkGate {
    let Some(hooks) = registered(settings, HookEvent::PreIssueWork) else {
        return WorkGate::Allow;
    };
    let payload = HookPayload::pre_issue_work(root.display().to_string(), issue.clone());

    let run = match dispatch(&payload, hooks) {
        Ok(run) => run,
        // The runtime itself could not be started. Fails closed with the rest,
        // per this module's header: the hook exists to withhold work, so "we
        // could not ask" is not "go ahead".
        Err(reason) => {
            return WorkGate::Skip {
                reason: format!("the PreIssueWork hook could not be evaluated: {reason}"),
            };
        }
    };

    for diagnostic in &run.diagnostics {
        eprintln!("  ! PreIssueWork: {diagnostic}");
    }
    if !run.output.trim().is_empty() {
        eprintln!("{}", run.output.trim_end());
    }

    match run.evaluation {
        // A chain that only modified ends `Allow`. There is nothing here for a
        // `modify` to rewrite — the payload is not a tool input — so it is
        // reported as a diagnostic above and otherwise carries no effect.
        Ok(HookDecision::Allow | HookDecision::Modify { .. }) => WorkGate::Allow,
        Ok(HookDecision::Deny(denial)) => WorkGate::Skip {
            reason: reason_or(denial.to_string(), "a PreIssueWork hook denied it"),
        },
        Ok(HookDecision::RequireApproval { reason }) => WorkGate::Skip {
            reason: reason_or(
                reason,
                "a PreIssueWork hook asked for approval, and no human is driving this loop",
            ),
        },
        Err(failure) => WorkGate::Skip {
            reason: format!("the PreIssueWork hook could not be evaluated: {failure}"),
        },
    }
}

/// Run the `PostIssueWork` chain. Reports; never blocks.
///
/// The outcome has already happened, so there is no decision to fold and a
/// failure here is a warning rather than a gate. It is still *reported* — a
/// notifier that silently stopped notifying is the shape invariant #10 exists
/// to prevent, on the consumption side.
pub(super) fn after_issue_work(
    root: &Path,
    settings: &Settings,
    issue: &HookIssueInfo,
    outcome: HookIssueOutcome,
) {
    let Some(hooks) = registered(settings, HookEvent::PostIssueWork) else {
        return;
    };
    let payload = HookPayload::post_issue_work(root.display().to_string(), issue.clone(), outcome);

    match dispatch(&payload, hooks) {
        Ok(run) => {
            for diagnostic in &run.diagnostics {
                eprintln!("  ! PostIssueWork: {diagnostic}");
            }
            if !run.output.trim().is_empty() {
                eprintln!("{}", run.output.trim_end());
            }
            if let Err(failure) = run.evaluation {
                eprintln!("  ! PostIssueWork hook failed: {failure}");
            }
        }
        Err(reason) => eprintln!("  ! PostIssueWork hook could not be run: {reason}"),
    }
}

/// The workspace's hooks, but only when something is registered for `event`.
///
/// The `None` arm is what keeps an unhooked workspace free: no runtime is
/// built and no payload is serialized. Project-scope hooks are already gated
/// by [`Settings::load`] on `STELLA_TRUST_PROJECT`, so a cloned repository
/// cannot make the loop run its commands — this reads the merged result rather
/// than re-deriving that gate, which would be a second copy of it.
fn registered(settings: &Settings, event: HookEvent) -> Option<&stella_core::hooks::Hooks> {
    let hooks = settings.hooks.as_ref()?;
    (!hooks.matchers_for(event).is_empty()).then_some(hooks)
}

/// Run one chain on a runtime of its own.
///
/// A `current_thread` runtime built per dispatch, matching
/// `self_driving_cmd`'s issue-provider call: these verbs are otherwise
/// synchronous, and a hook chain is a handful of bounded subprocesses rather
/// than something that needs the multi-threaded scheduler.
fn dispatch(
    payload: &HookPayload,
    hooks: &stella_core::hooks::Hooks,
) -> Result<stella_core::hooks::decision::DecisionRun, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start a runtime for the hook: {error}"))?;
    Ok(runtime.block_on(run_decision_hooks(&ShellHookRunner, Some(hooks), payload)))
}

/// A hook's own words when it gave any, and a description of what happened
/// when it did not.
///
/// A blank reason is the one thing this must not pass through: the operator
/// reads it to decide whether the skip was intended.
fn reason_or(reason: String, fallback: &str) -> String {
    if reason.trim().is_empty() {
        fallback.to_string()
    } else {
        reason
    }
}

#[cfg(test)]
mod tests;
