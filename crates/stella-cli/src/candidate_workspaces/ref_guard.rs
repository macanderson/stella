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
//! # Two refs the reachability probe cannot judge (#4478)
//!
//! **`refs/stash` moves the wrong way round.** A stash commit is built *on*
//! the HEAD it was taken from, so a candidate's stash is a descendant of the
//! candidate's tip and the probe above reads it as somebody else's work. It is
//! attributed by what git itself wrote into the commit instead: `git stash`
//! records `WIP on <branch>: …` (or `On <branch>: …` for `git stash -m`), and
//! the branch a linked worktree is on is the candidate's own. That holds even
//! for a candidate that has committed nothing, where every reachability
//! question is a tie because the candidate's tip and the user's branch are the
//! same commit.
//!
//! **A deletion carries no value to attribute.** It is judged by the
//! candidate's *own* HEAD reflog — per-worktree state that only the candidate
//! could have written — which names a ref exactly when the candidate checked
//! it out or reset onto it. That is the observed escape
//! (`git checkout --ignore-other-worktrees main`, then delete or move), and it
//! is silent for a user deleting one of their own branches mid-fan-out, which
//! is the false refusal this replaces. A candidate that deletes a branch it
//! never checked out is a miss — the direction this module's contract requires
//! when it cannot tell.
//!
//! # What this deliberately does not do
//!
//! It does not **repair**. The pre-#3865 substrate restored an escaped ref
//! under compare-and-swap; adoption here refuses instead, and the candidate's
//! checkout is still on disk (`CandidateFanouts::adopt` guarantees it), so the
//! reflog entry the candidate left is reachable and the user decides. Nor does
//! it attribute a *branch* the candidate created outright, or a rewind onto a
//! commit that is genuinely the candidate's — for those the real tree's own
//! reflog is the record. `refs/stash` is the one ref whose creation counts,
//! because the stack a user pops from is a single shared object rather than a
//! name they chose.
//!
//! Every probe is best-effort in the direction of **not** refusing: a git
//! invocation that fails at all reports "no escape", so this check can only
//! ever add a detection, never invent one.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use stella_fleet::git::Worktree;

/// Refs *named* in one refusal. A candidate that moved more than this has made
/// the point; the tail is counted, never silently dropped.
const MAX_NAMED: usize = 8;

/// The one shared ref whose new value is a **descendant** of the candidate's
/// tip when the candidate is the one that moved it. See the module header.
const STASH_REF: &str = "refs/stash";

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
    /// The value recorded when this candidate was created — `None` when the
    /// ref did not exist then, which only [`STASH_REF`] is judged on.
    pub(super) before: Option<String>,
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

    /// What `now` holds for `name` when this snapshot held nothing.
    ///
    /// [`Self::changes_against`] deliberately ignores every ref created since
    /// the snapshot, because the real tree never depended on it. [`STASH_REF`]
    /// is the exception the caller asks about by name: the stash stack is a
    /// single shared object a user pops from, so a candidate pushing the first
    /// entry onto an empty one hands the user its half-finished work under
    /// `git stash pop`.
    pub(super) fn created<'a>(&self, now: &'a Self, name: &str) -> Option<&'a String> {
        if self.refs.contains_key(name) {
            return None;
        }
        now.refs.get(name)
    }
}

/// The short name a reflog message spells a ref by — `refs/heads/main` is
/// written `main` there, and a deletion is reported by its full name.
fn short_ref(name: &str) -> &str {
    for namespace in ["refs/heads/", "refs/tags/", "refs/remotes/"] {
        if let Some(rest) = name.strip_prefix(namespace) {
            return rest;
        }
    }
    name
}

/// Does `subject` — the subject line of the commit `refs/stash` names — say
/// the stash was taken on `branch`?
///
/// `git stash` writes `WIP on <branch>: <oid> <subject>` and `git stash -m`
/// writes `On <branch>: <message>`; both name the branch the worktree that
/// stashed was sitting on. A detached HEAD gives `(no branch)`, which no
/// candidate branch is spelled as, so it attributes nothing.
pub(super) fn stash_taken_on(subject: &str, branch: &str) -> bool {
    let tail = subject
        .strip_prefix("WIP on ")
        .or_else(|| subject.strip_prefix("On "))
        .unwrap_or_default();
    tail.strip_prefix(branch)
        .is_some_and(|rest| rest.starts_with(':'))
}

