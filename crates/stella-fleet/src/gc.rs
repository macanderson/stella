//! Reclaiming what a fleet run leaves behind: isolated worktrees under
//! `.stella/worktrees/` and their `fleet/*` branches.
//!
//! Every isolated task of every run gets a fresh directory and a fresh branch
//! (the slug hashes the run scope, so re-runs never collide), and until this
//! module existed nothing ever removed either — the terminal state of a
//! long-lived workspace was disk exhaustion (#1217). Leaving them is right
//! *for review*; leaving them forever is not.
//!
//! The whole design point is that a garbage collector which eats live work is
//! worse than no garbage collector. So [`Gc::sweep`] is conservative on every
//! axis, and each decision it makes is reported with the reason ([`KeepReason`])
//! rather than performed in silence:
//!
//! - **Namespace.** Only worktrees whose path is under `.stella/worktrees/`
//!   and whose branch carries the `fleet/` prefix are candidates. The main
//!   checkout and any branch outside that namespace are never arguments to a
//!   `git` mutation — not even under `--force`.
//! - **In flight.** An attempt row is opened in the ledger *before* its worker
//!   runs, so an unfinished attempt naming a worktree means a run may still be
//!   using it. Those are kept unconditionally — `force` does not override a
//!   live run, only a judgment call about work at rest.
//! - **Work at rest.** A dirty working tree, or a branch carrying commits that
//!   are not yet contained in the integration ref, is kept unless the caller
//!   passes [`GcOptions::force`].
//! - **Errors keep.** Every predicate answers "keep" when git cannot answer it
//!   (an unresolvable range, a `merge-base` that failed for its own reasons),
//!   the same direction [`WorktreeManager::remove`] already errs.
//!
//! [`WorktreeManager::remove`]: crate::git::WorktreeManager::remove

use std::path::{Path, PathBuf};

use crate::git::{GitCli, WorktreeError, ensure_ok, parse_worktree_list, path_arg};

/// Milliseconds in a day — the unit [`GcOptions::min_age_days`] is expressed
/// in.
const MS_PER_DAY: u64 = 24 * 60 * 60 * 1000;

/// What the ledger knows about one worktree path: whether an attempt using it
/// is still in flight, and when its last attempt finished.
///
/// Produced by `Ledger::worktree_activity`; kept as a plain owned value so the
/// sweep is decidable without a database (its tests drive it directly).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeActivity {
    /// The path exactly as the dispatching fleet stamped it into `attempts`.
    pub worktree_path: String,
    /// Attempts naming this worktree that have no `finished_at_ms` — a run
    /// still working, or one that died before stamping its outcome.
    pub unfinished_attempts: u32,
    /// The most recent `finished_at_ms` across this worktree's attempts.
    pub last_finished_ms: Option<u64>,
}

/// How aggressive one sweep is allowed to be. The default is the safe one:
/// act, but only on worktrees and branches whose work is provably already in
/// the integration ref.
#[derive(Debug, Clone, Default)]
pub struct GcOptions {
    /// Decide and report, mutate nothing.
    pub dry_run: bool,
    /// Also reclaim a dirty worktree or a branch holding unmerged commits.
    /// Never overrides the in-flight or namespace rules.
    pub force: bool,
    /// Only consider a worktree whose last attempt finished at least this many
    /// days ago. A worktree the ledger has no finish time for cannot be shown
    /// to be old enough, so it is kept while this is set.
    pub min_age_days: Option<u32>,
    /// The ref a branch must already be contained in to count as reclaimable.
    /// Defaults to the detected integration ref (`origin/HEAD`, else `main`,
    /// else `master`).
    pub base_ref: Option<String>,
}

/// Why a candidate survived the sweep. Every kept worktree and branch carries
/// one, because "kept for review" is only a statement if something says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeepReason {
    /// The ledger has an unfinished attempt naming this worktree. Never
    /// overridden by `--force`.
    InFlight,
    /// `git status --porcelain` is non-empty: uncommitted or untracked files.
    Dirty,
    /// The branch has commits that the integration ref does not contain.
    UnmergedCommits,
    /// A detached worktree: there is no branch to judge, so there is no
    /// evidence its commits are reachable from anywhere.
    Detached,
    /// The checked-out branch is outside the `fleet/` namespace.
    ForeignBranch,
    /// `--age` is set and the last attempt finished more recently than that.
    TooRecent { age_days: u64 },
    /// `--age` is set and the ledger records no finish time for this worktree,
    /// so its age cannot be established.
    AgeUnknown,
}

