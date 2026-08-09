//! Vendor-neutral repository tools: `repo_status`, `repo_diff`,
//! `repo_commit`, `repo_push`, `repo_pull`, `repo_rollback`.
//!
//! Nothing here says "git" — not the tool names, not the argument names.
//! The tools speak in repository concepts (branch, paths, message) through
//! the [`RepoBackend`] port; today's only adapter is [`GitCli`], which
//! shells to `git` via direct argv spawns (no shell interpretation, no new
//! crates), and a future VCS is a new adapter, never a tool rewrite.
//!
//! Structural rules live in the TOOL layer, above the backend, so no
//! adapter can forget them:
//!
//! - `repo_commit` and `repo_rollback` require a **non-empty, explicit
//!   path list** — there is no `-A`, and a whole-tree operation must be
//!   spelled out path by path, never implied by an empty-args call. Magic
//!   pathspecs (anything starting `:`) are refused for the same reason:
//!   `:/` is git's spelling of "the whole repository".
//! - `repo_push` **refuses the repository's default branch** (resolved
//!   from the remote HEAD). No override exists, and force-push does not
//!   exist in this surface at all.
//! - `repo_pull` is **fast-forward only** — divergence is a typed error,
//!   not a merge.
//! - History rewriting (`reset --hard`, rebase, amend) is deliberately
//!   absent: `repo_rollback` restores named paths to the last committed
//!   state and that is the only "undo" this surface offers.
//! - `repo_diff` caps its patch payload at `MAX_PATCH_BYTES` with a
//!   **loud elision marker**, so a capped review can never be mistaken
//!   for a complete one.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use stella_protocol::tool::{ToolOutput, ToolSchema};

use crate::exec;
use crate::registry::Tool;

/// Timeout for backend commands (push/pull are network-bound).
const REPO_TIMEOUT_SECS: u64 = 300;
/// Cap on `repo_status` changed-file rows (and `repo_diff` summary rows).
const MAX_CHANGED_ROWS: usize = 200;

/// Cap on unreachable commits listed by `repo_status`. Deliberately far below
/// [`MAX_CHANGED_ROWS`]: a caller is looking for work it lost, and a handful of
/// candidates is a lead while two hundred is a haystack. A repository with more
/// than this many orphans is not one where the list is the answer, and the flag
/// beside it says so rather than the payload pretending it enumerated them.
const MAX_UNREACHABLE_ROWS: usize = 20;
/// Cap on `repo_diff` patch bytes; beyond it the patch is cut at a line
/// boundary and the elision is loud (see the module doc). Sits below the
/// shared runner's 30k output cap so this cut, not the runner's middle-out
/// truncation, is the one the agent normally sees.
const MAX_PATCH_BYTES: usize = 24_000;

/// Typed, named failures crossing the [`RepoBackend`] port.
#[derive(Debug)]
pub enum RepoError {
    /// The VCS binary is missing or cannot start at all.
    Unavailable(String),
    /// A backend command ran and failed; `detail` carries its output.
    Failed {
        action: &'static str,
        detail: String,
    },
    /// Local and remote histories diverged — `repo_pull` is fast-forward
    /// only by contract, and history rewriting is out of scope.
    Diverged(String),
}

impl std::fmt::Display for RepoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RepoError::Unavailable(detail) => {
                write!(f, "repository backend unavailable: {detail}")
            }
            RepoError::Failed { action, detail } => write!(f, "{action} failed: {detail}"),
            RepoError::Diverged(detail) => write!(
                f,
                "histories diverged — repo_pull is fast-forward-only and never merges or \
                 rewrites; reconcile manually. {detail}"
            ),
        }
    }
}

/// One changed file in a [`RepoStatus`] — `state` is the backend's short
/// two-column status code (e.g. ` M`, `??`, `A `).
#[derive(Debug, Clone, Serialize)]
pub struct ChangedFile {
    pub state: String,
    pub path: String,
}

/// One commit, named enough to act on.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CommitRef {
    /// Abbreviated object name.
    pub commit: String,
    /// First line of the message.
    pub subject: String,
}

/// Which checkout of the repository this is.
///
/// Without it, an agent running in a Stella candidate workspace reads
/// `branch: null` and concludes something is wrong with the repository — the
/// shadow is created with `git worktree add --detach`, so it is genuinely on
/// no branch, and nothing in the payload said why. Observed costing a bash
/// round-trip to `git status` on the first turn of a real session, purely to
/// re-derive what this field states.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorktreeContext {
    /// True when this is a linked worktree rather than the repository's main
    /// checkout.
    pub linked: bool,
    /// True when this is a Stella candidate workspace — an isolated shadow
    /// whose edits are adopted back into the real tree. Detached by design.
    pub candidate_shadow: bool,
    /// The branch the repository's MAIN checkout is on. A caller in a detached
    /// shadow almost always wants this rather than its own `branch: null`: it
    /// is the branch the work will land on.
    pub main_branch: Option<String>,
}

