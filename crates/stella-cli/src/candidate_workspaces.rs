// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The isolation substrate a `candidate_fanout` runs on: one git worktree per
//! candidate, one writing worker turn inside each, and one all-or-nothing
//! adoption that lands a winner on the real tree (#3892).
//!
//! #3844 landed the socket half of best-of-N — the wire types, the
//! `[loop] max_fanout_width` ceiling, and
//! [`CandidateFanouts`](stella_runtime::wrapper::CandidateFanouts) over a
//! [`CandidateWorkspaces`] port — and **nothing implemented the port**, so
//! every shipped host answered both capabilities
//! [`Unavailable`](stella_plugin::HostCallRefusal::Unavailable) and
//! `plugins/stella-candidates` (`doc:pipeline-as-plugins` §3/§7 item 4) still
//! could not be written as best-of-N. This is that implementation.
//!
//! # What is reused, and the one thing that is not
//!
//! Almost everything here is somebody else's machinery held together:
//!
//! | Concern | Whose |
//! |---|---|
//! | `worktree add` / `remove --force`, the slug that survives `git check-ref-format` | [`WorktreeManager`] |
//! | the child's provider, seat map, spend pool, ledger, orphan cascade, settle-on-the-thread | [`SessionSubAgents`] |
//! | the write-directory grant | [`crate::write_dirs::registry_rooted_at`] |
//! | the operator's tool switches and the authorization gate | [`crate::agent::tool_stack`] |
//!
//! The one genuinely new decision is **which registry a candidate's turn runs
//! against**, and it is the reason this could not be a field on
//! [`SubAgentSpec`]. A fan-out runs N candidates *concurrently* in N disjoint
//! trees, and [`ToolRegistry`](stella_tools::ToolRegistry)'s `root` is what
//! every path fence resolves against — so there is no single root a shared
//! registry could hold that
//! would be true for all of them. A registry **per candidate**, rooted once at
//! construction and never moved, is the only shape that is. Everything else
//! about the turn stays the session's, which is what keeps one pool and one
//! ledger over one session's money
//! ([`SessionSubAgents::dispatch_in_workspace`]).
//!
//! # Adoption applies a patch; it does not merge, commit, or rebase
//!
//! A candidate's output is **uncommitted working-tree bytes**, so adoption is
//! `git diff` in the candidate and `git apply` on the real tree. Three
//! properties follow, and each is the reason this shape was chosen over
//! `cherry-pick`/`merge`:
//!
//! - **It is indistinguishable from the worker having done it.** The user's
//!   own turn leaves working-tree changes; so does an adopted candidate. A
//!   plugin cannot use best-of-N to put a commit in someone's history that
//!   they did not write.
//! - **It is all-or-nothing without needing to be made so.** `git apply` is
//!   already atomic — it validates every hunk before writing any, and this
//!   deliberately passes no `--reject`, so a patch that does not fit changes
//!   nothing at all. The two refusals it produces are named on
//!   [`SessionCandidateWorkspaces::adopt`].
//! - **It applies at the repository's top level, never at the session's own
//!   directory.** A patch's paths are top-relative, and `git apply` run from a
//!   subdirectory filters them to what lies below the current directory —
//!   finding nothing, applying nothing, and exiting **0**. See [`Layout`],
//!   which exists for that one silent failure.
//! - **It refuses rather than resolves.** There is no `--3way`, no merge
//!   driver and no conflict-marker path: a candidate that no longer applies is
//!   reported to the plugin as an `err`, and
//!   [`CandidateFanouts::adopt`](stella_runtime::wrapper::CandidateFanouts)
//!   guarantees the losing candidates are still on disk when it is. Resolving
//!   a conflict is a judgement, and a host that made it silently would be
//!   editing the user's tree on its own authority.
//!
//! # It needs a git repository, and says so rather than pretending
//!
//! A workspace that is not a repository, or one with no commit for `HEAD` to
//! name, fails at `git worktree add` — which surfaces as
//! [`CandidateFanoutError::NotCreated`], then as a `Failed` answer carrying
//! git's own words. That is the honest outcome for *this* substrate: what it
//! promotes is a patch, and a patch is a statement about a commit.
//!
//! Copying the tree instead is the other answer, and it is a second substrate
//! rather than a fallback here ([`copy_tree`], #1383). It is not
//! interchangeable: a copy carries no `.gitignore` semantics, so its adoption
//! carries a candidate's `target/` too — right where the tree is a disposable
//! container whose ignored state the task's own tests execute, wrong where
//! the tree is
//! somebody's working copy. Which one a workspace gets is
//! [`CandidateIsolation`](crate::settings::CandidateIsolation), an operator's
//! setting that defaults to this one and is never inferred.
//!
//! # What is left behind, and by what
//!
//! Every candidate this substrate mints is registered in the plane's live
//! table, and a wrapped run sweeps that table when it ends
//! (`crate::wrapper_plugin::run_wrapped`). That table is process memory, so a
//! process **killed** mid-fan-out used to take the only name its checkouts had
//! with it — `stella fleet gc` cannot reclaim them either, because this
//! substrate moves out of the `.stella/worktrees/` + `fleet/` namespace on
//! purpose ([`WorktreeManager::with_worktrees_root`]) rather than borrow a
//! sweeper that would then also delete checkouts it did not create. Each
//! candidate now writes a [`record`] beside its checkout, and a later run in
//! the same workspace names what a dead owner left rather than deleting it
//! (#2813).
//!
//! [`CandidateWorkspaces`]: stella_runtime::wrapper::CandidateWorkspaces
//! [`SubAgentSpec`]: stella_core::subagent::SubAgentSpec
//! [`SessionSubAgents`]: crate::subagent::SessionSubAgents
//! [`SessionSubAgents::dispatch_in_workspace`]: crate::subagent::SessionSubAgents::dispatch_in_workspace

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::config::Config;
use crate::subagent::SessionSubAgents;
use async_trait::async_trait;
use stella_core::subagent::{SubAgentOutcome, SubAgentSpec};
use stella_fleet::git::{GitCli, SystemGitCli, Worktree, WorktreeManager};
use stella_protocol::candidate::CandidateHandle;
use stella_runtime::wrapper::{
    CandidateFanoutError, CandidateReport, CandidateWork, CandidateWorkspace, CandidateWorkspaces,
};

