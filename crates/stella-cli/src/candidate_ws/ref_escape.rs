//! Shared-ref escape detection (#2541) — did this candidate write into the
//! ref namespace the GRADED tree depends on? A child module of
//! `candidate_ws`, like [`super::escape`], whose file-level sibling it is:
//! same seam (immediately around the seal), same best-effort contract, same
//! insistence on a signature a mid-run *user* action cannot forge.
//!
//! [`super::escape`] answers the question for file bytes. This one answers it
//! for refs, because candidate isolation isolates the working **tree** and not
//! the **ref namespace**: `git worktree add` gives the candidate its own
//! checkout but the *same* `.git`, so `refs/heads/*`, `refs/tags/*` and
//! `refs/stash` are one shared object the graded tree also reads. Git's own
//! interlock refuses to check out a branch another worktree holds, which is
//! why the observed failure had to defeat it explicitly
//! (`git checkout --ignore-other-worktrees master`) — after which every
//! subsequent commit moved the graded tree's `master` out from under its
//! index, and the graded tree's `git status` read as a staged reversal of the
//! entire task.
//!
//! # The signature
//!
//! A shared ref that simply *moved* proves nothing: a user committing in
//! their own checkout while a run is in flight moves refs too, and blaming a
//! candidate for that would be the false positive [`super::escape`] goes to
//! the same lengths to avoid. A move is this candidate's iff **the ref's new
//! value is work only this candidate has**, which is exactly two shapes:
//!
//! * the new value is an ancestor-or-equal of the candidate's live `HEAD` —
//!   the candidate committed *onto* the shared branch (the observed shape:
//!   after `--ignore-other-worktrees`, candidate `HEAD` and `refs/heads/master`
//!   are the same commit); or
//! * the candidate's private baseline commit is an ancestor of the new value —
//!   the candidate's snapshot history was *merged or grafted into* the shared
//!   ref, which is also how a banned `git stash` shows up (`refs/stash`'s
//!   first parent is the candidate's `HEAD`).
//!
//! Both are qualified by a third probe that removes the one collision either
//! would otherwise admit: a ref moved **backwards into history the graded
//! tree already had** (`git reset --hard HEAD~1` in the user's own checkout)
//! lands on a commit that is trivially an ancestor of the candidate's `HEAD`,
//! and is a user action, not an escape. So a divergence counts only when the
//! new value was *not* already reachable from the value recorded at creation.
//!
//! # Boundaries, accepted and documented
//!
//! Best-effort, like its sibling: every failed probe reports "no escape"
//! rather than failing a candidate on broken forensics, so this check can
//! only ever *add* a detection. A **deleted** ref carries no new value to
//! sign and is deliberately out of scope, as is a rewind to an unrelated
//! commit — for those the graded tree's own history is the record. And this
//! is detection, not repair: nothing here writes to the real repository, so
//! the refusal quotes the value recorded at creation and the one command that
//! puts it back. The structural fix — a candidate substrate that cannot reach
//! the graded tree's refs at all — is #1383, and the residual repair gap is
//! #2641.
//!
//! One coupling to know before changing either: the attribution probes run
//! against the REAL repository yet name the candidate's private `baseline` and
//! `HEAD`, which is only resolvable because the two share an object store —
//! the very property this module exists to police. A substrate that separates
//! the object stores (#1383) makes this module unnecessary *and* breaks its
//! probes in the same stroke; it must be retired with that change, not ported
//! across it.

use std::collections::BTreeMap;
use std::path::Path;

use super::{GitCandidateWorkspace, git};

/// Ceiling on refs named in one refusal. A candidate that moved more refs
/// than this has already made the point; the message stays readable and the
/// probe count stays bounded.
const MAX_NAMED: usize = 8;

/// One shared ref whose new value is provably this candidate's work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RefEscape {
    /// Full ref name, e.g. `refs/heads/master`.
    pub(super) name: String,
    /// The value recorded when this candidate was created — `None` when the
    /// candidate created the ref outright.
    pub(super) before: Option<String>,
    /// What the ref points at now.
    pub(super) after: String,
}

