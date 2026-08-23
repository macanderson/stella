// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Whether a wrapper's conclusion decides the run's exit status.
//!
//! Until #3554 it never did: `run_wrapped` returned the last round's turn
//! result, so a `DispatchReport` whose outcome was [`Outcome::Unmet`] printed
//! to stderr and exited `0`. A delivery gate that must not ship unproven work
//! had no way to read a plugin's refusal, and `--require-verified` — the flag
//! that shape of user reaches for — was wired to the deleted staged
//! pipeline's ladder and is refused unconditionally now (#3865).
//!
//! The default stays exit `0`, and that is the decision this module encodes
//! rather than an oversight it works around: installing a third party's
//! manifest must not, by itself, gain the power to fail somebody's build.
//! `--require-verdict` is the opt-in that grants it.
//!
//! Pure over the outcome, so both directions are assertable without a session
//! (`tests/verdict_gate.rs`). Split out of `wrapper_plugin.rs` rather than
//! grown inside it: that file sits under the 1500-line ratchet.

use stella_plugin::{Outcome, UndecidedReason};

/// The failure `--require-verdict` owes the caller, or `None` to exit on the
/// turn's own result.
///
/// Anything other than [`Outcome::Met`] fails under the flag, `Undecided`
/// included. A gate whose whole purpose is "do not ship work the wrapper did
/// not vouch for" cannot pass on "nothing decided it either way" — that is
/// precisely the case where nothing vouched. The message names which of the
/// two it was, because the remedies differ: `Unmet` is work to redo,
/// `Undecided` is usually a manifest that declares requirements no oracle
/// establishes.
pub(super) fn verdict_refusal(require_verdict: bool, outcome: &Outcome) -> Option<String> {
    if !require_verdict {
        return None;
    }
    match outcome {
        Outcome::Met { .. } => None,
        Outcome::Unmet { unmet, .. } => {
            let named = unmet
                .iter()
                .map(|requirement| requirement.requirement.as_str())
                .collect::<Vec<_>>();
            Some(format!(
                "--require-verdict: the wrapper left {} unmet ({})",
                plural(named.len(), "requirement"),
                if named.is_empty() {
                    "none named".to_string()
                } else {
                    named.join(", ")
                }
            ))
        }
        Outcome::Undecided { reason } => Some(format!(
            "--require-verdict: the wrapper reached no verdict ({})",
            undecided(reason)
        )),
    }
}

/// `1 requirement` / `2 requirements`.
fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("{count} {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// Why nothing decided, in the words the remedy depends on.
fn undecided(reason: &UndecidedReason) -> String {
    match reason {
        UndecidedReason::NoOracle => {
            "it declares requirements and no [oracle] to establish them".to_string()
        }
        UndecidedReason::Undecidable { requirement } => {
            format!("nothing can decide `{requirement}`")
        }
        UndecidedReason::MeasurementMissing {
            requirement,
            measurement,
        } => format!("`{requirement}` reads `{measurement}`, which the oracle did not report"),
        UndecidedReason::UnreadableCheck {
            requirement,
            reason,
        } => format!("`{requirement}`'s check could not be read: {reason}"),
        UndecidedReason::FlipUnobservable => {
            "the witness could not be run, so there is no flip to read".to_string()
        }
        UndecidedReason::WitnessUnsatisfiable => {
            "the witness failed identically before and after, so it discriminates nothing"
                .to_string()
        }
        UndecidedReason::TamperUnchecked => {
            "the declared tamper policy wants a snapshot and none was taken".to_string()
        }
    }
}