/// Where this substrate puts its checkouts, under the workspace root.
///
/// Under `private/` for two reasons at once: the generated `.stella/.gitignore`
/// already excludes that whole directory (`stella_store::private`), so a
/// candidate's checkout never shows up as untracked noise in the user's own
/// `git status`; and it is owner-only state, which is what a tree holding a
/// half-finished writing turn is.
const CANDIDATES_DIR: &str = ".stella/private/candidates";

/// The branch namespace a candidate's scratch checkout is cut onto.
///
/// Not `fleet/`, and that is load-bearing rather than cosmetic:
/// `stella fleet gc` reclaims by namespace, so a candidate branch spelled
/// `fleet/…` is one `gc` believes it owns and may delete out from under a
/// live fan-out.
const CANDIDATE_BRANCH_PREFIX: &str = "candidate/";

/// How many characters of a candidate turn's answer cross back to the plugin.
///
/// The same 8k [`SubAgentSpec`]'s default carries, and unchanged on purpose:
/// what a plugin scores is the *evidence* — the report, whether the turn
/// finished, how big the diff is — while the work itself stays in the
/// workspace, addressed by handle. A larger report would buy a plugin nothing
/// it cannot already get by reading the candidate's root.
const CANDIDATE_REPORT_CHARS: usize = 8_000;

/// Where the session sits inside its repository.
///
/// Both halves exist because `Config::workspace_root` is the process's
/// **working directory**, which need not be the repository's top level — and
/// a candidate worktree is always a full checkout, so the two disagree the
/// moment a user starts `stella` in a subdirectory.
///
/// Getting this wrong is not a visible failure, which is why it is a named
/// type rather than an inline `rev-parse`. `git apply` run from a
/// subdirectory **exits 0 and applies nothing**: it filters the patch to
/// paths under the current directory, and a top-relative patch has none. An
/// adoption would have reported success for a landing that never happened —
/// the exact outcome [`SessionCandidateWorkspaces::adopt`]'s contract calls
/// the worst available.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Layout {
    /// The repository's top level (`git rev-parse --show-toplevel`) — where
    /// `git apply` must run, because a patch's paths are top-relative.
    top: PathBuf,
    /// The session's own offset below it (`git rev-parse --show-prefix`),
    /// empty when the session *is* the top level.
    ///
    /// A candidate's root is its worktree top plus this, so the candidate's
    /// turn sees the same tree shape the session does and is fenced to the
    /// same subtree. Without it a candidate would hold a wider write scope
    /// than the session that asked for it.
    prefix: PathBuf,
}

/// One candidate's checkout, plus the two facts about the repository that are
/// only knowable at the moment it was cut.
///
/// Both are baselines rather than descriptions of what anything *should* be:
/// they say what this candidate was handed, so adoption can ask what the
/// candidate itself changed. See [`ref_guard`] and [`modes`].
#[derive(Debug)]
struct Minted {
    worktree: Worktree,
    /// Every shared ref of the repository and its value. Compared at adoption
    /// against the namespace as it stands then.
    refs: ref_guard::RefSnapshot,
    /// The permission bits `git worktree add` could not reproduce, stamped
    /// onto the checkout so the candidate sees the tree the user has.
    modes: modes::ModeRecord,
}