/// Every ref of `toplevel` and its object id, keyed by full ref name.
///
/// Read from the real repository, so it sees the shared namespace plus the
/// *main* worktree's own per-worktree refs — never a candidate's
/// `refs/worktree/*`, which is what keeps the pipeline's own
/// [`stella_tools::verify::WITNESS_BASELINE_WORKTREE_REF`] pin out of this
/// comparison by construction.
///
/// Best-effort: an unreadable ref store yields an empty map, which can only
/// make the later comparison quieter.
pub(super) async fn shared_refs(toplevel: &Path) -> BTreeMap<String, String> {
    let Ok(listing) = git(
        toplevel,
        &["for-each-ref", "--format=%(objectname) %(refname)"],
    )
    .await
    else {
        return BTreeMap::new();
    };
    listing
        .lines()
        .filter_map(|line| {
            // Ref names cannot contain a space, so the first one splits the
            // record exactly.
            let (oid, name) = line.trim_end().split_once(' ')?;
            Some((name.to_string(), oid.to_string()))
        })
        .collect()
}

/// `git merge-base --is-ancestor <ancestor> <descendant>` in `repo`.
///
/// Exit 0 is "yes"; git spells "no" as exit 1 and a broken argument as exit
/// 128, and [`git`] flattens both into `Err` — which is the conservative
/// answer for every caller here, since a failed probe must never manufacture
/// an escape.
async fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> bool {
    git(repo, &["merge-base", "--is-ancestor", ancestor, descendant])
        .await
        .is_ok()
}

/// Detach `HEAD` in `dir` if the worker attached it to a branch.
///
/// The seal is machinery: it must land in the candidate's private history and
/// nowhere else. When a worker has pointed the candidate's `HEAD` at a shared
/// branch, an ordinary `git commit` moves that branch instead — which is how
/// witness scaffolding reached a branch the user keeps (#2541). Rewriting
/// `HEAD` itself (`--no-deref`) to the commit it already resolves to detaches
/// it without touching the index or a single file in the worktree, so the
/// candidate's work is exactly as the worker left it.
///
/// A `HEAD` that is symbolic but unresolvable (a worker's `git checkout
/// --orphan`) is an error, not a silent pass: committing from there would
/// mint a new shared branch, which is the very thing this prevents.
pub(super) async fn detach_head(dir: &Path) -> Result<(), String> {
    // `symbolic-ref -q HEAD` exits non-zero exactly when HEAD is detached.
    if git(dir, &["symbolic-ref", "-q", "HEAD"]).await.is_err() {
        return Ok(());
    }
    let head = git(dir, &["rev-parse", "HEAD"])
        .await
        .map_err(|e| format!("candidate HEAD is attached to a branch with no commit on it: {e}"))?;
    git(dir, &["update-ref", "--no-deref", "HEAD", head.trim()])
        .await
        .map(|_| ())
}

impl GitCandidateWorkspace {
    /// Shared refs of the real repository whose current value is provably
    /// this candidate's work — see the module docs for the signature.
    ///
    /// Cost on a well-behaved candidate: exactly one `git` invocation (the
    /// ref enumeration against the real repo), because nothing has moved and
    /// no attribution probe is ever paid.
    pub(super) async fn escaped_refs_inner(&self) -> Vec<RefEscape> {
        let now = shared_refs(&self.toplevel).await;
        let moved: Vec<(&String, &String)> = now
            .iter()
            .filter(|(name, after)| self.refs_at_create.get(*name) != Some(*after))
            .take(MAX_NAMED)
            .collect();
        if moved.is_empty() {
            return Vec::new();
        }
        // The candidate's live HEAD, read once: the first signature compares
        // every moved ref against it.
        let Ok(head) = git(&self.dir, &["rev-parse", "HEAD"]).await else {
            return Vec::new();
        };
        let head = head.trim().to_string();

        let mut escaped = Vec::new();
        for (name, after) in moved {
            let before = self.refs_at_create.get(name);
            // A ref rewound into history the graded tree already had is the
            // user's own doing — screen it out before either signature can
            // read it as candidate work.
            if let Some(before) = before
                && is_ancestor(&self.toplevel, after, before).await
            {
                continue;
            }
            let committed_onto = is_ancestor(&self.toplevel, after, &head).await;
            let grafted_in =
                !committed_onto && is_ancestor(&self.toplevel, &self.baseline, after).await;
            if committed_onto || grafted_in {
                escaped.push(RefEscape {
                    name: name.clone(),
                    before: before.cloned(),
                    after: after.clone(),
                });
            }
        }
        escaped
    }
}