impl std::fmt::Display for KeepReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InFlight => f.write_str("an attempt is still in flight"),
            Self::Dirty => f.write_str("uncommitted changes (use --force)"),
            Self::UnmergedCommits => f.write_str("commits not in the base ref (use --force)"),
            Self::Detached => f.write_str("detached checkout, no branch to judge"),
            Self::ForeignBranch => f.write_str("branch is outside the fleet/ namespace"),
            Self::TooRecent { age_days } => {
                write!(f, "finished {age_days}d ago, younger than --age")
            }
            Self::AgeUnknown => f.write_str("no recorded finish time, age unknown"),
        }
    }
}

/// What the sweep did with one worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeAction {
    /// Removed, with whether its branch went too.
    Removed { branch_deleted: bool },
    /// A dry run: this is what a real sweep would have done.
    WouldRemove { branch_would_delete: bool },
    /// Left in place, with the reason.
    Kept(KeepReason),
    /// Git refused. The sweep continues to the next candidate rather than
    /// abandoning the ones it could still reclaim.
    Failed(String),
}

/// What the sweep did with one `fleet/*` branch that has no worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchAction {
    Deleted,
    WouldDelete,
    Kept(KeepReason),
    Failed(String),
}

/// One worktree's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeVerdict {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub action: WorktreeAction,
}

/// One branch's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchVerdict {
    pub branch: String,
    pub action: BranchAction,
}

/// Everything one sweep decided — the whole report, kept entries included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcReport {
    /// The ref merged-ness was judged against.
    pub base_ref: String,
    pub dry_run: bool,
    pub worktrees: Vec<WorktreeVerdict>,
    /// `fleet/*` branches with no worktree of their own (a worktree removed by
    /// hand, or by an earlier sweep that preserved the branch).
    pub branches: Vec<BranchVerdict>,
}

impl GcReport {
    /// How many worktrees were (or would be) removed.
    #[must_use]
    pub fn reclaimed_worktrees(&self) -> usize {
        self.worktrees
            .iter()
            .filter(|v| {
                matches!(
                    v.action,
                    WorktreeAction::Removed { .. } | WorktreeAction::WouldRemove { .. }
                )
            })
            .count()
    }

    /// How many branches were (or would be) deleted, worktree-attached ones
    /// included.
    #[must_use]
    pub fn reclaimed_branches(&self) -> usize {
        let attached = self
            .worktrees
            .iter()
            .filter(|v| {
                matches!(
                    v.action,
                    WorktreeAction::Removed {
                        branch_deleted: true
                    } | WorktreeAction::WouldRemove {
                        branch_would_delete: true
                    }
                )
            })
            .count();
        let loose = self
            .branches
            .iter()
            .filter(|v| matches!(v.action, BranchAction::Deleted | BranchAction::WouldDelete))
            .count();
        attached + loose
    }

    /// Whether anything failed — the caller's exit-code signal.
    #[must_use]
    pub fn had_failures(&self) -> bool {
        self.worktrees
            .iter()
            .any(|v| matches!(v.action, WorktreeAction::Failed(_)))
            || self
                .branches
                .iter()
                .any(|v| matches!(v.action, BranchAction::Failed(_)))
    }
}

/// Failures a sweep can end on — as opposed to a per-candidate refusal, which
/// is reported in the [`GcReport`] and never aborts the sweep.
#[derive(Debug, thiserror::Error)]
pub enum GcError {
    #[error(transparent)]
    Worktree(#[from] WorktreeError),

    /// Nothing resolvable to judge merged-ness against, and no `--base` given.
    /// Deleting a branch on a guess is exactly the failure mode this module
    /// exists to avoid, so the sweep stops instead.
    #[error(
        "cannot determine the base ref to judge merged work against \
         (tried origin/HEAD, main, master) — pass an explicit base ref"
    )]
    NoBaseRef,
}

/// The garbage collector over a [`GitCli`], rooted at a repository.
///
/// Constructed the same way [`WorktreeManager`](crate::git::WorktreeManager)
/// is, and deliberately a separate type: creating a worktree is a run-scoped
/// operation (the scope is hashed into every slug), while a sweep is
/// workspace-wide and belongs to no run.
pub struct Gc<G: GitCli> {
    git: G,
    repo_root: PathBuf,
    worktrees_root: PathBuf,
    branch_prefix: String,
}

