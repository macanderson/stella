// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! How a run ends, and why each ending is spelled differently.
//!
//! A supervisor reading the journal has to tell them apart. A run that reached
//! its issue bound has said nothing about the backlog; a run that spent its
//! ceiling has said nothing about the work; a signalled run has said nothing at
//! all. Collapsing any two is how a scheduler learns to stop scheduling.
//!
//! Every ending writes one `SessionStopped` record and names the tally, because
//! the alternative is a reader inferring the ending from the journal going
//! quiet — which is what the Observatory had to do for sixteen of twenty runs
//! before #4361.
//!
//! Its own module beside [`super`] rather than more of `drive.rs`, which the
//! file-size ratchet holds at 1500 lines (AGENTS.md, "God files — plan around
//! them, never into them"). `settlement.rs` is the same split, one boundary
//! earlier.

use super::super::audit::{self, Action as Audit};
use super::super::budget::{Exhausted, RunBudget};
use super::super::state::LoopState as Durable;

/// What one run of the loop did, for the closing line.
#[derive(Debug, Default)]
pub(super) struct Tally {
    pub opened: u32,
    pub merged: u32,
    pub escalated: u32,
}

/// End the run because the operating system said so, with the record that a
/// killed process could never write (#4361).
///
/// Returns `Err` so `main` reports the shell-conventional `128 + signum` —
/// `stella self-driving drive` cut short by SIGTERM must be distinguishable
/// from one that finished, the same contract every other door honours
/// ([`crate::signals`]).
pub(super) fn stopped_by_signal(
    durable: &Durable,
    tally: &Tally,
    signal: crate::signals::Interrupt,
) -> Result<(), String> {
    let reason = signal.reason();
    audit::record(
        durable,
        Audit::SessionStopped,
        None,
        &format!(
            "{reason} by a signal — {} opened, {} merged, {} escalated. Every claim this run \
             held is released.",
            tally.opened, tally.merged, tally.escalated
        ),
    );
    crate::signals::note_interrupt(signal);
    Err(reason.to_owned())
}

/// End a run that has spent its ceiling (#4353).
///
/// Reported as *budget reached*, never as *finished* — the same distinction
/// `--max-issues` already draws, and for the same reason: a run that stopped
/// because it ran out of money has told you nothing about whether the backlog
/// is done, and a supervisor that read the two endings alike would stop
/// scheduling work the moment one run hit its cap.
///
/// `Ok`, not `Err`. The ceiling was honoured, which is the run doing what it
/// was asked; a non-zero exit would make an operator's own budget read as a
/// failure to their scheduler.
pub(super) fn budget_reached(
    durable: &Durable,
    tally: &Tally,
    out: Exhausted,
) -> Result<(), String> {
    audit::record(
        durable,
        Audit::SessionStopped,
        None,
        &format!(
            "budget reached — ${:.2} of the run's ${:.2} spend limit is gone, so no further \
             turn was started. {} opened, {} merged, {} escalated. Raise --spend-limit to \
             carry on; every claim this run held is released.",
            out.spent, out.cap, tally.opened, tally.merged, tally.escalated
        ),
    );
    Ok(())
}

/// End a run that reached its issue bound, or ran out of work.
pub(super) fn report(durable: &Durable, tally: &Tally, budget: &RunBudget) -> Result<(), String> {
    audit::record(
        durable,
        Audit::SessionStopped,
        None,
        &format!(
            "{} opened, {} merged, {} escalated{} — `stella self-driving stats` has the rest",
            tally.opened,
            tally.merged,
            tally.escalated,
            // What the run actually cost, from the turns' own accounting
            // (#4353). Printed whether or not a ceiling was set: an operator
            // deciding what to set next time needs the number from the run that
            // did not have one.
            format_args!(", ${:.2} spent", budget.spent()),
        ),
    );
    Ok(())
}