/// The `repo_status` payload: typed rows, bounded.
#[derive(Debug, Clone, Serialize)]
pub struct RepoStatus {
    /// Current branch; `None` when the checkout is detached.
    pub branch: Option<String>,
    /// Which checkout this is, and what the main one is on. `None` outside a
    /// repository.
    pub worktree: Option<WorktreeContext>,
    /// Where `HEAD` actually points. Always populated, and the only way to
    /// read the state at all when [`Self::branch`] is `None` — a detached
    /// checkout reported `branch: null` and nothing else, which is true and
    /// says nothing about where you are.
    pub head: Option<CommitRef>,
    /// Commits ahead of upstream; `None` when no upstream is configured.
    pub ahead: Option<u32>,
    /// Commits behind upstream; `None` when no upstream is configured.
    pub behind: Option<u32>,
    /// Changed files, capped at `MAX_CHANGED_ROWS`.
    pub changed: Vec<ChangedFile>,
    /// True when the changed-file list was truncated at the cap.
    pub truncated: bool,
    /// Commits the reflog can still reach that **no branch, tag or remote
    /// can** — work orphaned by a checkout, a reset, or an abandoned rebase.
    ///
    /// The rest of this payload is working-tree state: what is modified,
    /// staged, untracked, and where a branch sits against its upstream. None
    /// of it can express "a commit exists and nothing points at it", so a
    /// caller asking about lost work got a truthful, clean, entirely useless
    /// answer and had to go to raw `git fsck` / `git reflog` to learn anything
    /// (observed on Terminal-Bench `fix-git`, whose whole task is this shape).
    ///
    /// Capped at [`MAX_UNREACHABLE_ROWS`]; the pipeline's own bookkeeping
    /// commits are never listed (see [`GitCli::unreachable_commits`]).
    pub unreachable: Vec<CommitRef>,
    /// True when the unreachable list was truncated at the cap.
    pub unreachable_truncated: bool,
    /// Stash entries. Zero is a fact worth having — it is how a caller rules
    /// the stash out as the place missing work went, rather than guessing.
    pub stashes: u32,
}

/// One changed file in a [`RepoDiff`] summary; line counts are `None` for
/// a binary change.
#[derive(Debug, Clone)]
pub struct DiffFileStat {
    pub path: String,
    /// Lines added, `None` for a binary change.
    pub added: Option<u64>,
    /// Lines removed, `None` for a binary change.
    pub removed: Option<u64>,
}

/// The `repo_diff` payload: per-file line stats plus the raw patch hunks.
/// The size caps live in the TOOL layer (see the module doc), so the port
/// carries the backend's full answer.
#[derive(Debug, Clone)]
pub struct RepoDiff {
    pub files: Vec<DiffFileStat>,
    pub patch: String,
}

/// Vendor-neutral repository port. Adapters translate these operations to
/// their VCS; the structural refusals (default-branch push, empty path
/// lists) live in the tools, not here — see the module doc.
#[async_trait]
pub trait RepoBackend: Send + Sync {
    async fn status(&self, root: &Path) -> Result<RepoStatus, RepoError>;
    /// Pending changes as patch hunks plus per-file line stats. `staged`
    /// selects changes already staged for commit instead of unstaged ones;
    /// a non-empty `paths` scopes the diff to those paths.
    async fn diff(
        &self,
        root: &Path,
        staged: bool,
        paths: &[String],
    ) -> Result<RepoDiff, RepoError>;
    /// Current branch, `None` when detached.
    async fn current_branch(&self, root: &Path) -> Result<Option<String>, RepoError>;
    /// The repository's default branch per the remote HEAD, `None` when it
    /// cannot be determined.
    async fn default_branch(&self, root: &Path) -> Result<Option<String>, RepoError>;
    /// Stage exactly `paths` and commit them with `message`; returns a
    /// one-line summary of the created commit.
    async fn commit_paths(
        &self,
        root: &Path,
        message: &str,
        paths: &[String],
    ) -> Result<String, RepoError>;
    /// Push `branch` to the primary remote (upstream set; never forced).
    async fn push_branch(&self, root: &Path, branch: &str) -> Result<String, RepoError>;
    /// Fast-forward-only pull; divergence is [`RepoError::Diverged`].
    async fn pull_ff_only(&self, root: &Path) -> Result<String, RepoError>;
    /// Restore exactly `paths` to the last committed state.
    async fn restore_paths(&self, root: &Path, paths: &[String]) -> Result<String, RepoError>;
}

/// The `git` CLI adapter — direct argv spawns through the shared runner
/// (process-group kill, `GIT_*` repo-env scrubbing, credential scrubbing,
/// output truncation).
///
/// Remote operations intentionally do not preserve environment-token auth.
/// Push can execute repository-controlled hooks and credential helpers, so an
/// AWS/GitHub token allowlist here would expose those secrets to untrusted
/// code. SSH agents and OS/configured credential helpers continue to work;
/// environment-only SCM auth requires a future isolated credential broker.
pub struct GitCli;

impl GitCli {
    async fn git(
        root: &Path,
        action: &'static str,
        args: &[&str],
    ) -> Result<(i32, String), RepoError> {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        exec::run_argv("git", &owned, root, REPO_TIMEOUT_SECS)
            .await
            .map_err(|e| {
                if e.contains("failed to spawn") {
                    RepoError::Unavailable(e)
                } else {
                    RepoError::Failed { action, detail: e }
                }
            })
    }