/// One session's candidate substrate: the repository it snapshots, the
/// dispatcher it borrows turns from, and the handles it has minted.
pub(crate) struct SessionCandidateWorkspaces {
    /// The session's own config — read for the write-directory grant, the
    /// operator's tool policy, and the engine's step cap. Cloned rather than
    /// borrowed because this outlives any one turn.
    cfg: Config,
    /// The installed plugin's manifest name: the [`Principal`] every candidate
    /// turn's tool calls authorize as.
    ///
    /// [`Principal`]: stella_core::ports::Principal
    plugin: String,
    /// The session's one dispatcher. See the module docs on why a second would
    /// be a second pool over one session's money.
    sub_agents: Arc<SessionSubAgents>,
    worktrees: WorktreeManager<SystemGitCli>,
    /// The session's own root — where `git` is invoked from, and the
    /// directory a candidate is made to *look* like.
    ///
    /// Not necessarily the repository's top level: `Config::workspace_root` is
    /// the process's working directory, and a user may start `stella` inside a
    /// subdirectory of their repository. [`Layout`] is what reconciles the two.
    repo_root: PathBuf,
    /// The repository's top level and this session's offset inside it,
    /// resolved from `git` the first time a candidate is minted and cached.
    ///
    /// Lazy because it costs a subprocess and the overwhelming majority of
    /// sessions install this substrate and never fan out — `session_host`
    /// builds one on every `--pipeline` run.
    layout: tokio::sync::OnceCell<Layout>,
    /// Handle → the checkout it addresses and what the repository looked like
    /// when it was minted. The substrate's own bookkeeping, deliberately not
    /// on [`CandidateWorkspace`]: a branch name is something a plugin cannot
    /// act on, so it has no business holding it.
    minted: Mutex<HashMap<CandidateHandle, Minted>>,
    /// Monotonic, so two fan-outs in one run cannot mint the same handle.
    next: AtomicU32,
}

impl std::fmt::Debug for SessionCandidateWorkspaces {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionCandidateWorkspaces")
            .field("plugin", &self.plugin)
            .field("repo_root", &self.repo_root)
            .field("minted", &self.minted.lock().map(|m| m.len()).unwrap_or(0))
            .finish_non_exhaustive()
    }
}

impl SessionCandidateWorkspaces {
    /// Build the substrate for one installed plugin over this session's tree.
    ///
    /// The run scope is the plugin name plus this process's id, and it is
    /// hashed into every checkout's directory and branch name
    /// ([`WorktreeManager::with_run_scope`]). Without it two `stella run`
    /// processes fanning out in the same workspace would derive the same slugs
    /// and the second `git worktree add` would fail on the first's directory —
    /// the collision that scope exists to prevent for fleets, at a door where
    /// two concurrent runs are ordinary rather than exotic.
    pub(crate) fn new(cfg: &Config, plugin: &str, sub_agents: Arc<SessionSubAgents>) -> Self {
        let repo_root = cfg.workspace_root.clone();
        let worktrees = WorktreeManager::new(SystemGitCli, repo_root.clone())
            .with_worktrees_root(repo_root.join(CANDIDATES_DIR))
            .with_branch_prefix(CANDIDATE_BRANCH_PREFIX)
            .with_run_scope(format!("{plugin}\u{1f}{}", std::process::id()));
        Self {
            cfg: cfg.clone(),
            plugin: plugin.to_string(),
            sub_agents,
            worktrees,
            repo_root,
            layout: tokio::sync::OnceCell::new(),
            minted: Mutex::new(HashMap::new()),
            next: AtomicU32::new(0),
        }
    }

    /// Resolve — once — where this session sits inside its repository.
    ///
    /// # Errors
    ///
    /// [`CandidateFanoutError::NotCreated`] when `git` cannot answer, which is
    /// the same failure a caller would get one line later from
    /// `git worktree add`: a workspace that is not a repository cannot serve
    /// best-of-N, and saying so here keeps the reason attached to the first
    /// thing that needed it.
    async fn layout(&self) -> Result<&Layout, CandidateFanoutError> {
        self.layout
            .get_or_try_init(|| async {
                let top = self
                    .git(&self.repo_root, &["rev-parse", "--show-toplevel"])
                    .await
                    .map_err(|reason| CandidateFanoutError::NotCreated { reason })?;
                let prefix = self
                    .git(&self.repo_root, &["rev-parse", "--show-prefix"])
                    .await
                    .map_err(|reason| CandidateFanoutError::NotCreated { reason })?;
                Ok(Layout {
                    top: PathBuf::from(top.trim()),
                    // Trimmed of its trailing separator: `--show-prefix` emits
                    // `sub/`, and joining that is fine while comparing it is
                    // not. Empty at the top level, where `join` is a no-op.
                    prefix: PathBuf::from(prefix.trim().trim_end_matches('/')),
                })
            })
            .await
    }