impl<G: GitCli> Gc<G> {
    /// A collector for `repo_root`, sweeping `<repo_root>/.stella/worktrees/`
    /// and the `fleet/` branch namespace — the exact two places
    /// [`WorktreeManager::create`](crate::git::WorktreeManager::create) writes.
    #[must_use]
    pub fn new(git: G, repo_root: impl Into<PathBuf>) -> Self {
        let repo_root = repo_root.into();
        let worktrees_root = repo_root.join(".stella").join("worktrees");
        Self {
            git,
            repo_root,
            worktrees_root,
            branch_prefix: "fleet/".to_string(),
        }
    }

    /// Decide and (unless `dry_run`) act on every fleet worktree and loose
    /// `fleet/*` branch. `now_ms` is the caller's clock, so the `--age`
    /// arithmetic is testable without waiting.
    pub async fn sweep(
        &self,
        activity: &[WorktreeActivity],
        opts: &GcOptions,
        now_ms: u64,
    ) -> Result<GcReport, GcError> {
        let base_ref = match opts.base_ref.as_deref() {
            Some(explicit) => explicit.to_string(),
            None => self.detect_base_ref().await?,
        };

        // Drop admin records for worktrees whose directory a human already
        // deleted: without this they are listed forever and every later
        // `git worktree add` reasons about a checkout that is not there.
        if !opts.dry_run {
            let out = self
                .git
                .run(&self.repo_root, &["worktree", "prune"])
                .await
                .map_err(WorktreeError::from)?;
            ensure_ok(out, "worktree prune").map_err(GcError::from)?;
        }

        let worktrees = self
            .sweep_worktrees(activity, opts, &base_ref, now_ms)
            .await?;
        let branches = self
            .sweep_loose_branches(&worktrees, opts, &base_ref)
            .await?;

        Ok(GcReport {
            base_ref,
            dry_run: opts.dry_run,
            worktrees,
            branches,
        })
    }

    async fn sweep_worktrees(
        &self,
        activity: &[WorktreeActivity],
        opts: &GcOptions,
        base_ref: &str,
        now_ms: u64,
    ) -> Result<Vec<WorktreeVerdict>, GcError> {
        let out = self
            .git
            .run(&self.repo_root, &["worktree", "list", "--porcelain"])
            .await
            .map_err(WorktreeError::from)?;
        let out = ensure_ok(out, "worktree list --porcelain").map_err(GcError::from)?;
        let entries = parse_worktree_list(&out.stdout);

        let mut verdicts = Vec::new();
        for entry in entries {
            // The main checkout is never a candidate, and neither is any
            // worktree a human made outside `.stella/worktrees/` — this is the
            // namespace rule, and it is checked before anything else so no
            // later branch of the logic can reach a `git` mutation for them.
            if !self.under_worktrees_root(&entry.path) {
                continue;
            }
            let action = self
                .judge_worktree(
                    &entry.path,
                    entry.branch.as_deref(),
                    activity,
                    opts,
                    base_ref,
                    now_ms,
                )
                .await?;
            let action = match action {
                Verdict::Keep(reason) => WorktreeAction::Kept(reason),
                Verdict::Reclaim => {
                    let branch = entry.branch.as_deref().unwrap_or_default().to_string();
                    if opts.dry_run {
                        WorktreeAction::WouldRemove {
                            branch_would_delete: !branch.is_empty(),
                        }
                    } else {
                        match self.remove_worktree(&entry.path, &branch, opts.force).await {
                            Ok(branch_deleted) => WorktreeAction::Removed { branch_deleted },
                            Err(e) => WorktreeAction::Failed(e.to_string()),
                        }
                    }
                }
            };
            verdicts.push(WorktreeVerdict {
                path: entry.path,
                branch: entry.branch,
                action,
            });
        }
        Ok(verdicts)
    }