    /// Run and require exit 0; a nonzero exit is a named failure carrying
    /// the command's output.
    async fn git_ok(root: &Path, action: &'static str, args: &[&str]) -> Result<String, RepoError> {
        match Self::git(root, action, args).await? {
            (0, output) => Ok(output),
            (code, output) => Err(RepoError::Failed {
                action,
                detail: format!("exit {code}: {}", output.trim()),
            }),
        }
    }

    /// `-c` overrides supplying an author, or empty when the repository
    /// already resolves one.
    ///
    /// A fresh container configures no git identity at any scope, so `git
    /// commit` exits 128 with "Author identity unknown" and the agent can
    /// never commit at all — three failures in one benchmark cycle, and the
    /// cost is larger than the failed calls: the model then spent turns
    /// theorising that its work went ungraded *because* it was uncommitted,
    /// a theory this defect made impossible to disprove (#2059).
    ///
    /// Conditional rather than unconditional, and passed inline rather than
    /// written into the repository's config: committing as Stella on a
    /// developer's own repository would be its own bug, so this only rescues
    /// the case where git refuses outright. The probe tolerates a non-zero
    /// exit because `--get` of an unset key exits 1 — that is the answer,
    /// not a failure.
    async fn commit_identity(root: &Path) -> Vec<&'static str> {
        // `git commit` needs BOTH a name and an email; a config with only one
        // set (or a name git cannot derive from the container's passwd entry)
        // still exits 128 with "empty ident name ... not allowed", so probe
        // both keys and inject the overrides unless both already resolve.
        let name = Self::git(root, "repo_commit", &["config", "--get", "user.name"]).await;
        let email = Self::git(root, "repo_commit", &["config", "--get", "user.email"]).await;
        match (name, email) {
            (Ok((0, name)), Ok((0, email)))
                if !name.trim().is_empty() && !email.trim().is_empty() =>
            {
                Vec::new()
            }
            _ => vec![
                "-c",
                "user.name=Stella",
                "-c",
                "user.email=stella@localhost",
            ],
        }
    }

    /// [`Self::git_ok`] for output that is PARSED rather than shown: on success
    /// it yields **stdout alone**.
    ///
    /// git writes advice hints and warnings to stderr, and trace lines too when
    /// `GIT_TRACE` is set anywhere in the environment. The shared runner folds
    /// stderr into stdout, so any of that chatter lands inside the value the
    /// caller is about to parse. A branch name is the sharp case: `rev-parse
    /// --abbrev-ref HEAD` plus one hint becomes a "branch" that git rejects as
    /// `fatal: invalid refspec` when [`RepoBackend::push_branch`] builds a refspec
    /// from it. `verify` already moved its own reads to stdout-only for exactly
    /// this reason; these call sites had not.
    ///
    /// The failure arm deliberately keeps the MERGED output: on a nonzero exit
    /// the explanation is the point, and it is almost always on stderr.
    async fn git_ok_stdout(
        root: &Path,
        action: &'static str,
        args: &[&str],
    ) -> Result<String, RepoError> {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let (code, stdout, stderr) = exec::run_argv_split("git", &owned, root, REPO_TIMEOUT_SECS)
            .await
            .map_err(|e| {
                if e.contains("failed to spawn") {
                    RepoError::Unavailable(e)
                } else {
                    RepoError::Failed { action, detail: e }
                }
            })?;
        if code == 0 {
            return Ok(stdout);
        }
        let mut detail = stdout;
        if !stderr.is_empty() {
            if !detail.is_empty() {
                detail.push('\n');
            }
            detail.push_str(&stderr);
        }
        Err(RepoError::Failed {
            action,
            detail: format!("exit {code}: {}", detail.trim()),
        })
    }

    /// [`Self::git_ok_stdout`] for callers that tolerate a nonzero exit and
    /// branch on the code themselves (no upstream configured, no remote HEAD).
    async fn git_stdout(
        root: &Path,
        action: &'static str,
        args: &[&str],
    ) -> Result<(i32, String), RepoError> {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        exec::run_argv_split("git", &owned, root, REPO_TIMEOUT_SECS)
            .await
            .map(|(code, stdout, _stderr)| (code, stdout))
            .map_err(|e| {
                if e.contains("failed to spawn") {
                    RepoError::Unavailable(e)
                } else {
                    RepoError::Failed { action, detail: e }
                }
            })
    }
}

impl GitCli {
    /// Split `<abbrev> <subject>` lines into [`CommitRef`]s, bounded.
    ///
    /// Returns the flag beside the rows so a truncated answer says so rather
    /// than reading as a complete enumeration.
    /// Rows are `<abbrev>\t<author-email>\t<subject>`; `skip_author` drops the
    /// pipeline's own commits. Tab-delimited because a subject may contain
    /// spaces and an email may not contain a tab.
    fn parse_commit_lines(
        out: &str,
        cap: usize,
        skip_author: Option<&str>,
    ) -> (Vec<CommitRef>, bool) {
        let mut rows = Vec::new();
        let mut truncated = false;
        for line in out.lines().filter(|l| !l.trim().is_empty()) {
            let mut parts = line.splitn(3, '\t');
            let commit = parts.next().unwrap_or_default();
            let email = parts.next().unwrap_or_default();
            let subject = parts.next().unwrap_or_default();
            if skip_author.is_some_and(|skip| email == skip) {
                continue;
            }
            if rows.len() >= cap {
                truncated = true;
                break;
            }
            rows.push(CommitRef {
                commit: commit.to_string(),
                subject: subject.trim().to_string(),
            });
        }
        (rows, truncated)
    }