    /// The worktree top a handle addresses — where every `git` command about a
    /// candidate must run.
    ///
    /// Not [`CandidateWorkspace::root`], which is the candidate's *session-
    /// shaped* root and may sit below this one. A `git diff` run there would
    /// produce a patch scoped to the subdirectory rather than to everything
    /// the candidate changed.
    fn checkout(&self, handle: &CandidateHandle) -> Result<PathBuf, CandidateFanoutError> {
        self.minted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(handle)
            .map(|minted| minted.worktree.path.clone())
            .ok_or_else(|| CandidateFanoutError::NotAdopted {
                handle: handle.clone(),
                reason: "this host minted no workspace under that handle".to_string(),
            })
    }

    /// The checkout, the ref namespace and the modes recorded when `handle`
    /// was minted — everything adoption compares the candidate against.
    fn baseline(
        &self,
        handle: &CandidateHandle,
    ) -> Result<(Worktree, ref_guard::RefSnapshot, modes::ModeRecord), CandidateFanoutError> {
        self.minted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(handle)
            .map(|minted| {
                (
                    minted.worktree.clone(),
                    minted.refs.clone(),
                    minted.modes.clone(),
                )
            })
            .ok_or_else(|| CandidateFanoutError::NotAdopted {
                handle: handle.clone(),
                reason: "this host minted no workspace under that handle".to_string(),
            })
    }

    /// Run one git command in `dir`, turning a non-zero exit into prose that
    /// names the command and repeats git's own stderr.
    ///
    /// Every git invocation here goes through [`SystemGitCli`] rather than
    /// `std::process::Command`, which is not a style choice: that adapter
    /// scrubs the `GIT_DIR`/`GIT_WORK_TREE` family and the
    /// config-injection family from the child's environment, so a candidate
    /// created from inside a git hook cannot be silently aimed at the outer
    /// repository.
    async fn git(&self, dir: &Path, args: &[&str]) -> Result<String, String> {
        let out = SystemGitCli
            .run(dir, args)
            .await
            .map_err(|error| error.to_string())?;
        if out.success {
            Ok(out.stdout)
        } else {
            Err(format!(
                "`git {}` failed: {}",
                args.join(" "),
                out.stderr.trim()
            ))
        }
    }

    /// Every ref of the repository at `dir` and its object id, one per line.
    async fn for_each_ref(&self, dir: &Path) -> Result<ref_guard::RefSnapshot, String> {
        self.git(dir, &["for-each-ref", "--format=%(objectname) %(refname)"])
            .await
            .map(|output| ref_guard::RefSnapshot::parse(&output))
    }

    /// Is `maybe_ancestor` reachable from `descendant`?
    ///
    /// `merge-base --is-ancestor` answers by exit status, so this cannot go
    /// through [`Self::git`], which reads a non-zero exit as a failure. A git
    /// invocation that failed for any other reason answers `false`, which is
    /// the direction that never invents an escape ([`ref_guard`]'s contract).
    async fn is_ancestor(&self, dir: &Path, maybe_ancestor: &str, descendant: &str) -> bool {
        SystemGitCli
            .run(
                dir,
                &["merge-base", "--is-ancestor", maybe_ancestor, descendant],
            )
            .await
            .is_ok_and(|out| out.success)
    }

    /// The subject line of the commit `oid` names, empty when git could not
    /// say.
    ///
    /// Read for one question only — which worktree took a stash — and empty
    /// rather than an error for [`ref_guard`]'s reason: a fact this host could
    /// not read must not become an accusation.
    async fn commit_subject(&self, dir: &Path, oid: &str) -> String {
        self.git(dir, &["log", "-1", "--no-color", "--format=%s", oid])
            .await
            .map(|subject| subject.trim().to_string())
            .unwrap_or_default()
    }

    /// The messages of `checkout`'s **own** HEAD reflog, newest first.
    ///
    /// Per-worktree state (`.git/worktrees/<name>/logs/HEAD`), which is what
    /// makes it evidence: only the process sitting in that checkout writes it,
    /// so a ref it names is one this candidate moved onto.
    async fn head_reflog(&self, checkout: &Path) -> String {
        self.git(checkout, &["reflog", "show", "HEAD", "--format=%gs"])
            .await
            .unwrap_or_default()
    }