    /// Whether `path` names something inside `.stella/worktrees/` — the
    /// namespace test, and the only thing standing between a sweep and the
    /// main checkout.
    ///
    /// Compared on canonical paths when both sides resolve, because `git
    /// worktree list` reports the *resolved* path while the workspace root a
    /// caller hands us may travel through a symlink (macOS `/var` →
    /// `/private/var` is the everyday case, and a repo under a symlinked home
    /// is the same shape). A textual comparison there answers "outside the
    /// namespace" for every fleet worktree in the repository, which is safe
    /// but useless — it collects nothing. When either side cannot be resolved
    /// the raw comparison still has to agree, so an unresolvable path is
    /// never *promoted* into the namespace.
    fn under_worktrees_root(&self, path: &Path) -> bool {
        if path.starts_with(&self.worktrees_root) {
            return true;
        }
        let (Ok(root), Ok(candidate)) = (
            std::fs::canonicalize(&self.worktrees_root),
            std::fs::canonicalize(path),
        ) else {
            return false;
        };
        candidate.starts_with(&root)
    }

    /// The per-worktree decision. Split out so every "keep" has exactly one
    /// place it can come from.
    async fn judge_worktree(
        &self,
        path: &Path,
        branch: Option<&str>,
        activity: &[WorktreeActivity],
        opts: &GcOptions,
        base_ref: &str,
        now_ms: u64,
    ) -> Result<Verdict, GcError> {
        let Some(branch) = branch else {
            return Ok(Verdict::Keep(KeepReason::Detached));
        };
        if !branch.starts_with(&self.branch_prefix) {
            return Ok(Verdict::Keep(KeepReason::ForeignBranch));
        }

        // In flight beats everything, `--force` included: force is a judgment
        // about work at rest, never a licence to yank the tree out from under
        // a running worker.
        let record = activity
            .iter()
            .find(|a| same_worktree(Path::new(&a.worktree_path), path));
        if record.is_some_and(|a| a.unfinished_attempts > 0) {
            return Ok(Verdict::Keep(KeepReason::InFlight));
        }

        if let Some(min_days) = opts.min_age_days {
            let Some(finished_ms) = record.and_then(|a| a.last_finished_ms) else {
                return Ok(Verdict::Keep(KeepReason::AgeUnknown));
            };
            let age_days = now_ms.saturating_sub(finished_ms) / MS_PER_DAY;
            if age_days < u64::from(min_days) {
                return Ok(Verdict::Keep(KeepReason::TooRecent { age_days }));
            }
        }

        if !opts.force {
            if self.is_dirty(path).await? {
                return Ok(Verdict::Keep(KeepReason::Dirty));
            }
            if !self.is_contained_in(branch, base_ref).await? {
                return Ok(Verdict::Keep(KeepReason::UnmergedCommits));
            }
        }
        Ok(Verdict::Reclaim)
    }

    /// `git worktree remove` then `branch -D`, in that order (git refuses to
    /// delete a branch that is still checked out somewhere). Returns whether
    /// the branch went with it.
    async fn remove_worktree(
        &self,
        path: &Path,
        branch: &str,
        force: bool,
    ) -> Result<bool, WorktreeError> {
        let path_str = path_arg(path)?;
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(path_str);
        let out = self.git.run(&self.repo_root, &args).await?;
        ensure_ok(out, &format!("worktree remove {path_str}"))?;

        if branch.is_empty() || !branch.starts_with(&self.branch_prefix) {
            return Ok(false);
        }
        let out = self
            .git
            .run(&self.repo_root, &["branch", "-D", branch])
            .await?;
        ensure_ok(out, &format!("branch -D {branch}"))?;
        Ok(true)
    }

    /// The second half of the accumulation: a `fleet/*` branch whose worktree
    /// is already gone. Only branches nothing has checked out are considered,
    /// and the same containment rule decides them.
    async fn sweep_loose_branches(
        &self,
        worktrees: &[WorktreeVerdict],
        opts: &GcOptions,
        base_ref: &str,
    ) -> Result<Vec<BranchVerdict>, GcError> {
        let pattern = format!("refs/heads/{}*", self.branch_prefix);
        let out = self
            .git
            .run(
                &self.repo_root,
                &["for-each-ref", "--format=%(refname:short)", &pattern],
            )
            .await
            .map_err(WorktreeError::from)?;
        let out = ensure_ok(out, "for-each-ref refs/heads/fleet/*").map_err(GcError::from)?;

        // A branch still attached to a worktree this sweep kept (or failed to
        // remove) is that worktree's business, not a loose branch.
        let attached: Vec<&str> = worktrees
            .iter()
            .filter(|v| !matches!(v.action, WorktreeAction::Removed { .. }))
            .filter_map(|v| v.branch.as_deref())
            .collect();

        let mut verdicts = Vec::new();
        for branch in out.stdout.lines().map(str::trim).filter(|l| !l.is_empty()) {
            // Belt and braces: `for-each-ref` was already scoped to the
            // namespace, but nothing outside it may reach `branch -D`.
            if !branch.starts_with(&self.branch_prefix) || attached.contains(&branch) {
                continue;
            }
            if !opts.force && !self.is_contained_in(branch, base_ref).await? {
                verdicts.push(BranchVerdict {
                    branch: branch.to_string(),
                    action: BranchAction::Kept(KeepReason::UnmergedCommits),
                });
                continue;
            }
            let action = if opts.dry_run {
                BranchAction::WouldDelete
            } else {
                match self
                    .git
                    .run(&self.repo_root, &["branch", "-D", branch])
                    .await
                {
                    Ok(out) if out.success => BranchAction::Deleted,
                    Ok(out) => BranchAction::Failed(out.stderr.trim().to_string()),
                    Err(e) => BranchAction::Failed(e.to_string()),
                }
            };
            verdicts.push(BranchVerdict {
                branch: branch.to_string(),
                action,
            });
        }
        Ok(verdicts)
    }