    /// Where `HEAD` points, named. `None` only in a repository with no commits.
    async fn head_commit(&self, root: &Path) -> Result<Option<CommitRef>, RepoError> {
        match Self::git_stdout(
            root,
            "status",
            &[
                "log",
                "-1",
                "--no-color",
                "--format=%h%x09%ae%x09%s",
                "HEAD",
            ],
        )
        .await?
        {
            (0, out) => Ok(Self::parse_commit_lines(&out, 1, None).0.into_iter().next()),
            // An unborn HEAD is an ordinary state, not a failure.
            _ => Ok(None),
        }
    }

    /// Commits the reflog still reaches that no branch, tag or remote does.
    ///
    /// `rev-list --all --reflog --not --branches --tags --remotes` rather than
    /// `git fsck --unreachable`: this is a ref walk, while fsck scans every
    /// object in the store. The two answer nearly the same question here — a
    /// commit orphaned by a checkout, reset or abandoned rebase is exactly what
    /// the reflog remembers and no ref points at — and only one of them is
    /// affordable to run on every `repo_status` call.
    ///
    /// **The pipeline's own snapshots are excluded.** A candidate workspace is
    /// a detached-HEAD shadow carrying baseline commits authored by
    /// [`crate::verify::CANDIDATE_SNAPSHOT_EMAIL`], which are unreachable by
    /// construction. Listing them here would do to every caller what it already
    /// did to one authored witness: present the harness's own bookkeeping as
    /// the user's lost work, and send the agent off to merge commits it must
    /// not touch.
    async fn unreachable_commits(&self, root: &Path) -> Result<(Vec<CommitRef>, bool), RepoError> {
        // `git log`, not `rev-list --format`: rev-list prints a `commit <sha>`
        // header line before each formatted row unless suppressed by a flag
        // that is newer than the git this may run against. Exclusion happens in
        // Rust — git can filter TO an author (`--author=`) but has no
        // invert-author, and `--invert-grep` applies only to `--grep`.
        match Self::git_stdout(
            root,
            "status",
            &[
                "log",
                "--no-color",
                "--format=%h%x09%ae%x09%s",
                // Enough headroom that dropping the pipeline's own commits
                // cannot leave the list short of the cap.
                "--max-count=60",
                "--all",
                "--reflog",
                "--not",
                "--branches",
                "--tags",
                "--remotes",
            ],
        )
        .await?
        {
            (0, out) => Ok(Self::parse_commit_lines(
                &out,
                MAX_UNREACHABLE_ROWS,
                Some(crate::verify::CANDIDATE_SNAPSHOT_EMAIL),
            )),
            // An old git without one of these flags, or a non-repository:
            // report nothing rather than guessing. Silence here is the
            // channel saying nothing, never "there is no lost work".
            _ => Ok((Vec::new(), false)),
        }
    }

    /// Which checkout this is, and the branch the main one is on.
    ///
    /// Every fact here comes from `git worktree list --porcelain`, whose FIRST
    /// record is always the main worktree — so one invocation answers both
    /// "am I a linked worktree" and "what is the real checkout on". The
    /// alternative, comparing `--git-dir` against `--git-common-dir`, answers
    /// only the first and needs a second call for the second.
    ///
    /// A candidate shadow is recognised by the per-worktree baseline ref the
    /// pipeline pins at creation, not by the directory name. The name is a
    /// convention (`stella_candidate_<pid>_<seq>`) that a temp dir could
    /// coincide with; the ref is the pipeline's own signature.
    async fn worktree_context(&self, root: &Path) -> Result<Option<WorktreeContext>, RepoError> {
        let (code, listing) =
            Self::git_stdout(root, "status", &["worktree", "list", "--porcelain"]).await?;
        if code != 0 {
            return Ok(None);
        }
        // The first record is the main worktree; its `branch` line is a full
        // ref, and a detached main checkout has none.
        let mut main_branch = None;
        for line in listing.lines() {
            if let Some(rest) = line.strip_prefix("branch ") {
                main_branch = Some(
                    rest.trim()
                        .strip_prefix("refs/heads/")
                        .unwrap_or(rest.trim())
                        .to_string(),
                );
                break;
            }
            // A blank line ends the first record: the main worktree is
            // detached, and later records are other worktrees, not it.
            if line.trim().is_empty() {
                break;
            }
        }
        let main_path = listing
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("worktree "))
            .map(str::trim);
        let linked = main_path.is_some_and(|main| {
            // Compare canonically: the listing reports `/private/var/...`
            // where the caller holds `/var/...` on macOS.
            let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            canon(Path::new(main)) != canon(root)
        });
        let candidate_shadow = matches!(
            Self::git_stdout(
                root,
                "status",
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    crate::verify::WITNESS_BASELINE_WORKTREE_REF,
                ],
            )
            .await,
            Ok((0, _))
        );
        Ok(Some(WorktreeContext {
            linked,
            candidate_shadow,
            main_branch,
        }))
    }

    /// How many stash entries exist. Zero rules the stash out; it is not noise.
    async fn stash_count(&self, root: &Path) -> Result<u32, RepoError> {
        match Self::git_stdout(root, "status", &["stash", "list"]).await? {
            (0, out) => Ok(out.lines().filter(|l| !l.trim().is_empty()).count() as u32),
            _ => Ok(0),
        }
    }
}

