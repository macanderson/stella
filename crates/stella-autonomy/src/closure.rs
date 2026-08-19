//! How an issue ends, and what has to be true first.
//!
//! Closing an issue is the loop's most consequential write: it is the one that
//! makes work *disappear* from the queue. A loop that closes carelessly does
//! not look broken — it looks productive, right up until someone notices the
//! backlog is smaller than the work remaining.
//!
//! So every closure is a **closed vocabulary plus a citation**, and neither
//! half is optional.
//!
//! # Evidence is cited, never asserted
//!
//! [`Citation`] is typed rather than a free string, and the kinds are not
//! interchangeable. A fix is established by the **pull request** that carried
//! it, or — in an all-hands P0 where a change lands directly — by the
//! **commit**. A decision *not* to do something is established by an
//! **authority**: a document, or a context record. Those are different claims
//! and they are evidenced in different currencies.
//!
//! [`check`] enforces the pairing, so "closed as won't fix, reason: stale" with
//! nothing behind it is not expressible. A refusal with no authority is an
//! opinion wearing a decision's clothes, and the next person to hit the same
//! problem has no way to find out who decided or why.
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
//! # Three kinds, because a tracker has three
//!
//! *Completed*, *not planned*, *duplicate* — GitHub's own close reasons, and
//! the distinctions the loop acts on. A regression sweep re-checks what was
//! **completed**; re-running a witness for something declined as stale, or
//! folded into another issue, would measure nothing.
//!
//! [`resolution_of`] returns Stella's **canonical** name, which the active
//! provider's vocabulary spells in that tracker's words
//! (`stella_protocol::issue::Vocabulary`). Returning the spelling here would
//! bake one tracker's words into the pure machine.

use serde::{Deserialize, Serialize};

/// What establishes a claim about an issue.
///
/// Typed rather than a string because the kinds are not interchangeable, and
/// [`check`] refuses a mismatch — see the module docs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "cite")]
pub enum Citation {
    /// The pull request that carried the change. The ordinary path.
    PullRequest {
        /// Its number or key, as the forge spells it.
        key: String,
    },
    /// The commit that carried it.
    ///
    /// For the all-hands case where a fix lands directly on the branch rather
    /// than through a pull request — a P0 where waiting for review is the
    /// larger risk. Recorded distinctly so an audit can see how often that
    /// path was taken.
    Commit {
        /// The sha.
        sha: String,
    },
    /// A document, by its stable id — `doc:backlog-self-driving`.
    ///
    /// Cited by id rather than path, because a path citation breaks the moment
    /// the file moves and this one has to survive being read years later.
    Document {
        /// The document id.
        doc_id: String,
    },
    /// A context record, by lineage id.
    ///
    /// The steering plane a workspace changes by retiring records and adding
    /// new ones, so a decision cited this way stays traceable to the authority
    /// that made it — and stops being in force when that authority is retired.
    ContextRecord {
        /// The record's lineage id.
        lineage_id: String,
    },
}

impl Citation {
    /// Whether this citation is empty of the thing it is supposed to name.
    fn is_blank(&self) -> bool {
        match self {
            Self::PullRequest { key } => key.trim().is_empty(),
            Self::Commit { sha } => sha.trim().is_empty(),
            Self::Document { doc_id } => doc_id.trim().is_empty(),
            Self::ContextRecord { lineage_id } => lineage_id.trim().is_empty(),
        }
    }

    /// Whether this evidences that work *happened*.
    fn is_change(&self) -> bool {
        matches!(self, Self::PullRequest { .. } | Self::Commit { .. })
    }

    /// Whether this evidences a *decision*.
    fn is_authority(&self) -> bool {
        matches!(self, Self::Document { .. } | Self::ContextRecord { .. })
    }

