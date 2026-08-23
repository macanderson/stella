// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Did this candidate write into the ref namespace the **real tree** depends
//! on? (#4390, successor to #2812.)
//!
//! Candidate isolation isolates the working *tree*, not the ref namespace:
//! `git worktree add` gives a candidate its own checkout and the *same*
//! `.git`, so `refs/heads/*`, `refs/tags/*` and `refs/stash` are one shared
//! object the user's own checkout reads. Git's interlock refuses to check out
//! a branch another worktree holds, which is why the observed failure had to
//! defeat it explicitly (`git checkout --ignore-other-worktrees main`) — after
//! which every subsequent commit moved the user's `main` out from under their
//! index.
//!
//! # The signature
//!
//! A shared ref that simply *moved* proves nothing: a user committing in their
//! own checkout while a fan-out is in flight moves refs too, and refusing a
//! good candidate for that would be a false accusation. So a move counts only
//! when both halves hold:
//!
//! - the new value is **not** already reachable from the value recorded when
//!   this candidate was created — which is what excludes a mid-run
//!   `git reset --hard HEAD~1` in the user's own checkout, a rewind into
//!   history the real tree already had; and
//! - the new value **is** reachable from this candidate's own branch tip —
//!   the candidate committed onto a shared ref, which is the observed shape.
//!
//! A user's ordinary mid-run commit fails the second probe: their new commit
//! is a *descendant* of the candidate's tip, never an ancestor of it.
//!
//! A **deletion** carries no value to attribute, so it is judged on the change
//! alone: a ref that existed when the candidate was created and is gone when
//! it asks to be adopted refuses the adoption. A user deleting a branch
//! mid-fan-out therefore costs one candidate its adoption — a refusal that
//! names the ref, against a shared namespace that would otherwise be
//! corrupted silently.
//!
//! # What this deliberately does not do
//!
//! It does not **repair**. The pre-#3865 substrate restored an escaped ref
//! under compare-and-swap; adoption here refuses instead, and the candidate's
//! checkout is still on disk (`CandidateFanouts::adopt` guarantees it), so the
//! reflog entry the candidate left is reachable and the user decides. Nor does
//! it attribute a ref the candidate created outright, or a rewind onto a
//! commit that is genuinely the candidate's — for those the real tree's own
//! reflog is the record.
//!
//! Every probe is best-effort in the direction of **not** refusing: a git
//! invocation that fails at all reports "no escape", so this check can only
//! ever add a detection, never invent one.

use std::collections::BTreeMap;
use std::path::Path;

/// Refs *named* in one refusal. A candidate that moved more than this has made
/// the point; the tail is counted, never silently dropped.
const MAX_NAMED: usize = 8;

/// Every ref of a repository and its object id, keyed by full ref name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RefSnapshot {
    refs: BTreeMap<String, String>,
}

/// One shared ref this candidate is answerable for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RefEscape {
    /// Full ref name, e.g. `refs/heads/main`.
    pub(super) name: String,
    /// The value recorded when this candidate was created.
    pub(super) before: String,
    /// What the ref points at now — `None` when it was deleted.
    pub(super) after: Option<String>,
}

/// A ref whose value differs from the recorded one, before attribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RefChange {
    /// Present in both snapshots, pointing somewhere else now.
    Moved {
        name: String,
        before: String,
        after: String,
    },
    /// Recorded at creation and gone now.
    Deleted { name: String, before: String },
}

impl RefSnapshot {
    /// Parse `git for-each-ref --format=%(objectname) %(refname)`.
    ///
    /// Refs under [`super::CANDIDATE_BRANCH_PREFIX`] are dropped here rather
    /// than filtered at each comparison: the whole `candidate/` namespace
    /// belongs to this substrate, so a candidate's own branch — and every
    /// sibling's, minted concurrently by the same fan-out — is not shared
    /// state anyone else reads.
    pub(super) fn parse(for_each_ref: &str) -> Self {
        let mut refs = BTreeMap::new();
        for row in for_each_ref.lines() {
            let Some((oid, name)) = row.trim_end().split_once(' ') else {
                continue;
            };
            if name.starts_with(&format!("refs/heads/{}", super::CANDIDATE_BRANCH_PREFIX)) {
                continue;
            }
            refs.insert(name.to_string(), oid.to_string());
        }
        Self { refs }
    }

    /// Every recorded ref whose value `now` disagrees with.
    ///
    /// A ref `now` has and this snapshot does not is **not** reported: the
    /// real tree never depended on it, so a user branching mid-fan-out is not
    /// a candidate's fault.
    pub(super) fn changes_against(&self, now: &Self) -> Vec<RefChange> {
        self.refs
            .iter()
            .filter_map(|(name, before)| match now.refs.get(name) {
                Some(after) if after == before => None,
                Some(after) => Some(RefChange::Moved {
                    name: name.clone(),
                    before: before.clone(),
                    after: after.clone(),
                }),
                None => Some(RefChange::Deleted {
                    name: name.clone(),
                    before: before.clone(),
                }),
            })
            .collect()
    }
}