#[async_trait]
impl RepoBackend for GitCli {
    async fn status(&self, root: &Path) -> Result<RepoStatus, RepoError> {
        let branch = self.current_branch(root).await?;
        // No upstream configured is the common non-error case → None/None.
        let (ahead, behind) = match Self::git_stdout(
            root,
            "status",
            &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
        )
        .await?
        {
            (0, out) => {
                let mut it = out.split_whitespace();
                let ahead = it.next().and_then(|n| n.parse::<u32>().ok());
                let behind = it.next().and_then(|n| n.parse::<u32>().ok());
                (ahead, behind)
            }
            _ => (None, None),
        };
        let porcelain = Self::git_ok_stdout(root, "status", &["status", "--porcelain"]).await?;
        let mut changed = Vec::new();
        let mut truncated = false;
        for line in porcelain.lines() {
            if line.len() < 4 {
                continue;
            }
            if changed.len() >= MAX_CHANGED_ROWS {
                truncated = true;
                break;
            }
            changed.push(ChangedFile {
                state: line[..2].to_string(),
                path: line[3..].to_string(),
            });
        }
        let worktree = self.worktree_context(root).await?;
        let head = self.head_commit(root).await?;
        let (unreachable, unreachable_truncated) = self.unreachable_commits(root).await?;
        let stashes = self.stash_count(root).await?;
        Ok(RepoStatus {
            branch,
            worktree,
            head,
            ahead,
            behind,
            changed,
            truncated,
            unreachable,
            unreachable_truncated,
            stashes,
        })
    }

    async fn diff(
        &self,
        root: &Path,
        staged: bool,
        paths: &[String],
    ) -> Result<RepoDiff, RepoError> {
        // `--no-ext-diff --no-textconv`: repository config can bind diff
        // drivers to arbitrary commands, and a read-only "show me the
        // hunks" must never execute repository-controlled code.
        let mut base = vec!["diff", "--no-color", "--no-ext-diff", "--no-textconv"];
        if staged {
            base.push("--staged");
        }
        let path_refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        let mut stat_args = base.clone();
        stat_args.push("--numstat");
        if !path_refs.is_empty() {
            stat_args.push("--");
            stat_args.extend(&path_refs);
        }
        let numstat = Self::git_ok_stdout(root, "repo_diff", &stat_args).await?;
        // `added\tremoved\tpath` per line; binary changes report `-` in the
        // count columns, which parses to `None`.
        let files = numstat
            .lines()
            .filter_map(|line| {
                let mut cols = line.splitn(3, '\t');
                let added = cols.next()?;
                let removed = cols.next()?;
                let path = cols.next()?;
                Some(DiffFileStat {
                    path: path.to_string(),
                    added: added.parse().ok(),
                    removed: removed.parse().ok(),
                })
            })
            .collect();
        let mut patch_args = base;
        if !path_refs.is_empty() {
            patch_args.push("--");
            patch_args.extend(&path_refs);
        }
        // stdout alone, like the numstat read above (#801's last missed call
        // site): the patch is rendered verbatim AND its emptiness is what
        // makes a clean tree say "no changes" — stderr chatter (advice
        // hints, `GIT_TRACE` lines) merged into it polluted the shown diff
        // and turned a clean tree into "0 files changed" plus noise.
        let patch = Self::git_ok_stdout(root, "repo_diff", &patch_args).await?;
        Ok(RepoDiff { files, patch })
    }