    /// Whether `repo` has uncommitted or untracked changes. A worktree git
    /// cannot report on counts as dirty: unreadable is not the same as clean.
    async fn is_dirty(&self, repo: &Path) -> Result<bool, GcError> {
        let out = self
            .git
            .run(repo, &["status", "--porcelain"])
            .await
            .map_err(WorktreeError::from)?;
        if !out.success {
            return Ok(true);
        }
        Ok(!out.stdout.trim().is_empty())
    }

    /// Whether every commit on `branch` is already reachable from `base_ref`
    /// (`git merge-base --is-ancestor`). Exit 1 is git's honest "no"; any
    /// other failure means git could not answer, and an unanswered question
    /// keeps the branch.
    async fn is_contained_in(&self, branch: &str, base_ref: &str) -> Result<bool, GcError> {
        let out = self
            .git
            .run(
                &self.repo_root,
                &["merge-base", "--is-ancestor", branch, base_ref],
            )
            .await
            .map_err(WorktreeError::from)?;
        Ok(out.success)
    }

    /// The ref merged-ness is judged against when the caller names none:
    /// `origin/HEAD` if the remote publishes one, else a local `main`, else
    /// `master`.
    async fn detect_base_ref(&self) -> Result<String, GcError> {
        let out = self
            .git
            .run(
                &self.repo_root,
                &[
                    "symbolic-ref",
                    "--quiet",
                    "--short",
                    "refs/remotes/origin/HEAD",
                ],
            )
            .await
            .map_err(WorktreeError::from)?;
        if out.success {
            let name = out.stdout.trim();
            if !name.is_empty() {
                return Ok(name.to_string());
            }
        }
        for candidate in ["main", "master"] {
            let out = self
                .git
                .run(
                    &self.repo_root,
                    &["rev-parse", "--verify", "--quiet", candidate],
                )
                .await
                .map_err(WorktreeError::from)?;
            if out.success && !out.stdout.trim().is_empty() {
                return Ok(candidate.to_string());
            }
        }
        Err(GcError::NoBaseRef)
    }
}

/// Whether a ledger-stamped worktree path and a `git worktree list` path name
/// the same checkout.
///
/// Three rungs, widest last, because a *missed* match is the dangerous
/// direction: it hides an in-flight attempt and the sweep would then judge a
/// live worktree on its files alone. Textual equality, then canonical
/// equality (the ledger stamps the path the dispatching fleet built from the
/// workspace root; git reports the resolved one — they differ under any
/// symlink, `/var` → `/private/var` on macOS being the everyday case), and
/// finally the directory name, which is a run-scope+task-id hash suffixed
/// slug and therefore already unique per workspace. An over-match on that last
/// rung only ever attributes *more* history to a worktree, which keeps it.
fn same_worktree(ledger_path: &Path, listed: &Path) -> bool {
    if ledger_path == listed {
        return true;
    }
    if let (Ok(a), Ok(b)) = (
        std::fs::canonicalize(ledger_path),
        std::fs::canonicalize(listed),
    ) && a == b
    {
        return true;
    }
    match (ledger_path.file_name(), listed.file_name()) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// The internal two-way outcome of [`Gc::judge_worktree`], before it is turned
/// into the reported (dry-run aware) action.
enum Verdict {
    Keep(KeepReason),
    Reclaim,
}

#[cfg(test)]
mod tests;