/// Audit the shared namespace of the repository at `top` against what was
/// recorded when this candidate was created.
///
/// `tip` is the candidate's own branch — the commit its work is reachable
/// from, and the second half of the signature in this module's header.
pub(super) async fn audit(
    subject: &super::SessionCandidateWorkspaces,
    top: &Path,
    at_create: &RefSnapshot,
    tip: &str,
) -> Vec<RefEscape> {
    let Ok(now) = subject.for_each_ref(top).await else {
        // Best-effort: forensics this host could not read report no escape
        // rather than failing a candidate on the reading.
        return Vec::new();
    };
    let mut escaped = Vec::new();
    for change in at_create.changes_against(&now) {
        match change {
            RefChange::Deleted { name, before } => {
                escaped.push(RefEscape {
                    name,
                    before,
                    after: None,
                });
            }
            RefChange::Moved {
                name,
                before,
                after,
            } => {
                if subject.is_ancestor(top, &after, &before).await {
                    // A rewind into history the real tree already had: the
                    // user's own `reset --hard`, not an escape.
                    continue;
                }
                if !subject.is_ancestor(top, &after, tip).await {
                    // Not reachable from this candidate's work, so it is not
                    // this candidate's — an ordinary mid-run user commit.
                    continue;
                }
                escaped.push(RefEscape {
                    name,
                    before,
                    after: Some(after),
                });
            }
        }
    }
    escaped
}

/// What adoption tells the plugin when a candidate escaped into shared refs.
pub(super) fn refusal(escaped: &[RefEscape]) -> String {
    let mut named: Vec<String> = escaped
        .iter()
        .take(MAX_NAMED)
        .map(|escape| match &escape.after {
            Some(after) => format!(
                "{} moved {}..{}",
                escape.name,
                short(&escape.before),
                short(after)
            ),
            None => format!(
                "{} was deleted (was {})",
                escape.name,
                short(&escape.before)
            ),
        })
        .collect();
    if escaped.len() > MAX_NAMED {
        named.push(format!("and {} more", escaped.len() - MAX_NAMED));
    }
    format!(
        "this candidate changed the repository's shared refs, which its \
         worktree does not isolate: {}. The adoption was refused and the \
         candidate's checkout is still on disk, so `git reflog` names what it \
         wrote",
        named.join("; ")
    )
}

/// Object ids are compared in full and reported abbreviated — a refusal is
/// read by a human, and forty characters of hex is not.
fn short(oid: &str) -> &str {
    &oid[..oid.len().min(8)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(rows: &[(&str, &str)]) -> RefSnapshot {
        RefSnapshot::parse(
            &rows
                .iter()
                .map(|(oid, name)| format!("{oid} {name}\n"))
                .collect::<String>(),
        )
    }

    #[test]
    fn the_substrates_own_namespace_is_not_shared_state() {
        let snap = snapshot(&[
            ("aaaa", "refs/heads/main"),
            ("bbbb", "refs/heads/candidate/plugin-0-abcd"),
        ]);
        assert_eq!(
            snap.refs.keys().collect::<Vec<_>>(),
            vec!["refs/heads/main"],
            "a candidate branch is this substrate's, not the user's"
        );
    }

    #[test]
    fn a_ref_the_snapshot_never_saw_is_not_a_change() {
        let before = snapshot(&[("aaaa", "refs/heads/main")]);
        let now = snapshot(&[("aaaa", "refs/heads/main"), ("cccc", "refs/heads/feature")]);
        assert!(
            before.changes_against(&now).is_empty(),
            "a branch created mid-fan-out is nothing the real tree depended on"
        );
    }

    #[test]
    fn a_moved_and_a_deleted_ref_are_both_changes() {
        let before = snapshot(&[
            ("aaaa", "refs/heads/main"),
            ("bbbb", "refs/tags/v1"),
            ("cccc", "refs/heads/kept"),
        ]);
        let now = snapshot(&[("dddd", "refs/heads/main"), ("cccc", "refs/heads/kept")]);
        assert_eq!(
            before.changes_against(&now),
            vec![
                RefChange::Moved {
                    name: "refs/heads/main".into(),
                    before: "aaaa".into(),
                    after: "dddd".into(),
                },
                RefChange::Deleted {
                    name: "refs/tags/v1".into(),
                    before: "bbbb".into(),
                },
            ]
        );
    }

    #[test]
    fn the_refusal_names_every_ref_up_to_the_ceiling_and_counts_the_rest() {
        let escaped: Vec<RefEscape> = (0..MAX_NAMED + 3)
            .map(|n| RefEscape {
                name: format!("refs/heads/b{n}"),
                before: "0123456789abcdef".into(),
                after: Some("fedcba9876543210".into()),
            })
            .collect();
        let reason = refusal(&escaped);
        assert!(reason.contains("refs/heads/b0 moved 01234567..fedcba98"));
        assert!(reason.contains("and 3 more"));
        assert!(!reason.contains("refs/heads/b8"));
    }

    #[test]
    fn a_deleted_ref_is_reported_as_a_deletion() {
        let reason = refusal(&[RefEscape {
            name: "refs/heads/main".into(),
            before: "0123456789abcdef".into(),
            after: None,
        }]);
        assert!(reason.contains("refs/heads/main was deleted (was 01234567)"));
    }
}