    async fn current_branch(&self, root: &Path) -> Result<Option<String>, RepoError> {
        let out =
            Self::git_ok_stdout(root, "status", &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
        let name = out.trim();
        Ok((name != "HEAD").then(|| name.to_string()))
    }

    async fn default_branch(&self, root: &Path) -> Result<Option<String>, RepoError> {
        // Cheap local resolution first (set by clone), then ask the remote.
        if let (0, out) = Self::git_stdout(
            root,
            "push",
            &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        )
        .await?
            && let Some(name) = out.trim().strip_prefix("origin/")
        {
            return Ok(Some(name.to_string()));
        }
        if let (0, out) =
            Self::git_stdout(root, "push", &["ls-remote", "--symref", "origin", "HEAD"]).await?
        {
            for line in out.lines() {
                if let Some(rest) = line.strip_prefix("ref: refs/heads/")
                    && let Some(name) = rest.split_whitespace().next()
                {
                    return Ok(Some(name.to_string()));
                }
            }
        }
        Ok(None)
    }

    async fn commit_paths(
        &self,
        root: &Path,
        message: &str,
        paths: &[String],
    ) -> Result<String, RepoError> {
        let path_refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        let mut add = vec!["add", "--"];
        add.extend(&path_refs);
        Self::git_ok(root, "repo_commit", &add).await?;
        // Pathspec-limited commit: exactly the named paths land, whatever
        // else may sit in the index stays uncommitted.
        let mut commit = Self::commit_identity(root).await;
        commit.extend(["commit", "-m", message, "--"]);
        commit.extend(&path_refs);
        Self::git_ok(root, "repo_commit", &commit).await?;
        let summary = Self::git_ok(root, "repo_commit", &["log", "-1", "--oneline"]).await?;
        Ok(format!("committed: {}", summary.trim()))
    }

    async fn push_branch(&self, root: &Path, branch: &str) -> Result<String, RepoError> {
        // Fully-qualified refspec: what gets pushed can only ever be a
        // branch head, and never with force.
        let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
        let out = Self::git_ok(
            root,
            "repo_push",
            &["push", "--set-upstream", "origin", &refspec],
        )
        .await?;
        Ok(format!("pushed `{branch}`\n{}", out.trim()))
    }

    async fn pull_ff_only(&self, root: &Path) -> Result<String, RepoError> {
        match Self::git(root, "repo_pull", &["pull", "--ff-only"]).await? {
            (0, out) => Ok(out.trim().to_string()),
            (_, out) if out.contains("fast-forward") || out.contains("divergent") => {
                Err(RepoError::Diverged(out.trim().to_string()))
            }
            (code, out) => Err(RepoError::Failed {
                action: "repo_pull",
                detail: format!("exit {code}: {}", out.trim()),
            }),
        }
    }

    async fn restore_paths(&self, root: &Path, paths: &[String]) -> Result<String, RepoError> {
        let mut args = vec!["checkout", "HEAD", "--"];
        args.extend(paths.iter().map(String::as_str));
        Self::git_ok(root, "repo_rollback", &args).await?;
        Ok(format!(
            "restored {} path(s) to the last committed state",
            paths.len()
        ))
    }
}

/// Extract a non-empty `paths: string[]` or return the structural refusal
/// shared by `repo_commit` and `repo_rollback` (see the module doc).
fn required_paths(input: &Value, tool: &str, verb: &str) -> Result<Vec<String>, ToolOutput> {
    let paths: Vec<String> = input
        .get("paths")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if paths.is_empty() {
        return Err(ToolOutput::Error {
            message: format!(
                "{tool} {verb} exactly the paths you name — `paths` must be a non-empty \
                 list; a whole-tree operation must be spelled out path by path, never \
                 implied by an empty list"
            ),
        });
    }
    // A leading `:` is git PATHSPEC MAGIC, not a path: `:/` means "the whole
    // repository root" and `:(exclude)…` inverts the selection. Either one
    // turns a named-paths call into the stage-everything mode this surface
    // structurally does not have — `git add -- :/` commits the entire tree,
    // and `repo_rollback` would discard every local modification in it.
    // Refuse before the argv is built; `--` only stops OPTION parsing.
    if let Some(magic) = paths.iter().find(|p| p.starts_with(':')) {
        return Err(ToolOutput::Error {
            message: format!(
                "`{magic}` is pathspec magic, not a path — {tool} {verb} the literal paths \
                 you name, and a magic pathspec can silently widen that to the whole tree"
            ),
        });
    }
    Ok(paths)
}

/// A branch name safe to hand to the backend as its own argv entry: no
/// leading `-` (option injection), no whitespace or control characters, and
/// none of the characters `git check-ref-format` forbids in a ref. The last
/// group matters beyond hygiene: `:` is the refspec separator, so a name
/// carrying one would splice extra source/destination pairs into the
/// `refs/heads/{branch}:refs/heads/{branch}` push refspec.
fn valid_branch_name(branch: &str) -> bool {
    const BANNED: [char; 7] = [':', '~', '^', '?', '*', '[', '\\'];
    !branch.is_empty()
        && !branch.starts_with('-')
        && !branch.contains("..")
        && !branch.contains(|c: char| c.is_whitespace() || c.is_control() || BANNED.contains(&c))
}

/// `repo_status` — read-only: names and states, never content (that is
/// `repo_diff`).
pub struct RepoStatusTool(pub Arc<dyn RepoBackend>);

#[async_trait]
impl Tool for RepoStatusTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "repo_status".into(),
            description: "Repository status: current branch, commits ahead/behind upstream, \
                          and the changed files as structured rows."
                .into(),
            input_schema: serde_json::json!({ "type": "object", "properties": {} }),
            read_only: true,
            speculation_safe: true,
        }
    }

    async fn execute(&self, _input: &Value, root: &Path) -> ToolOutput {
        match self.0.status(root).await {
            Ok(status) => match serde_json::to_string_pretty(&status) {
                Ok(json) => ToolOutput::Ok { content: json },
                Err(e) => ToolOutput::Error {
                    message: format!("cannot render repository status: {e}"),
                },
            },
            Err(e) => ToolOutput::Error {
                message: e.to_string(),
            },
        }
    }

    // The repo family's git argv joins the `command.started` fence (#804):
    // each tool reports the primary line of the sequence [`GitCli`] — the
    // only shipped backend — runs, joined argv-style like `start_process`.
    async fn command_for_gate(&self, _input: &Value, _root: &Path) -> Option<String> {
        Some("git status --porcelain".into())
    }
}

