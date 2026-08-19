//! How an issue ends, and what has to be true first.
//!
//! Closing an issue is the loop's most consequential write: it is the one that
//! makes work *disappear* from the queue. A loop that closes carelessly does
//! not look broken — it looks productive, right up until someone notices the
//! backlog is smaller than the work remaining.
//!
//! So the closure kinds are a closed vocabulary, and each carries what a reader
//! would need a month later.
//!
//! # The rule that makes "nothing left behind" structural
//!
//! [`Closure::Partial`] — *mostly fixed* — is the dangerous one, because it is
//! the honest description of most real work and the easiest thing to round up
//! to "done". [`check`] **refuses a partial closure that names no remaining
//! issues.** Not warns: refuses. The remainder must already exist as its own
//! tracked issues before the original may close, so the queue never shrinks by
//! more than the work actually finished.
//!
//! `a_partial_closure_with_no_filed_remainder_is_refused` is the witness, and
//! it is the whole reason this type is not a bare string reason.
//!
//! # Why `NotPlanned` is a separate kind rather than a flag
//!
//! A tracker distinguishes *completed* from *not planned*, and the distinction
//! is load-bearing for anything that reads history later: a regression sweep
//! re-checks what was **fixed**, and re-running a witness for an issue that was
//! declined as stale would be measuring nothing. Collapsing the two into one
//! "closed" would make the loop's own past claims unreadable.

use serde::{Deserialize, Serialize};

/// How an issue is being closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "closure")]
pub enum Closure {
    /// It was fixed.
    ///
    /// `evidence` names what establishes that — a pull request, a test, a
    /// commit. Required rather than optional: a closure with no evidence
    /// cannot be re-checked later, and the count of unsweepable closures is
    /// the direct, unflattering measure of how often *done* was a claim rather
    /// than a proof.
    Fixed {
        /// What establishes the fix.
        evidence: String,
    },

    /// It is no longer worth doing — stale, superseded, or declined.
    ///
    /// Closed as *not planned* on the tracker, which is a different state from
    /// completed and must stay so; see the module docs.
    NotPlanned {
        /// Why it is no longer worth doing.
        reason: String,
    },

    /// Part of it was fixed, and the rest is now tracked separately.
    ///
    /// The remainder is a list of issue keys that **already exist**. This kind
    /// cannot be constructed honestly before the remainder is filed, and
    /// [`check`] enforces that rather than trusting it.
    Partial {
        /// What was actually finished.
        done: String,
        /// The issues now carrying what was not.
        remaining: Vec<String>,
    },
}

/// Why a closure was refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "refusal")]
pub enum ClosureRefusal {
    /// A partial closure named no remaining issues.
    ///
    /// The one that matters: closing "mostly fixed" with nothing tracking the
    /// rest is how work vanishes from a backlog without being done.
    PartialWithNoRemainder,
    /// A closure kind that requires prose supplied none.
    ///
    /// An empty reason is not a reason, and a closed issue whose comment says
    /// nothing is worse than an open one — it looks handled.
    MissingRationale {
        /// Which field was empty.
        field: &'static str,
    },
}

/// Whether this closure may proceed.
///
/// Pure and total. The tracker write happens only after this returns `Ok`, so
/// the discipline is not a habit a caller can forget.
///
/// # Errors
///
/// [`ClosureRefusal`] names what is missing, in terms the caller can fix.
pub fn check(closure: &Closure) -> Result<(), ClosureRefusal> {
    match closure {
        Closure::Fixed { evidence } if evidence.trim().is_empty() => {
            Err(ClosureRefusal::MissingRationale { field: "evidence" })
        }
        Closure::NotPlanned { reason } if reason.trim().is_empty() => {
            Err(ClosureRefusal::MissingRationale { field: "reason" })
        }
        Closure::Partial { remaining, .. } if remaining.iter().all(|key| key.trim().is_empty()) => {
            Err(ClosureRefusal::PartialWithNoRemainder)
        }
        Closure::Partial { done, .. } if done.trim().is_empty() => {
            Err(ClosureRefusal::MissingRationale { field: "done" })
        }
        _ => Ok(()),
    }
}

/// Whether the tracker should record this as *completed* or *not planned*.
///
/// A partial closure is **completed**: the work that was in scope is finished,
/// and the rest is tracked elsewhere. Recording it as *not planned* would tell
/// a later reader the issue was declined, which is the opposite of what
/// happened.
#[must_use]
pub fn tracker_state(closure: &Closure) -> &'static str {
    match closure {
        Closure::Fixed { .. } | Closure::Partial { .. } => "completed",
        Closure::NotPlanned { .. } => "not_planned",
    }
}