    /// The tracked paths of the tree at `dir`, repository-relative.
    async fn tracked_paths(&self, dir: &Path) -> Vec<String> {
        self.git(dir, &["ls-files", "-z"])
            .await
            .as_deref()
            .map(modes::nul_separated)
            .unwrap_or_default()
    }

    /// Stage every unignored change as intent-to-add, so `git diff` sees files
    /// the candidate *created* and not only the ones it edited.
    ///
    /// Without this a candidate that wrote a new file would produce an empty
    /// patch and adopt as a silent no-op — the worst available outcome, since
    /// the plugin was told the adoption succeeded. `--intent-to-add` records
    /// the path in the index without its content, which is exactly what makes
    /// the file show up in `git diff` as an addition.
    ///
    /// Ignored files stay out, which is correct and deliberate: a candidate's
    /// `target/` is not part of its answer.
    async fn stage_intent(&self, root: &Path) -> Result<(), String> {
        self.git(root, &["add", "--intent-to-add", "--all"])
            .await
            .map(|_| ())
    }

    /// What earlier runs in this workspace left behind, one line each, naming
    /// the command that reclaims it (#2813).
    ///
    /// Read at the start of a wrapped run, before this run writes any record
    /// of its own — so what it sees is only ever somebody else's, and a
    /// concurrent fan-out's records are skipped because their owner answers
    /// the liveness probe.
    ///
    /// Reporting rather than reclaiming is the whole point: a leftover
    /// checkout is either a crash's residue or a live sibling run's, and
    /// deleting the second on this host's own authority would destroy a
    /// fan-out somebody is watching. See [`record`].
    pub(crate) fn orphaned_candidates(&self) -> Vec<String> {
        record::report(&self.repo_root.join(CANDIDATES_DIR))
    }

    /// The candidate's whole change against the tree it was cut from.
    ///
    /// `--binary` so a candidate that wrote an image is adoptable rather than
    /// silently truncated to "Binary files differ"; `--no-ext-diff` so a
    /// user's configured external differ cannot substitute prose for a patch.
    async fn patch(&self, root: &Path) -> Result<String, String> {
        self.git(root, &["diff", "--binary", "--no-ext-diff", "HEAD"])
            .await
    }

    /// Write `checkout`'s uncommitted work out beside it, and say where.
    ///
    /// `Ok(None)` for a candidate that changed nothing — there is no file to
    /// point a user at, and writing an empty one would put a path in a report
    /// that is worth nothing to open.
    ///
    /// The file is named for the checkout's **slug**, not for the handle a
    /// plugin spells. `candidate-0` is unique within one run and repeats in
    /// the next, so a second aborted run would overwrite the first's rescued
    /// work — which is the exact loss this exists to prevent. The slug carries
    /// this run's scope hash ([`WorktreeManager::with_run_scope`]).
    async fn write_patch(&self, checkout: &Path) -> Result<Option<PathBuf>, String> {
        self.stage_intent(checkout).await?;
        let patch = self.patch(checkout).await?;
        if patch.trim().is_empty() {
            return Ok(None);
        }
        let slug = checkout
            .file_name()
            .and_then(|slug| slug.to_str())
            .ok_or_else(|| "a candidate checkout with no final path component".to_string())?;
        let path = self
            .repo_root
            .join(CANDIDATES_DIR)
            .join(format!("{slug}.patch"));
        std::fs::write(&path, patch.as_bytes())
            .map_err(|error| format!("{}: {error}", path.display()))?;
        Ok(Some(path))
    }

    /// Write out every candidate still live, before anything removes it, and
    /// say where each landed (#2651).
    ///
    /// Called when a run ends **abnormally** — the turn budget stopped it, the
    /// dispatch failed, a turn aborted — which is precisely the ending where
    /// no plugin ever got to score what these candidates did. The end-of-run
    /// sweep then deletes the checkouts and their branches unconditionally, so
    /// without this a candidate that solved the task is discarded with no
    /// residue at all.
    ///
    /// The patch is written, never applied. Which candidate deserved to land
    /// is the plugin's judgement, and a host that made it on an abort would be
    /// adopting an unscored answer onto the user's tree on its own authority
    /// — the same line [`Self::adopt`] refuses to cross when a patch does not
    /// fit.
    pub(crate) async fn preserve_unscored(&self) -> Vec<String> {
        let live: Vec<(CandidateHandle, PathBuf)> = self
            .minted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(handle, minted)| (handle.clone(), minted.worktree.path.clone()))
            .collect();
        let mut lines = Vec::new();
        for (handle, checkout) in live {
            match self.write_patch(&checkout).await {
                Ok(Some(path)) => lines.push(format!(
                    "the run ended before anything scored candidate {handle}; its work is \
                     kept at {} and applies with `git apply --binary`",
                    path.display()
                )),
                Ok(None) => {}
                // Reported rather than raised, like the sweep beside it: the
                // run is over, and a patch that would not write is a fact the
                // user needs, not the run's exit status.
                Err(reason) => lines.push(format!(
                    "candidate {handle}'s work could not be written out before its \
                     workspace was removed: {reason}"
                )),
            }
        }
        lines
    }
}

