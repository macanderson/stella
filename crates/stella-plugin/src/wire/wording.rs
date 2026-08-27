// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! How a verdict's vocabulary reads to a person.
//!
//! [`UnmetBecause`](super::UnmetBecause) has worded itself since it was
//! written; its sibling [`UndecidedReason`] did not, so the two places that
//! print one reached for `{:?}` and a user watching a run read
//! `MeasurementMissing { requirement: "fast", measurement: "p50" }`. A verdict
//! that words its failures and debug-prints its abstentions makes the
//! abstention look like the internal error it is not — and the abstention is
//! the answer a reader is *least* equipped to interpret, because it is about
//! the instrument rather than about the work.
//!
//! A sibling file rather than more lines in `wire.rs`, which sits a few lines
//! under the 1500-line ratchet: the wire contract itself has to stay inside it,
//! and wordings grow independently of the vocabulary they word.

use std::fmt;

use super::UndecidedReason;

impl fmt::Display for UndecidedReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoOracle => {
                f.write_str("requirements are declared but no [oracle] establishes them")
            }
            Self::Undecidable { requirement } => {
                write!(f, "no check and no flip can decide \"{requirement}\"")
            }
            Self::MeasurementMissing {
                requirement,
                measurement,
            } => write!(
                f,
                "the oracle reported no \"{measurement}\", which the check on \
                 \"{requirement}\" reads"
            ),
            Self::UnreadableCheck {
                requirement,
                reason,
            } => write!(
                f,
                "the check on \"{requirement}\" could not be read: {reason}"
            ),
            Self::FlipUnobservable => f.write_str(
                "the flip evidence was unavailable — the witness could not be run, or nothing \
                 was gathered",
            ),
            Self::WitnessUnsatisfiable => f.write_str(
                "the witness failed identically before and after, so it discriminates nothing",
            ),
            Self::TamperUnchecked => f.write_str(
                "the tamper policy demands a snapshot and none was taken, so the flip can be \
                 neither credited nor refused",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every reason words itself, and none of them leaks the `Debug` shape a
    /// reader cannot use. The enumeration is the point: a reason added without
    /// a wording would fall to a `_` arm — there is none, so it is an `E0004` —
    /// but a reason worded as `format!("{self:?}")` would pass a spot check on
    /// any other arm.
    #[test]
    fn every_undecided_reason_reads_as_a_sentence() {
        for reason in [
            UndecidedReason::NoOracle,
            UndecidedReason::Undecidable {
                requirement: "proven".into(),
            },
            UndecidedReason::MeasurementMissing {
                requirement: "fast".into(),
                measurement: "p50".into(),
            },
            UndecidedReason::UnreadableCheck {
                requirement: "fast".into(),
                reason: "expected a rule of the form \"<measurement> <op> <number>\"".into(),
            },
            UndecidedReason::FlipUnobservable,
            UndecidedReason::WitnessUnsatisfiable,
            UndecidedReason::TamperUnchecked,
        ] {
            let worded = reason.to_string();
            assert!(
                !worded.contains('{') && !worded.contains("::"),
                "{reason:?} words itself as a Debug rendering: {worded}"
            );
            assert!(
                worded.split_whitespace().count() >= 4,
                "{reason:?} is not a sentence: {worded}"
            );
        }
    }
}