/// The seal refusal for a candidate that moved refs the graded tree shares.
///
/// Says what moved, what it was, and the one command that puts it back —
/// this module never writes to the real repository, so recovery has to be
/// spelled out rather than performed.
pub(super) fn refusal(escaped: &[RefEscape]) -> String {
    let mut named: Vec<String> = Vec::with_capacity(escaped.len());
    let mut recovery: Vec<String> = Vec::new();
    for esc in escaped {
        match &esc.before {
            Some(before) => {
                named.push(format!("{} (was {}, now {})", esc.name, before, esc.after));
                recovery.push(format!("git update-ref {} {}", esc.name, before));
            }
            None => {
                named.push(format!("{} (created, now {})", esc.name, esc.after));
                recovery.push(format!("git update-ref -d {}", esc.name));
            }
        }
    }
    format!(
        "this candidate moved refs the graded tree shares: {}. A candidate worktree isolates the \
         working tree but not the ref namespace — it shares `.git` with the real repository, so a \
         ref write escapes isolation even though every file write is contained. Nothing was \
         restored; `{}` puts the real repository back",
        named.join("; "),
        recovery.join(" && ")
    )
}

#[cfg(test)]
mod tests {
    use stella_pipeline::ports::CandidateWorkspace;
    use stella_tools::RegistryOptions;

    use super::super::tests::{scaffold, scratch_git};
    use super::super::{GitCandidateWorkspaces, git};
    use super::{RefEscape, refusal};

    fn port(root: std::path::PathBuf) -> GitCandidateWorkspaces {
        GitCandidateWorkspaces::new(
            root,
            RegistryOptions::default(),
            Default::default(),
            Vec::new(),
            crate::rules::ResolvedRules::default(),
        )
    }

    /// The branch the scaffold's `git init` produced — `master` or `main`
    /// depending on the host's `init.defaultBranch`.
    fn default_branch(root: &std::path::Path) -> String {
        scratch_git(root, &["rev-parse", "--abbrev-ref", "HEAD"])
            .trim()
            .to_string()
    }

    /// The observed Terminal-Bench `fix-git` shape (#2541): the worker
    /// defeats git's worktree interlock, commits onto the branch the graded
    /// tree has checked out, and the graded tree's ref moves out from under
    /// its index. The seal must refuse and name the ref.
    #[tokio::test]
    async fn committing_onto_the_graded_trees_branch_refuses_the_seal() {
        let root = scaffold("refesc-hijack");
        let branch = default_branch(&root);
        let ws = port(root.clone()).create_workspace().await.unwrap();

        // Exactly the observed escape: git refuses a plain checkout of a
        // branch another worktree holds, so the worker overrides it.
        scratch_git(
            ws.dir(),
            &["checkout", "--ignore-other-worktrees", "-q", &branch],
        );
        std::fs::write(ws.dir().join("hijack.txt"), "worker\n").unwrap();
        scratch_git(ws.dir(), &["add", "hijack.txt"]);
        scratch_git(ws.dir(), &["commit", "-q", "-m", "worker commit"]);

        let err = ws.seal().await.unwrap_err().to_string();
        assert!(
            err.contains(&format!("refs/heads/{branch}")),
            "the refusal must name the moved ref: {err}"
        );
        assert!(
            err.contains("git update-ref"),
            "the refusal must spell out recovery: {err}"
        );

        ws.remove().await;
        std::fs::remove_dir_all(&root).ok();
    }

    /// The other half of #2541: witness scaffolding reached a branch the user
    /// keeps because the seal committed wherever the worker had left `HEAD`.
    /// The seal detaches first, so the shared branch cannot receive it — and
    /// the candidate's own work is untouched by the detach.
    #[tokio::test]
    async fn the_seal_never_lands_on_a_branch_the_user_keeps() {
        let root = scaffold("refesc-detach");
        let branch = default_branch(&root);
        let ws = port(root.clone()).create_workspace().await.unwrap();
        let branch_ref = format!("refs/heads/{branch}");
        let before = scratch_git(&root, &["rev-parse", &branch_ref])
            .trim()
            .to_string();

        // The worker attaches HEAD but commits nothing: the only thing that
        // would move the shared branch is Stella's own seal.
        scratch_git(
            ws.dir(),
            &["checkout", "--ignore-other-worktrees", "-q", &branch],
        );
        std::fs::write(ws.dir().join("witness_lost_changes.sh"), "#!/bin/sh\n").unwrap();

        ws.seal().await.expect("an unescaped candidate still seals");

        assert_eq!(
            scratch_git(&root, &["rev-parse", &branch_ref]).trim(),
            before,
            "the seal must not move a branch the graded tree depends on"
        );
        assert!(
            git(ws.dir(), &["symbolic-ref", "-q", "HEAD"])
                .await
                .is_err(),
            "the seal must leave the candidate's HEAD detached"
        );
        // The detach is metadata only — the worker's file is still there and
        // is what the seal captured.
        assert!(ws.dir().join("witness_lost_changes.sh").exists());
        assert!(
            ws.sealed_is_unchanged().await.unwrap(),
            "the seal captured the candidate's tree as the worker left it"
        );

        ws.remove().await;
        std::fs::remove_dir_all(&root).ok();
    }