    /// How the citation reads in a comment.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::PullRequest { key } => {
                format!("#{}", key.trim().trim_start_matches('#'))
            }
            Self::Commit { sha } => format!("commit {}", sha.trim()),
            Self::Document { doc_id } => doc_id.trim().to_owned(),
            Self::ContextRecord { lineage_id } => {
                format!("context record `{}`", lineage_id.trim())
            }
        }
    }
}

/// How an issue is being closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "closure")]
pub enum Closure {
    /// It was done.
    Fixed {
        /// The pull request or commit that carried it.
        by: Citation,
    },

    /// It will not be done — won't fix, can't reproduce, stale.
    ///
    /// Carries **both** a reason a person can read and the authority that
    /// settles it. The reason alone is an opinion; the authority is what lets
    /// the next person who hits the same problem find out who decided, and
    /// what would have to change for the decision to be revisited.
    NotPlanned {
        /// Why, in a sentence.
        reason: String,
        /// The document or context record that settles it.
        per: Citation,
    },

    /// Another issue already covers it.
    Duplicate {
        /// The issue this duplicates. Required — a duplicate closure that does
        /// not say what it duplicates sends the reader looking for an original
        /// they cannot find, which is worse than leaving it open.
        of: String,
    },

    /// Part of it was done, and the rest is now tracked separately.
    Partial {
        /// What was actually finished.
        done: String,
        /// The pull request or commit that carried that part.
        by: Citation,
        /// The issues now carrying what was not. Must already exist.
        remaining: Vec<String>,
    },
}

/// Why a closure was refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "refusal")]
pub enum ClosureRefusal {
    /// A partial closure named no remaining issues.
    PartialWithNoRemainder,
    /// A required field was empty.
    MissingRationale {
        /// Which one.
        field: &'static str,
    },
    /// The citation was of the wrong kind for the claim.
    ///
    /// A fix evidenced by a document, or a refusal evidenced by a commit. Both
    /// are category errors: one claims a change happened and cites something
    /// that is not a change, the other claims a decision and cites something
    /// that is not a decision.
    WrongCitationKind {
        /// What the claim needed.
        wanted: &'static str,
    },
}

/// Whether this closure may proceed.
///
/// Pure and total. The tracker write happens only after this returns `Ok`, so
/// the discipline is not a habit a caller can forget.
///
/// # Errors
///
/// [`ClosureRefusal`] names what is missing, in terms the caller can act on.
pub fn check(closure: &Closure) -> Result<(), ClosureRefusal> {
    match closure {
        Closure::Fixed { by } => cite_change(by),

        Closure::NotPlanned { reason, per } => {
            if reason.trim().is_empty() {
                return Err(ClosureRefusal::MissingRationale { field: "reason" });
            }
            if per.is_blank() {
                return Err(ClosureRefusal::MissingRationale { field: "per" });
            }
            if !per.is_authority() {
                return Err(ClosureRefusal::WrongCitationKind {
                    wanted: "a document or a context record",
                });
            }
            Ok(())
        }

        Closure::Duplicate { of } if of.trim().is_empty() => {
            Err(ClosureRefusal::MissingRationale { field: "of" })
        }
        Closure::Duplicate { .. } => Ok(()),

        Closure::Partial {
            done,
            by,
            remaining,
        } => {
            if done.trim().is_empty() {
                return Err(ClosureRefusal::MissingRationale { field: "done" });
            }
            cite_change(by)?;
            if remaining.iter().all(|key| key.trim().is_empty()) {
                return Err(ClosureRefusal::PartialWithNoRemainder);
            }
            Ok(())
        }
    }
}

fn cite_change(by: &Citation) -> Result<(), ClosureRefusal> {
    if by.is_blank() {
        return Err(ClosureRefusal::MissingRationale { field: "by" });
    }
    if !by.is_change() {
        return Err(ClosureRefusal::WrongCitationKind {
            wanted: "a pull request or a commit",
        });
    }
    Ok(())
}