/// The comment left on the issue as it closes.
///
/// Every kind leaves one. A closure with no comment forces the next reader to
/// reconstruct the decision from a diff, which they will not do.
#[must_use]
pub fn receipt(closure: &Closure) -> String {
    match closure {
        Closure::Fixed { evidence } => format!("Closing as fixed.\n\nEvidence: {evidence}"),
        Closure::NotPlanned { reason } => {
            format!("Closing as not planned.\n\nReason: {reason}")
        }
        Closure::Partial { done, remaining } => {
            let list = remaining
                .iter()
                .filter(|k| !k.trim().is_empty())
                .map(|k| format!("- #{}", k.trim_start_matches('#')))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "Closing as fixed in part.\n\nDone: {done}\n\nWhat is left is tracked \
                 separately, so nothing here is dropped:\n{list}"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The witness.** "Mostly fixed" is the honest description of most real
    /// work and the easiest thing to round up to done. It is refused outright
    /// unless the remainder already exists as its own issues.
    #[test]
    fn a_partial_closure_with_no_filed_remainder_is_refused() {
        let sloppy = Closure::Partial {
            done: "fixed the parser".into(),
            remaining: Vec::new(),
        };
        assert_eq!(check(&sloppy), Err(ClosureRefusal::PartialWithNoRemainder));

        // Whitespace is not a remainder either.
        let sloppier = Closure::Partial {
            done: "fixed the parser".into(),
            remaining: vec!["  ".into(), "".into()],
        };
        assert_eq!(
            check(&sloppier),
            Err(ClosureRefusal::PartialWithNoRemainder)
        );
    }

    /// The other half — with the remainder filed, it proceeds, and the comment
    /// names every issue carrying what was left.
    #[test]
    fn a_partial_closure_that_filed_its_remainder_proceeds_and_links_it() {
        let honest = Closure::Partial {
            done: "fixed the parser".into(),
            remaining: vec!["4001".into(), "#4002".into()],
        };
        assert_eq!(check(&honest), Ok(()));

        let comment = receipt(&honest);
        assert!(comment.contains("- #4001"), "{comment}");
        assert!(comment.contains("- #4002"), "{comment}");
        assert!(comment.contains("nothing here is dropped"), "{comment}");
    }

    /// A partial closure records *completed*, not *not planned*: the scoped
    /// work finished, and saying otherwise would tell a later reader it was
    /// declined.
    #[test]
    fn a_partial_closure_is_recorded_as_completed() {
        let honest = Closure::Partial {
            done: "some".into(),
            remaining: vec!["1".into()],
        };
        assert_eq!(tracker_state(&honest), "completed");
    }

    /// Fixed and not-planned are different tracker states, and stay different:
    /// a regression sweep re-checks what was fixed, and re-running a witness
    /// for something declined as stale would measure nothing.
    #[test]
    fn fixed_and_not_planned_are_distinct_tracker_states() {
        let fixed = Closure::Fixed {
            evidence: "pr #3985".into(),
        };
        let declined = Closure::NotPlanned {
            reason: "superseded by the config collapse".into(),
        };
        assert_eq!(tracker_state(&fixed), "completed");
        assert_eq!(tracker_state(&declined), "not_planned");
    }

    /// A closure with no rationale is refused: a closed issue whose comment
    /// says nothing is worse than an open one, because it looks handled.
    #[test]
    fn every_kind_requires_its_rationale() {
        assert_eq!(
            check(&Closure::Fixed {
                evidence: "   ".into()
            }),
            Err(ClosureRefusal::MissingRationale { field: "evidence" })
        );
        assert_eq!(
            check(&Closure::NotPlanned {
                reason: String::new()
            }),
            Err(ClosureRefusal::MissingRationale { field: "reason" })
        );
        assert_eq!(
            check(&Closure::Partial {
                done: " ".into(),
                remaining: vec!["1".into()]
            }),
            Err(ClosureRefusal::MissingRationale { field: "done" })
        );
    }

    /// Every kind leaves a comment — a closure the next reader has to
    /// reconstruct from a diff is one they will not reconstruct.
    #[test]
    fn every_kind_leaves_a_receipt() {
        for closure in [
            Closure::Fixed {
                evidence: "pr #1".into(),
            },
            Closure::NotPlanned {
                reason: "stale".into(),
            },
            Closure::Partial {
                done: "half".into(),
                remaining: vec!["2".into()],
            },
        ] {
            assert!(!receipt(&closure).trim().is_empty(), "{closure:?}");
        }
    }

    #[test]
    fn a_closure_round_trips_through_json() {
        let c = Closure::Partial {
            done: "half".into(),
            remaining: vec!["2".into()],
        };
        let json = serde_json::to_string(&c).expect("serialize");
        let back: Closure = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(c, back);
    }
}