    /// A user committing in their own checkout while a run is in flight moves
    /// a shared ref too. That is the false positive this signature exists to
    /// exclude — the candidate must still seal.
    #[tokio::test]
    async fn a_mid_run_user_commit_is_not_a_ref_escape() {
        let root = scaffold("refesc-user-commit");
        let ws = port(root.clone()).create_workspace().await.unwrap();

        std::fs::write(root.join("user.txt"), "user\n").unwrap();
        scratch_git(&root, &["add", "user.txt"]);
        scratch_git(&root, &["commit", "-q", "-m", "the user's own commit"]);

        ws.seal()
            .await
            .expect("a user's own commit must never fail a candidate's seal");

        ws.remove().await;
        std::fs::remove_dir_all(&root).ok();
    }

    /// A branch rewound into history the graded tree already had is trivially
    /// an ancestor of the candidate's HEAD — and is still the user's doing.
    #[tokio::test]
    async fn a_mid_run_user_rewind_is_not_a_ref_escape() {
        let root = scaffold("refesc-user-rewind");
        // Two commits so there is somewhere to rewind to.
        std::fs::write(root.join("second.txt"), "second\n").unwrap();
        scratch_git(&root, &["add", "second.txt"]);
        scratch_git(&root, &["commit", "-q", "-m", "second"]);
        let ws = port(root.clone()).create_workspace().await.unwrap();

        scratch_git(&root, &["reset", "--hard", "-q", "HEAD~1"]);

        ws.seal()
            .await
            .expect("a user's own rewind must never fail a candidate's seal");

        ws.remove().await;
        std::fs::remove_dir_all(&root).ok();
    }

    /// The module's other signature: candidate history reaching a shared ref
    /// without the candidate ever committing onto it. `git stash` is the
    /// everyday shape — `refs/stash`'s first parent is the candidate's HEAD —
    /// and the stash is shared machine state the module doc bans outright.
    #[tokio::test]
    async fn stashing_from_a_candidate_refuses_the_seal() {
        let root = scaffold("refesc-stash");
        let ws = port(root.clone()).create_workspace().await.unwrap();

        std::fs::write(ws.dir().join("tracked.txt"), "base\ndirty\ncandidate\n").unwrap();
        scratch_git(ws.dir(), &["stash", "push", "-q", "-m", "escape"]);

        let err = ws.seal().await.unwrap_err().to_string();
        assert!(
            err.contains("refs/stash"),
            "the refusal must name the stash ref: {err}"
        );

        ws.remove().await;
        std::fs::remove_dir_all(&root).ok();
    }

    /// A candidate that touches no ref pays one enumeration and seals.
    #[tokio::test]
    async fn an_ordinary_candidate_seals_with_no_ref_findings() {
        let root = scaffold("refesc-clean");
        let ws = port(root.clone()).create_workspace().await.unwrap();

        std::fs::write(ws.dir().join("answer.js"), "const answer = 42;\n").unwrap();
        ws.seal().await.unwrap();

        assert_eq!(ws.escaped_refs_inner().await, Vec::new());

        ws.remove().await;
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_refusal_names_a_created_ref_with_a_delete_recovery() {
        let reason = refusal(&[RefEscape {
            name: "refs/heads/scratch".to_string(),
            before: None,
            after: "abc1234".to_string(),
        }]);
        assert!(reason.contains("refs/heads/scratch (created, now abc1234)"));
        assert!(reason.contains("git update-ref -d refs/heads/scratch"));
    }

    #[test]
    fn the_refusal_quotes_the_value_recorded_at_creation() {
        let reason = refusal(&[RefEscape {
            name: "refs/heads/master".to_string(),
            before: Some("1111111".to_string()),
            after: "2222222".to_string(),
        }]);
        assert!(reason.contains("refs/heads/master (was 1111111, now 2222222)"));
        assert!(reason.contains("git update-ref refs/heads/master 1111111"));
    }
}