/// Stella's canonical resolution for this closure.
///
/// A canonical name, not a tracker's spelling — see the module docs.
#[must_use]
pub fn resolution_of(closure: &Closure) -> &'static str {
    match closure {
        Closure::Fixed { .. } | Closure::Partial { .. } => "completed",
        Closure::NotPlanned { .. } => "not_planned",
        Closure::Duplicate { .. } => "duplicate",
    }
}

/// The comment left on the issue as it closes.
///
/// Every kind leaves one, and every kind names its citation. A closure the
/// next reader has to reconstruct from a diff is one they will not
/// reconstruct.
#[must_use]
pub fn receipt(closure: &Closure) -> String {
    match closure {
        Closure::Fixed { by } => {
            format!("Closing as completed.\n\nFixed by {}.", by.render())
        }
        Closure::NotPlanned { reason, per } => format!(
            "Closing as not planned.\n\nReason: {reason}\n\nThis follows {}.",
            per.render()
        ),
        Closure::Duplicate { of } => format!(
            "Closing as a duplicate of #{}.\n\nEverything reported here is tracked there.",
            of.trim().trim_start_matches('#')
        ),
        Closure::Partial {
            done,
            by,
            remaining,
        } => {
            let list = remaining
                .iter()
                .filter(|k| !k.trim().is_empty())
                .map(|k| format!("- #{}", k.trim().trim_start_matches('#')))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "Closing as completed in part.\n\nDone: {done}\n\nCarried by {}.\n\nWhat is \
                 left is tracked separately, so nothing here is dropped:\n{list}",
                by.render()
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(key: &str) -> Citation {
        Citation::PullRequest { key: key.into() }
    }

    /// **The witness.** "Mostly fixed" is the honest description of most real
    /// work and the easiest thing to round up to done. It is refused outright
    /// unless the remainder already exists as its own issues.
    #[test]
    fn a_partial_closure_with_no_filed_remainder_is_refused() {
        for remaining in [Vec::new(), vec!["  ".to_owned(), String::new()]] {
            let sloppy = Closure::Partial {
                done: "fixed the parser".into(),
                by: pr("3985"),
                remaining,
            };
            assert_eq!(check(&sloppy), Err(ClosureRefusal::PartialWithNoRemainder));
        }
    }

    /// The other half — with the remainder filed, it proceeds, and the comment
    /// links every issue carrying what was left.
    #[test]
    fn a_partial_closure_that_filed_its_remainder_proceeds_and_links_it() {
        let honest = Closure::Partial {
            done: "fixed the parser".into(),
            by: pr("3985"),
            remaining: vec!["4001".into(), "#4002".into()],
        };
        assert_eq!(check(&honest), Ok(()));

        let comment = receipt(&honest);
        assert!(comment.contains("- #4001"), "{comment}");
        assert!(comment.contains("- #4002"), "{comment}");
        assert!(comment.contains("nothing here is dropped"), "{comment}");
        assert!(comment.contains("#3985"), "must cite what carried it");
    }

    /// **The evidence witness.** A fix cites the pull request that carried it,
    /// or the commit — and nothing else will do. A document is a category
    /// error: it is not a change.
    #[test]
    fn a_fix_must_cite_a_change_not_an_authority() {
        assert_eq!(check(&Closure::Fixed { by: pr("3985") }), Ok(()));
        assert_eq!(
            check(&Closure::Fixed {
                by: Citation::Commit {
                    sha: "f8935f2".into()
                }
            }),
            Ok(()),
            "the all-hands path lands directly and cites the commit"
        );
        assert_eq!(
            check(&Closure::Fixed {
                by: Citation::Document {
                    doc_id: "doc:backlog-self-driving".into()
                }
            }),
            Err(ClosureRefusal::WrongCitationKind {
                wanted: "a pull request or a commit"
            })
        );
    }

    /// **The authority witness.** A refusal is not an opinion: it names why and
    /// cites what settles it, so the next person to hit the same problem can
    /// find who decided and what would reopen it.
    #[test]
    fn a_refusal_must_name_its_reason_and_cite_its_authority() {
        let settled = Closure::NotPlanned {
            reason: "superseded by the config collapse".into(),
            per: Citation::ContextRecord {
                lineage_id: "ctx.macanderson.stella.reference-grade-bar".into(),
            },
        };
        assert_eq!(check(&settled), Ok(()));
        assert!(receipt(&settled).contains("context record"), "cites it");

        // A reason with no authority behind it is refused.
        assert_eq!(
            check(&Closure::NotPlanned {
                reason: "stale".into(),
                per: Citation::Document {
                    doc_id: "   ".into()
                },
            }),
            Err(ClosureRefusal::MissingRationale { field: "per" })
        );

        // And a change is the wrong currency for a decision.
        assert_eq!(
            check(&Closure::NotPlanned {
                reason: "stale".into(),
                per: pr("3985"),
            }),
            Err(ClosureRefusal::WrongCitationKind {
                wanted: "a document or a context record"
            })
        );
    }

    /// A duplicate closure cites what it duplicates, and the `#` is not
    /// doubled however the caller wrote the key.
    #[test]
    fn a_duplicate_closure_cites_the_issue_it_duplicates() {
        let dup = Closure::Duplicate { of: "#3599".into() };
        assert_eq!(check(&dup), Ok(()));

        let comment = receipt(&dup);
        assert!(comment.contains("#3599"), "{comment}");
        assert!(
            !comment.contains("##"),
            "the `#` must not double: {comment}"
        );

        // However the caller spelled the key, the comment reads the same.
        assert_eq!(receipt(&Closure::Duplicate { of: "3599".into() }), comment);
    }

    /// An uncited duplicate is refused.
    #[test]
    fn a_duplicate_closure_with_no_citation_is_refused() {
        assert_eq!(
            check(&Closure::Duplicate { of: "  ".into() }),
            Err(ClosureRefusal::MissingRationale { field: "of" })
        );
    }

    /// The three kinds map to GitHub's three close reasons.
    #[test]
    fn the_three_kinds_map_to_the_three_github_close_reasons() {
        assert_eq!(resolution_of(&Closure::Fixed { by: pr("1") }), "completed");
        assert_eq!(
            resolution_of(&Closure::NotPlanned {
                reason: "x".into(),
                per: Citation::Document {
                    doc_id: "doc:x".into()
                },
            }),
            "not_planned"
        );
        assert_eq!(
            resolution_of(&Closure::Duplicate { of: "1".into() }),
            "duplicate"
        );
        // A partial closure is completed: the scoped work finished, and saying
        // otherwise would tell a later reader it was declined.
        assert_eq!(
            resolution_of(&Closure::Partial {
                done: "some".into(),
                by: pr("1"),
                remaining: vec!["2".into()],
            }),
            "completed"
        );
    }

    /// Every kind leaves a comment naming its citation.
    #[test]
    fn every_kind_leaves_a_receipt_that_cites_something() {
        for closure in [
            Closure::Fixed { by: pr("3985") },
            Closure::NotPlanned {
                reason: "stale".into(),
                per: Citation::Document {
                    doc_id: "doc:backlog-self-driving".into(),
                },
            },
            Closure::Duplicate { of: "9".into() },
            Closure::Partial {
                done: "half".into(),
                by: Citation::Commit {
                    sha: "f8935f2".into(),
                },
                remaining: vec!["2".into()],
            },
        ] {
            let r = receipt(&closure);
            assert!(!r.trim().is_empty(), "{closure:?}");
            assert!(r.len() > 20, "a receipt must say something: {r:?}");
        }
    }

    #[test]
    fn a_closure_round_trips_through_json() {
        let c = Closure::Partial {
            done: "half".into(),
            by: pr("3985"),
            remaining: vec!["2".into()],
        };
        let json = serde_json::to_string(&c).expect("serialize");
        let back: Closure = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(c, back);
    }
}