/// Render a [`RepoDiff`] as a compact per-file summary followed by the raw
/// patch hunks, capping the summary rows at `MAX_CHANGED_ROWS` and the
/// patch at `MAX_PATCH_BYTES` — always with loud elision (module doc).
fn render_diff(diff: &RepoDiff, staged: bool) -> String {
    let scope = if staged { "staged" } else { "unstaged" };
    let patch = diff.patch.trim_end();
    if diff.files.is_empty() && patch.is_empty() {
        return format!(
            "no {scope} changes (files never staged or committed have no diff — \
             repo_status lists those)"
        );
    }
    let (added, removed) = diff.files.iter().fold((0u64, 0u64), |(a, r), f| {
        (a + f.added.unwrap_or(0), r + f.removed.unwrap_or(0))
    });
    let mut out = format!(
        "{} {scope} file(s) changed: +{added} -{removed}\n",
        diff.files.len()
    );
    for f in diff.files.iter().take(MAX_CHANGED_ROWS) {
        match (f.added, f.removed) {
            (Some(a), Some(r)) => out.push_str(&format!("  {} +{a} -{r}\n", f.path)),
            _ => out.push_str(&format!("  {} (binary)\n", f.path)),
        }
    }
    if diff.files.len() > MAX_CHANGED_ROWS {
        out.push_str(&format!(
            "  [… {} more file(s) not listed …]\n",
            diff.files.len() - MAX_CHANGED_ROWS
        ));
    }
    out.push('\n');
    if patch.len() > MAX_PATCH_BYTES {
        // Snap to a char boundary, then back to a whole line: a half-shown
        // hunk line would read as a content change that isn't there.
        let mut cap = MAX_PATCH_BYTES;
        while !patch.is_char_boundary(cap) {
            cap -= 1;
        }
        let cut = patch[..cap].rfind('\n').map_or(cap, |i| i + 1);
        out.push_str(&patch[..cut]);
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!(
            "[… diff truncated: {cut} of {} bytes shown — re-run with `paths` scoped \
             to specific files for their full hunks …]",
            patch.len()
        ));
    } else {
        out.push_str(patch);
    }
    out
}

/// `repo_diff` — the other read-only repository tool: the actual pending
/// hunks, so a pre-commit self-review is grounded in the real patch rather
/// than the agent's narration of it.
pub struct RepoDiffTool(pub Arc<dyn RepoBackend>);

#[async_trait]
impl Tool for RepoDiffTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "repo_diff".into(),
            description: "The pending changes as patch hunks (with file/line context) plus a \
                          per-file +added/-removed summary — review what you ACTUALLY changed \
                          before repo_commit or verify_done. Unstaged changes by default; \
                          `staged: true` shows changes already staged for commit instead. \
                          Files never staged or committed have no diff — repo_status lists \
                          those."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "staged": { "type": "boolean", "description": "Show changes already staged for commit instead of unstaged ones (default false)" },
                    "paths": { "type": "array", "items": { "type": "string" }, "description": "Optional workspace-relative paths to scope the diff to" }
                }
            }),
            read_only: true,
            speculation_safe: true,
        }
    }

    async fn execute(&self, input: &Value, root: &Path) -> ToolOutput {
        let staged = input
            .get("staged")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let paths: Vec<String> = input
            .get("paths")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        match self.0.diff(root, staged, &paths).await {
            Ok(diff) => ToolOutput::Ok {
                content: render_diff(&diff, staged),
            },
            Err(e) => ToolOutput::Error {
                message: e.to_string(),
            },
        }
    }

    // See [`RepoStatusTool::command_for_gate`].
    async fn command_for_gate(&self, input: &Value, _root: &Path) -> Option<String> {
        let mut line = String::from("git diff --no-color --no-ext-diff --no-textconv");
        if input
            .get("staged")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            line.push_str(" --staged");
        }
        if let Some(paths) = input.get("paths").and_then(|v| v.as_array()) {
            let named: Vec<&str> = paths.iter().filter_map(|v| v.as_str()).collect();
            if !named.is_empty() {
                line.push_str(" -- ");
                line.push_str(&named.join(" "));
            }
        }
        Some(line)
    }
}

/// `repo_commit` — pathspec-explicit commit; see the module doc.
pub struct RepoCommit(pub Arc<dyn RepoBackend>);

#[async_trait]
impl Tool for RepoCommit {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "repo_commit".into(),
            description: "Commit EXACTLY the named paths: stages them, then commits only \
                          them. paths is required and must be non-empty — there is no \
                          stage-everything mode."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "message": { "type": "string", "description": "Commit message" },
                    "paths": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "The exact workspace-relative paths to commit" }
                },
                "required": ["message", "paths"]
            }),
            read_only: false,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, root: &Path) -> ToolOutput {
        let Some(message) = input
            .get("message")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|m| !m.is_empty())
        else {
            return ToolOutput::Error {
                message: "missing required field `message`".into(),
            };
        };
        let paths = match required_paths(input, "repo_commit", "commits") {
            Ok(paths) => paths,
            Err(refusal) => return refusal,
        };
        match self.0.commit_paths(root, message, &paths).await {
            Ok(summary) => ToolOutput::Ok { content: summary },
            Err(e) => ToolOutput::Error {
                message: e.to_string(),
            },
        }
    }

    // See [`RepoStatusTool::command_for_gate`]. Both mutating steps of the
    // sequence are shown; a structurally invalid input resolves `None` and
    // returns the tool's own refusal at execute time, ungated.
    async fn command_for_gate(&self, input: &Value, _root: &Path) -> Option<String> {
        let message = input.get("message").and_then(|v| v.as_str())?;
        let paths = required_paths(input, "repo_commit", "commits").ok()?;
        let joined = paths.join(" ");
        Some(format!(
            "git add -- {joined} && git commit -m {message} -- {joined}"
        ))
    }
}