/// Every ref a worktree's own HEAD reflog says that worktree moved onto.
///
/// Parsed structurally rather than by searching the whole message for a name:
/// a reflog carries commit subjects too, and a candidate whose commit message
/// mentioned a branch would otherwise be answerable for that branch's fate.
/// Only the two messages that record a HEAD move contribute —
/// `checkout: moving from A to B` and `reset: moving to B`.
pub(super) fn refs_head_moved_onto(reflog: &str) -> BTreeSet<String> {
    let mut refs = BTreeSet::new();
    for message in reflog.lines().map(str::trim) {
        if let Some(move_) = message.strip_prefix("checkout: moving from ") {
            if let Some((from, to)) = move_.split_once(" to ") {
                refs.insert(from.to_string());
                refs.insert(to.to_string());
            }
        } else if let Some(to) = message.strip_prefix("reset: moving to ") {
            refs.insert(to.to_string());
        }
    }
    refs
}

/// Audit the shared namespace of the repository at `top` against what was
/// recorded when `candidate` was created.
///
/// The candidate's branch is the commit its work is reachable from — the
/// second half of the signature in this module's header — and its checkout is
/// where the per-worktree HEAD reflog that attributes a deletion lives.
pub(super) async fn audit(
    subject: &super::SessionCandidateWorkspaces,
    top: &Path,
    at_create: &RefSnapshot,
    candidate: &Worktree,
) -> Vec<RefEscape> {
    let Ok(now) = subject.for_each_ref(top).await else {
        // Best-effort: forensics this host could not read report no escape
        // rather than failing a candidate on the reading.
        return Vec::new();
    };
    let changes = at_create.changes_against(&now);
    // Read once, and only when a deletion is on the table: the overwhelming
    // majority of adoptions have nothing to attribute and should not pay for a
    // reflog nobody reads.
    let moved_onto = if changes
        .iter()
        .any(|change| matches!(change, RefChange::Deleted { .. }))
    {
        refs_head_moved_onto(&subject.head_reflog(&candidate.path).await)
    } else {
        BTreeSet::new()
    };
    let mut escaped = Vec::new();
    // The stash stack the candidate started, on a repository that had none.
    if let Some(after) = at_create.created(&now, STASH_REF)
        && stash_taken_on(&subject.commit_subject(top, after).await, &candidate.branch)
    {
        escaped.push(RefEscape {
            name: STASH_REF.to_string(),
            before: None,
            after: Some(after.clone()),
        });
    }
    for change in changes {
        match change {
            RefChange::Deleted { name, before } => {
                if !moved_onto.contains(short_ref(&name)) && !moved_onto.contains(&name) {
                    // Nothing in this candidate's own worktree state says it
                    // ever sat on that ref, so the deletion is the user's.
                    continue;
                }
                escaped.push(RefEscape {
                    name,
                    before: Some(before),
                    after: None,
                });
            }
            RefChange::Moved {
                name,
                before,
                after,
            } if name == STASH_REF => {
                if !stash_taken_on(
                    &subject.commit_subject(top, &after).await,
                    &candidate.branch,
                ) {
                    // Someone else's stash — the user's own, taken in their own
                    // checkout, which names their branch and not this one.
                    continue;
                }
                escaped.push(RefEscape {
                    name,
                    before: Some(before),
                    after: Some(after),
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
                if !subject.is_ancestor(top, &after, &candidate.branch).await {
                    // Not reachable from this candidate's work, so it is not
                    // this candidate's — an ordinary mid-run user commit.
                    continue;
                }
                escaped.push(RefEscape {
                    name,
                    before: Some(before),
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
        .map(|escape| match (&escape.before, &escape.after) {
            (Some(before), Some(after)) => {
                format!("{} moved {}..{}", escape.name, short(before), short(after))
            }
            (Some(before), None) => {
                format!("{} was deleted (was {})", escape.name, short(before))
            }
            (None, after) => format!(
                "{} was created ({})",
                escape.name,
                after.as_deref().map_or("", short)
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
                before: Some("0123456789abcdef".into()),
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
            before: Some("0123456789abcdef".into()),
            after: None,
        }]);
        assert!(reason.contains("refs/heads/main was deleted (was 01234567)"));
    }

    #[test]
    fn a_created_stash_is_reported_as_a_creation() {
        let reason = refusal(&[RefEscape {
            name: STASH_REF.into(),
            before: None,
            after: Some("0123456789abcdef".into()),
        }]);
        assert!(
            reason.contains("refs/stash was created (01234567)"),
            "{reason}"
        );
    }

    /// **#4478's stash half.** Both spellings git writes, and the two rejects
    /// that keep this from becoming an accusation: another worktree's branch,
    /// and a branch whose name merely starts with the candidate's.
    #[test]
    fn a_stash_is_attributed_by_the_branch_git_recorded_in_it() {
        assert!(stash_taken_on(
            "WIP on candidate/p-0-abcd: e4468e0 seed",
            "candidate/p-0-abcd"
        ));
        assert!(stash_taken_on(
            "On candidate/p-0-abcd: a note",
            "candidate/p-0-abcd"
        ));
        assert!(
            !stash_taken_on("WIP on main: e4468e0 seed", "candidate/p-0-abcd"),
            "the user's own stash names the user's own branch"
        );
        assert!(
            !stash_taken_on("WIP on (no branch): e4468e0 seed", "candidate/p-0-abcd"),
            "a detached HEAD names no branch, so it attributes nothing"
        );
        assert!(
            !stash_taken_on(
                "WIP on candidate/p-0-abcde: e4468e0 x",
                "candidate/p-0-abcd"
            ),
            "a longer branch name that merely starts the same is not this one"
        );
        assert!(
            !stash_taken_on("candidate/p-0-abcd: something", "candidate/p-0-abcd"),
            "a subject that is not a stash subject attributes nothing"
        );
    }

    /// **#4478's deletion half.** Only the two messages that record a HEAD
    /// move contribute, so a candidate whose *commit message* named a branch
    /// is not answerable for that branch's fate.
    #[test]
    fn the_head_reflog_names_only_the_refs_the_worktree_moved_onto() {
        let moved = refs_head_moved_onto(
            "commit: fix refs/heads/main at last\n\
             checkout: moving from candidate/p-0-abcd to main\n\
             reset: moving to HEAD\n\
             branch: Created from HEAD\n",
        );
        assert!(moved.contains("main"), "{moved:?}");
        assert!(moved.contains("candidate/p-0-abcd"), "{moved:?}");
        assert!(moved.contains("HEAD"), "{moved:?}");
        assert_eq!(
            moved.len(),
            3,
            "a commit subject is not a HEAD move: {moved:?}"
        );
    }

    #[test]
    fn a_reflog_spells_a_ref_the_way_a_deletion_is_reported() {
        assert_eq!(short_ref("refs/heads/main"), "main");
        assert_eq!(short_ref("refs/tags/v1"), "v1");
        assert_eq!(short_ref("refs/remotes/origin/main"), "origin/main");
        assert_eq!(short_ref("refs/stash"), "refs/stash");
    }

    /// The stash stack is the one ref whose *creation* is a change to state
    /// the real tree depends on; a branch's is not.
    #[test]
    fn only_the_named_ref_reports_its_creation() {
        let before = snapshot(&[("aaaa", "refs/heads/main")]);
        let now = snapshot(&[
            ("aaaa", "refs/heads/main"),
            ("cccc", "refs/heads/feature"),
            ("dddd", STASH_REF),
        ]);
        assert_eq!(before.created(&now, STASH_REF), Some(&"dddd".to_string()));
        assert_eq!(before.created(&now, "refs/heads/main"), None);
        assert_eq!(
            now.created(&now, STASH_REF),
            None,
            "a ref the snapshot already held was not created since"
        );
    }
}