#[async_trait]
impl CandidateWorkspaces for SessionCandidateWorkspaces {
    async fn create(&self, label: &str) -> Result<CandidateWorkspace, CandidateFanoutError> {
        let layout = self.layout().await?.clone();
        let prefix = layout.prefix.clone();
        let ordinal = self.next.fetch_add(1, Ordering::Relaxed);
        // The handle a plugin will spell back, and deliberately not the
        // directory name: the slug carries a hash of a run scope this host
        // chose, which is noise to a plugin and a fingerprint of the host's
        // process to anyone reading a plugin's logs. `candidate-N` is unique
        // within the run, which is the only property adoption needs — and it
        // is not `host-tree`, which is what keeps
        // `stella_plugin::HOST_TREE_HANDLE` naming no entry in this table.
        let handle = CandidateHandle::new(format!("candidate-{ordinal}"));
        let worktree = self
            .worktrees
            .create(&format!("{label}-{ordinal}"), "HEAD")
            .await
            .map_err(|error| CandidateFanoutError::NotCreated {
                reason: error.to_string(),
            })?;
        // The worktree top plus this session's own offset inside the
        // repository, so a candidate is shaped like the workspace that asked
        // for it — a session started in `sub/` gets candidates rooted at
        // `<checkout>/sub`, not at a whole repository it never had.
        //
        // Canonical, because the plugin is handed this path to read and test
        // in, and a `..`-bearing or symlinked spelling is one a plugin's own
        // path handling may resolve differently than this host would.
        let root = worktree.path.join(&prefix);
        let root = root.canonicalize().unwrap_or(root);
        let root = root.to_string_lossy().into_owned();

        // The two baselines, read now because neither can be reconstructed
        // later: what the shared ref namespace held before this candidate
        // could touch it, and the permission bits `git worktree add` did not
        // reproduce in the checkout it just made.
        //
        // Both are best-effort. A repository whose refs this host could not
        // read audits nothing at adoption rather than refusing a candidate on
        // the reading, and a mode it could not stamp is one the patch will
        // carry as far as git can.
        let refs = self.for_each_ref(&layout.top).await.unwrap_or_default();
        let modes = modes::stamp_unrepresentable(
            &layout.top,
            &worktree.path,
            &self.tracked_paths(&layout.top).await,
        );
        // The durable half of the live table above: what this checkout is, on
        // disk, so a process that never reaches `remove` leaves something a
        // later run can name (#2813). Best-effort like the two baselines —
        // a candidate that could not be described is still one that should
        // run.
        let _ = record::CandidateRecord::of(handle.as_str(), &worktree, &layout.top).write();

        self.minted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                handle.clone(),
                Minted {
                    worktree,
                    refs,
                    modes,
                },
            );
        Ok(CandidateWorkspace { handle, root })
    }

    async fn work(
        &self,
        workspace: &CandidateWorkspace,
        work: CandidateWork,
    ) -> Result<CandidateReport, CandidateFanoutError> {
        let outcome = dispatch_candidate_turn(
            &self.cfg,
            &self.plugin,
            &self.sub_agents,
            PathBuf::from(&workspace.root),
            work,
        )
        .await?;

        // Measured after the turn and from the tree, never from what the turn
        // said it did: the two disagree exactly when it matters. At the
        // checkout's **top**, so the count covers everything the candidate
        // changed rather than only what it changed below the session's own
        // offset.
        let checkout = self.checkout(&workspace.handle)?;
        let (files_changed, lines_changed) = match self.stage_intent(&checkout).await {
            Ok(()) => self
                .git(&checkout, &["diff", "--numstat", "--no-ext-diff", "HEAD"])
                .await
                .map_or((0, 0), |numstat| numstat_totals(&numstat)),
            // A diff this host could not read is reported as no diff rather
            // than as a failed turn: the candidate ran, its report is real,
            // and a plugin scoring on size alone gets a zero it can see rather
            // than an error that hides an answer it could have used.
            Err(_) => (0, 0),
        };

        Ok(CandidateReport {
            report: outcome.summary().to_string(),
            completed: matches!(outcome, SubAgentOutcome::Completed(_)),
            cost_usd: outcome.cost_usd(),
            files_changed,
            lines_changed,
        })
    }

    /// Apply this candidate's changes to the real tree, all-or-nothing.
    ///
    /// # The two refusals
    ///
    /// - **The patch does not apply.** The real tree moved under the
    ///   candidate — its `HEAD` advanced, or it carries uncommitted edits to
    ///   the same lines — so `git apply` rejects the whole patch and writes
    ///   nothing. Reported with git's own words, because "which hunk" is the
    ///   only useful thing to say and git says it better.
    /// - **The candidate's own diff could not be read.** A `git diff` that
    ///   fails leaves nothing to apply, and adopting an empty patch on that
    ///   basis would report success for a landing that did not happen.
    ///
    /// A candidate that changed nothing adopts successfully and applies
    /// nothing, which is not the same event and is deliberately not an error:
    /// "the best of these three was the one that correctly decided the code
    /// was already right" is a real answer, and refusing it would push a
    /// plugin toward selecting a candidate that wrote something for the sake
    /// of writing.
    async fn adopt(&self, workspace: &CandidateWorkspace) -> Result<(), CandidateFanoutError> {
        let fail = |reason: String| CandidateFanoutError::NotAdopted {
            handle: workspace.handle.clone(),
            reason,
        };
        let (worktree, refs_at_create, modes_at_create) = self.baseline(&workspace.handle)?;
        let checkout = worktree.path.clone();
        let top = self.layout().await?.top.clone();

        // Before anything is written: a candidate that reached outside its own
        // tree into the shared ref namespace does not get to land, however
        // good its patch is.
        let escaped = ref_guard::audit(self, &top, &refs_at_create, &worktree).await;
        if !escaped.is_empty() {
            return Err(fail(ref_guard::refusal(&escaped)));
        }

        // Re-staged rather than trusted from `work`: a plugin may have run its
        // own tests in this workspace through `run_test`, and anything that
        // produced an unignored file since then is part of what it chose.
        self.stage_intent(&checkout).await.map_err(&fail)?;
        let patch = self.patch(&checkout).await.map_err(&fail)?;
        // The candidate's own view of the tree, and which of its directories
        // the apply is about to create — both read before the patch lands,
        // because afterwards neither question can be asked.
        let tracked = self.tracked_paths(&checkout).await;
        let created_dirs = modes::directories_adoption_will_create(&top, &tracked);
        if patch.trim().is_empty() {
            // A pure `chmod` is not a diff — git records `100644` and `100755`
            // and nothing else — so an empty patch still has modes to deliver.
            modes::replay_changed(&checkout, &top, &tracked, &modes_at_create, &[]);
            return Ok(());
        }

        // A file rather than stdin because `GitCli` is an args-only port, and
        // widening it to carry stdin for one caller would put a `&[u8]` in
        // every fleet git call's signature. `NamedTempFile` removes itself on
        // drop, including on the error path below.
        let file = tempfile::NamedTempFile::new()
            .map_err(|error| fail(format!("no temporary file for the patch: {error}")))?;
        std::fs::write(file.path(), patch.as_bytes())
            .map_err(|error| fail(format!("the patch could not be written: {error}")))?;
        let patch_arg = file
            .path()
            .to_str()
            .ok_or_else(|| fail("the patch path is not valid UTF-8".to_string()))?;

        // No `--3way` and no `--reject`: git validates every hunk before it
        // writes any, so a patch that does not fit changes nothing at all, and
        // resolving a conflict is a judgement this host must not make on the
        // user's tree. See the module docs.
        // At the repository's **top level**, never at `repo_root`. A patch's
        // paths are top-relative, and `git apply` run from a subdirectory
        // filters the patch to paths under the current directory — which for a
        // top-relative patch is none of them. It then exits **0**, so a
        // session started in a subdirectory would have been told its adoption
        // landed while nothing was written at all.
        self.git(
            &top,
            &["apply", "--binary", "--whitespace=nowarn", patch_arg],
        )
        .await
        .map_err(&fail)?;

        // The half the patch could not carry, delivered from the candidate's
        // own tree now that the bytes are down. Best-effort by contract: the
        // apply already happened and cannot be un-happened, so a `chmod` that
        // fails is a partially faithful delivery, never an absent one.
        modes::replay_changed(&checkout, &top, &tracked, &modes_at_create, &created_dirs);
        Ok(())
    }

    async fn remove(&self, workspace: &CandidateWorkspace) -> Result<(), CandidateFanoutError> {
        let Some(minted) = self
            .minted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&workspace.handle)
        else {
            // Already gone, and that is success rather than an error: removal
            // is what the end-of-run sweep does to everything, and a handle
            // adoption already took must not make the sweep report a failure
            // no one can act on.
            return Ok(());
        };
        let discarded = self.worktrees.discard(&minted.worktree).await;
        if discarded.is_ok() {
            // Only once the checkout is really gone. A record dropped ahead of
            // a failed discard would leave a leak nothing on disk names, which
            // is the exact state the record exists to prevent.
            record::forget(&minted.worktree.path);
        }
        discarded.map_err(|error| CandidateFanoutError::NotRemoved {
            handle: workspace.handle.clone(),
            reason: error.to_string(),
        })
    }
}