/// `repo_push` — never the default branch, never forced; see the module doc.
pub struct RepoPush(pub Arc<dyn RepoBackend>);

#[async_trait]
impl Tool for RepoPush {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "repo_push".into(),
            description: "Push the current (or named) branch to the primary remote. \
                          STRUCTURALLY refuses the repository's default branch — publish \
                          work on a feature branch. Force-push does not exist here."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "branch": { "type": "string", "description": "Branch to push (default: the current branch)" }
                }
            }),
            read_only: false,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, root: &Path) -> ToolOutput {
        let named = input.get("branch").and_then(|v| v.as_str());
        if let Some(branch) = named
            && !valid_branch_name(branch)
        {
            return ToolOutput::Error {
                message: format!(
                    "`{branch}` is not a valid branch name (must not start with `-` or \
                     contain whitespace)"
                ),
            };
        }
        let branch = match named {
            Some(b) => b.to_string(),
            None => match self.0.current_branch(root).await {
                Ok(Some(b)) => b,
                Ok(None) => {
                    return ToolOutput::Error {
                        message: "the checkout is detached (no current branch) — pass \
                                  `branch` explicitly"
                            .into(),
                    };
                }
                Err(e) => {
                    return ToolOutput::Error {
                        message: e.to_string(),
                    };
                }
            },
        };
        // The structural rule: resolve the default branch and refuse it.
        // An UNRESOLVABLE default fails closed — pushing blind could be
        // pushing the default.
        match self.0.default_branch(root).await {
            Ok(Some(default)) if default == branch => {
                return ToolOutput::Error {
                    message: format!(
                        "repo_push refuses to push `{branch}`: it is the repository's \
                         default branch. Publish work on a feature branch instead — this \
                         rule is structural and has no override"
                    ),
                };
            }
            Ok(Some(_)) => {}
            Ok(None) => {
                return ToolOutput::Error {
                    message: "cannot determine the repository's default branch (remote \
                              HEAD) — refusing to push rather than risk pushing it"
                        .into(),
                };
            }
            Err(e) => {
                return ToolOutput::Error {
                    message: e.to_string(),
                };
            }
        }
        match self.0.push_branch(root, &branch).await {
            Ok(out) => ToolOutput::Ok { content: out },
            Err(e) => ToolOutput::Error {
                message: e.to_string(),
            },
        }
    }

    // See [`RepoStatusTool::command_for_gate`]. Without a named branch the
    // refspec resolves from the checkout mid-execute; the gate then sees the
    // line minus the refspec — still enough for a policy denying pushes.
    async fn command_for_gate(&self, input: &Value, _root: &Path) -> Option<String> {
        Some(match input.get("branch").and_then(|v| v.as_str()) {
            Some(branch) => {
                format!("git push --set-upstream origin refs/heads/{branch}:refs/heads/{branch}")
            }
            None => "git push --set-upstream origin".to_string(),
        })
    }
}

/// `repo_pull` — fast-forward only; see the module doc.
pub struct RepoPull(pub Arc<dyn RepoBackend>);

#[async_trait]
impl Tool for RepoPull {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "repo_pull".into(),
            description: "Update the current branch from its upstream, fast-forward only. \
                          Diverged histories are a typed error — this tool never merges or \
                          rewrites."
                .into(),
            input_schema: serde_json::json!({ "type": "object", "properties": {} }),
            read_only: false,
            speculation_safe: false,
        }
    }

    async fn execute(&self, _input: &Value, root: &Path) -> ToolOutput {
        match self.0.pull_ff_only(root).await {
            Ok(out) => ToolOutput::Ok { content: out },
            Err(e) => ToolOutput::Error {
                message: e.to_string(),
            },
        }
    }

    // See [`RepoStatusTool::command_for_gate`].
    async fn command_for_gate(&self, _input: &Value, _root: &Path) -> Option<String> {
        Some("git pull --ff-only".into())
    }
}

/// `repo_rollback` — restore named paths to HEAD; see the module doc.
pub struct RepoRollback(pub Arc<dyn RepoBackend>);

#[async_trait]
impl Tool for RepoRollback {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "repo_rollback".into(),
            description: "Restore EXACTLY the named paths to their last committed state, \
                          discarding local modifications to them. paths is required and \
                          must be non-empty. History itself is never rewritten."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "paths": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "The exact workspace-relative paths to restore" }
                },
                "required": ["paths"]
            }),
            read_only: false,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, root: &Path) -> ToolOutput {
        let paths = match required_paths(input, "repo_rollback", "restores") {
            Ok(paths) => paths,
            Err(refusal) => return refusal,
        };
        match self.0.restore_paths(root, &paths).await {
            Ok(out) => ToolOutput::Ok { content: out },
            Err(e) => ToolOutput::Error {
                message: e.to_string(),
            },
        }
    }

    // See [`RepoStatusTool::command_for_gate`].
    async fn command_for_gate(&self, input: &Value, _root: &Path) -> Option<String> {
        let paths = required_paths(input, "repo_rollback", "restores").ok()?;
        Some(format!("git checkout HEAD -- {}", paths.join(" ")))
    }
}

#[cfg(test)]
mod tests;