/// Run one candidate's writing turn, rooted at `root`.
///
/// Shared by both substrates because the turn is the one part of a candidate
/// that does not depend on how its tree was isolated: the same registry rooted
/// at the same kind of directory, the same seat, the same session pool and
/// ledger. Only what surrounds it — snapshot, measure, promote — differs (see
/// [`copy_tree`]).
///
/// # Errors
///
/// [`CandidateFanoutError::NotRun`] for a turn that never reached its first
/// model call and cost exactly zero. That is the one outcome that is genuinely
/// an error: every other shape is a candidate that ran and did not finish,
/// which a plugin can score. It cannot score one that never existed.
async fn dispatch_candidate_turn(
    cfg: &Config,
    plugin: &str,
    sub_agents: &SessionSubAgents,
    root: PathBuf,
    work: CandidateWork,
) -> Result<SubAgentOutcome, CandidateFanoutError> {
    // Rooted at the candidate, and carrying the operator's `--allow-dir`
    // grant — which is the whole reason this goes through `write_dirs`
    // rather than `ToolRegistry::new`. See that module's own witness.
    let registry = Arc::new(crate::write_dirs::registry_rooted_at(cfg, root));
    let spec = SubAgentSpec {
        role: work.seat,
        // The plugin's own word for the role, passed through untouched —
        // the routing key the user's seat assignment is looked up by.
        // `role` above is the receipt label. Nothing below may branch on
        // this string's contents.
        seat: Some(work.role.clone()),
        turn_instance: work.turn_instance,
        budget_usd: work.budget_usd,
        // The whole capability. Every other dispatched child in this crate
        // is read-only; a candidate that could not write would have
        // nothing to adopt.
        write_access: true,
        // The session's own cap, not `SubAgentSpec`'s 16: a candidate is
        // not a searcher summarizing for a parent, it is the work, and
        // capping it below the turn it stands in for would make best-of-N
        // structurally worse than the single turn it replaces.
        max_steps: crate::agent::engine_config_for(cfg).max_steps,
        max_report_chars: CANDIDATE_REPORT_CHARS,
        ..SubAgentSpec::read_only(work.agent_id, work.instruction)
    };

    let outcome = sub_agents
        .dispatch_in_workspace(
            spec,
            registry,
            crate::agent::session_tool_policy(cfg),
            stella_core::ports::Principal::Plugin(plugin.to_string()),
        )
        .await;
    if let SubAgentOutcome::Refused { reason } = &outcome {
        return Err(CandidateFanoutError::NotRun {
            reason: reason.clone(),
        });
    }
    Ok(outcome)
}

/// Sum `git diff --numstat` into `(files, lines added + removed)`.
///
/// A binary file reports `-\t-\t<path>` rather than counts; it is counted as a
/// changed *file* with no lines, which is the honest reading — its size in
/// lines is not a number that exists. Pure, so the parsing is testable without
/// a repository.
fn numstat_totals(numstat: &str) -> (u32, u32) {
    let mut files = 0_u32;
    let mut lines = 0_u32;
    for row in numstat.lines().filter(|row| !row.trim().is_empty()) {
        let mut cols = row.split('\t');
        let added = cols.next().unwrap_or_default().trim();
        let removed = cols.next().unwrap_or_default().trim();
        // A row with no path is not a numstat row — never seen from git, and
        // counting it would inflate the one number a plugin scores on.
        if cols.next().is_none() {
            continue;
        }
        files = files.saturating_add(1);
        for count in [added, removed] {
            lines = lines.saturating_add(count.parse::<u32>().unwrap_or(0));
        }
    }
    (files, lines)
}

mod copy_tree;
mod modes;
mod record;
mod ref_guard;
mod substrate;

pub(crate) use substrate::CandidateSubstrate;

#[cfg(test)]
mod tests;
