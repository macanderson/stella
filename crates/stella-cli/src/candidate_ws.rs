//! Best-of-N candidate isolation over real git — the production
//! [`CandidateWorkspacePort`]. Each candidate gets a detached shadow
//! worktree (temp-dir path, pid+counter name, forced removal + prune on
//! every exit) snapshotting the user's CURRENT tree state:
//!
//! 1. `git worktree add --detach <tmp> <HEAD>` — never a branch, never a
//!    checkout of the user's tree;
//! 2. the uncommitted tracked delta overlaid via `git diff --binary HEAD`
//!    applied inside the shadow;
//! 3. untracked, non-ignored files copied in byte-for-byte (the pipeline uses
//!    complete content hashes for witness integrity);
//! 4. one baseline commit sealed in the shadow's PRIVATE index (detached
//!    HEAD — no ref of the user's repo moves).
//!
//! The user's working tree and index are NEVER touched, and the stash is
//! shared machine state other sessions rely on, so it is banned outright —
//! no `git stash push` appears anywhere in this module. Commands that run
//! against the real repo are read-only with exactly two exceptions: the final
//! winner-adoption `git apply` (worktree only, no `--index`), and the ref
//! repair described below, which writes only a ref a candidate itself moved
//! and only back to the value recorded before that candidate existed.
//! Final verification first seals
//! the shadow in a private commit. Adoption requires the shadow to remain
//! byte-identical to that seal *except* where the verification run itself is
//! the observed author of the difference ([`oracle_writes`], #2877), diffs the
//! immutable baseline against that exact verified commit, applies the result
//! in one `git apply`, and then replays the permission bits that patch could
//! not encode ([`modes`], #2935). The
//! worker cannot race verification with adoption, and a real tree that no
//! longer matches what the candidate started from fails the whole adoption
//! loudly instead of half-applying — naming which of the two divergences it
//! was, a user edit or a candidate that wrote outside its snapshot (see
//! [`adopt::PathDivergence`]).
//!
//! # What isolation does NOT cover: the ref namespace (#2541)
//!
//! A shadow worktree isolates the working **tree**, not the **ref**
//! namespace: it shares one `.git` with the graded tree, so `refs/heads/*`,
//! `refs/tags/*` and `refs/stash` are the same objects the user's checkout
//! reads. Git's own interlock refuses to check out a branch another worktree
//! holds, but a worker can defeat it (`git checkout
//! --ignore-other-worktrees`) and then move the graded tree's branch out from
//! under its index. Three guards close the observed hole:
//! [`ref_escape::detach_head`] re-detaches a `HEAD` the worker attached, so a
//! machinery commit — witness scaffolding included — can never land in a
//! history the user keeps; [`GitCandidateWorkspace::escaped_refs_inner`]
//! refuses to seal a candidate whose work has reached a shared ref; and
//! [`GitCandidateWorkspace::restore`] **puts that ref back** before the
//! refusal is raised (#2641) — a refusal that only named the recovery command
//! left the graded tree as corrupted as it found it, which cost a measured
//! run its whole score. The detach runs first for that reason: a restore
//! issued from the main worktree would otherwise drag an attached candidate
//! `HEAD` back with the branch. The repair runs on the teardown path too
//! ([`GitCandidateWorkspace::restore_escaped_refs_inner`]), because a run
//! that dies on the turn budget never reaches a seal. See [`ref_escape`] for
//! the compare-and-swap discipline, the `refs/stash` special case, and what a
//! restore can unwind. The structural fix — a substrate that cannot reach
//! those refs at all — is #1383.
//!
//! # What a candidate's engine can reach
//!
//! Candidates drive the built-in [`stella_tools::ToolRegistry`] PLUS the session's custom
//! script tools, both rooted at the snapshot (with the session's workspace
//! rules applied) — a [`CustomToolSet`] owning the registry by `Arc`.
//! Custom tools spawn subprocesses with the snapshot as cwd
//! ([`CustomToolSet::new_owned`]), so their writes land in the isolated
//! shadow, never the real tree.
//!
//! # MCP: a phased tool surface (issue #248)
//!
//! A cost/benefit review found the naive "full per-candidate MCP" build
//! low-ROI: most configured servers are filesystem-rooted (redundant with
//! the snapshot-rooted built-ins above) or side-effecting (harmful to run N
//! times), and only read-only, cwd-INDEPENDENT servers (docs/web search,
//! code graph, GitHub/Linear/DB reads) have real pickup in candidates. So
//! this is phased rather than all-or-nothing:
//!
//! - **Phase 1 (built here).** An explicit per-server `candidate_safe = true`
//!   opt-in in `.stella/mcp.toml` is the trustworthy gate — a server's own
//!   `read_only_hint` is UNTRUSTED and can't distinguish "reads an external
//!   system" from "reads the local tree" (`stella_mcp::config`'s doc). Only
//!   allowlisted servers' tools reach a candidate
//!   ([`stella_mcp::McpToolSet::for_candidates`],
//!   [`GitCandidateWorkspaces::with_candidate_mcp`]) — sharing the SAME
//!   already-connected clients, no new subprocess. A filesystem-rooted or
//!   otherwise unlisted server stays withheld for exactly the correctness
//!   reason the naive build would have hit: it was spawned against the REAL
//!   workspace, so even a read-only call on it would return the real tree's
//!   content — a candidate that edited a file then read it back would see
//!   the UNEDITED bytes, mixing a stale view into its snapshot-rooted work.
//!   Additionally, an **orchestrator pre-fetch** ([`McpPrefetchAdapter`],
//!   [`stella_mcp::McpToolSet::prefetch_candidate_context`]) calls every
//!   allowlisted server's zero-argument tools ONCE, before the fan-out, and
//!   folds the result into every candidate's shared starting messages — the
//!   common "every candidate needs the same schema/ticket context" case, at
//!   the cost of one round trip instead of N.
//! - **Phase 2 (explicitly NOT built here).** Per-candidate stdio sessions
//!   with `cwd` = the snapshot, which would let a filesystem-rooted server
//!   join safely too. Deferred until measured need — Phase 1's allowlist
//!   already covers the servers that showed real pickup.
//!
//! Candidates run non-interactively regardless of phase: a fan-out of N
//! candidates has no single owner for a prompt, so nothing in a candidate's
//! stack can ask the human anything.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use async_trait::async_trait;
use std::sync::Arc;
use stella_fleet::git::{GitCli, SystemGitCli};
use stella_pipeline::ports::{
    AdoptedChange, CandidateWorkspace, CandidateWorkspacePort, DiagnosticRunner, McpPrefetchPort,
    RepoStatusPort, TestRunner, WorkspaceError,
};

use stella_tools::custom::{CustomTool, CustomToolSet};

use crate::agent::{
    GitDiagnosticRunner, GitRepoStatus, TypedTestRunner, fs_artifact_identity, fs_fingerprint,
};

mod adopt;
mod escape;
mod modes;
mod oracle_writes;
mod ref_escape;
mod snapshot_gaps;
mod task_events;
mod witness_tools;
use oracle_writes::{ObservedTestRunner, OracleWrites};
use witness_tools::WitnessToolExecutor;

/// The commit identity for snapshot plumbing commits (which exist only
/// inside the shadow and are discarded with it) — the user's repo may have
/// no identity configured (CI), and their real identity must never be
/// implied on machinery commits.
///
/// The email is the machinery identity the pipeline's witness prompt tells
/// authors to exclude from history walks
/// ([`stella_pipeline::witness::MACHINERY_COMMIT_EMAIL`]). A `const` array
/// cannot embed the other crate's constant, so a parity test pins the two
/// spellings together instead.
const SNAPSHOT_IDENT: [&str; 4] = [
    "-c",
    "user.name=stella-pipeline",
    "-c",
    "user.email=pipeline@stella.invalid",
];

/// The subject line of every seal commit written into a shadow's private
/// history. The leading `stella: ` prefix is part of the machinery
/// vocabulary the pipeline's witness prompt tells authors to exclude from
/// history walks, so the spelling stays fixed.
const CANDIDATE_SEAL_SUBJECT: &str = "stella: candidate verified snapshot";

/// Shadow names carry pid + a process-wide counter: concurrent candidates
/// must never collide on a path.
static SHADOW_SEQ: AtomicU64 = AtomicU64::new(0);

/// One git command against `repo` through the fleet's [`SystemGitCli`]
/// (explicit `-C` targeting, hook-exported `GIT_*` env scrubbed, no
/// terminal prompt). Flattens both spawn failures and non-zero exits into a
/// reason string; callers wrap it into the right [`WorkspaceError`] variant.
async fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    match SystemGitCli.run(repo, args).await {
        Ok(out) if out.success => Ok(out.stdout),
        Ok(out) => Err(format!(
            "`git {}` failed: {}",
            args.join(" "),
            out.stderr.trim()
        )),
        Err(e) => Err(e.to_string()),
    }
}

/// `git <args>` with stdout captured as raw bytes and written to `out`.
/// Patches must never round-trip through [`SystemGitCli`]'s lossy UTF-8
/// stdout — a non-UTF-8 source file's diff would be corrupted and then
/// mis-applied. (Raw bytes in memory, not an fd redirect: tokio's
/// `Command::output` forcibly re-pipes any configured stdout.) Same
/// repo-targeting discipline as the port: explicit `-C`, scrubbed `GIT_*`
/// env, no prompt.
async fn git_stdout_to_file(repo: &Path, args: &[&str], out: &Path) -> Result<(), String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C").arg(repo).args(args);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    for var in stella_tools::exec::GIT_REPO_ENV_VARS {
        cmd.env_remove(var);
    }
    stella_tools::subprocess_env::scrub_sensitive_env(&mut cmd);
    cmd.kill_on_drop(true);
    let output = cmd
        .output()
        .await
        .map_err(|e| format!("failed to spawn `git {}`: {e}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "`git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    tokio::fs::write(out, &output.stdout)
        .await
        .map_err(|e| format!("could not write `{}`: {e}", out.display()))
}

/// Copy `src` to `dst` (creating parents), preserving the modification time.
/// Copy one untracked overlay file while preserving its filesystem metadata.
async fn copy_preserving_mtime(src: &Path, dst: &Path) -> std::io::Result<()> {
    if let Some(parent) = dst.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::copy(src, dst).await?;
    let meta = tokio::fs::metadata(src).await?;
    if let Ok(modified) = meta.modified() {
        // `std::fs::copy` copies the source's permission bits, so a read-only
        // source (e.g. a `chmod 444` untracked artifact) yields a read-only
        // `dst`. Opening that `.write(true)` to stamp the mtime would fail
        // with EACCES for the owner, aborting the whole candidate snapshot.
        // Temporarily clear the read-only bit, set the time, then restore the
        // original permissions so the snapshot's mode still mirrors the real
        // tree.
        let perms = meta.permissions();
        let restore = if perms.readonly() {
            let mut writable = perms.clone();
            // Grant only the owner-write bit so the mtime stamp below can open
            // `dst` for writing; `set_readonly(false)` would also add the
            // group/other-write bits (`0o222`, world-writable on Unix). The
            // original mode is restored right after, so the writable window is
            // momentary and confined to this private per-candidate shadow tree.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                writable.set_mode(writable.mode() | 0o200);
            }
            #[cfg(not(unix))]
            {
                #[allow(clippy::permissions_set_readonly_false)]
                writable.set_readonly(false);
            }
            std::fs::set_permissions(dst, writable)?;
            Some(perms)
        } else {
            None
        };
        let res = std::fs::OpenOptions::new()
            .write(true)
            .open(dst)
            .and_then(|dst_file| {
                dst_file.set_times(std::fs::FileTimes::new().set_modified(modified))
            });
        if let Some(perms) = restore {
            // Restore the original read-only bits regardless of the result.
            let _ = std::fs::set_permissions(dst, perms);
        }
        res?;
    }
    Ok(())
}

/// Best-effort removal of a candidate shadow worktree — the registration and
/// the directory both, then a prune for any stale registration. Called on
/// every path, success or failure.
async fn cleanup(toplevel: &Path, dir: &Path) {
    if let Some(d) = dir.to_str() {
        let _ = SystemGitCli
            .run(toplevel, &["worktree", "remove", "--force", d])
            .await;
    }
    let _ = tokio::fs::remove_dir_all(dir).await;
    let _ = SystemGitCli.run(toplevel, &["worktree", "prune"]).await;
}

/// The production [`CandidateWorkspacePort`]: real-git candidate snapshots
/// rooted at the session's workspace.
pub(crate) struct GitCandidateWorkspaces {
    /// The session workspace root — possibly a subdirectory of the repo
    /// toplevel (the shadow mirrors the TOPLEVEL, and the candidate's ports
    /// re-descend into the matching subdirectory).
    root: PathBuf,
    /// The operator's tool switches, applied over each candidate's own tool
    /// stack — best-of-N must not be a way around a
    /// `"tools": {"task": "off"}`.
    policy: stella_tools::policy::ToolPolicy,
    /// The session's custom script tools, re-rooted at each candidate's
    /// snapshot (their subprocesses spawn with the snapshot as cwd, so they
    /// stay isolated). Cloned per candidate; the manifests are identical to
    /// the session's because the snapshot is a copy of the real tree.
    custom_tools: Vec<CustomTool>,
    /// Immutable rules resolved from the real session workspace before any
    /// shadow exists. Candidate discovery must never depend on which ignored
    /// or store-backed policy files happened to enter a git snapshot.
    active_rules: crate::rules::ResolvedRules,
    /// The session's connected MCP servers, shared read-only into every
    /// candidate (issue #248 Phase 1) — `Arc`, not a borrow, so this struct
    /// and its candidates stay `'static` (see the module doc's "MCP: a
    /// phased tool surface" section). `None` when the session has no MCP
    /// servers connected, or hasn't opted any into `with_candidate_mcp`.
    candidate_mcp: Option<Arc<stella_mcp::McpToolSet>>,
    /// The turn's event sink, used to bridge each candidate registry's
    /// policy plane into the journal (#441). `None` leaves a candidate's
    /// rule denials evaluating and blocking exactly as before — they simply
    /// never land as typed `PolicyDecision` events.
    events: Option<stella_core::EventSender>,
    /// The session's sub-agent runner, shared into every candidate registry.
    /// `None` leaves the `task` tool reporting sub-agents as unavailable —
    /// see [`GitCandidateWorkspaces::with_sub_agents`].
    sub_agents: Option<Arc<dyn stella_core::subagent::SubAgentDispatcher>>,
}

impl GitCandidateWorkspaces {
    pub(crate) fn new(
        root: PathBuf,
        policy: stella_tools::policy::ToolPolicy,
        custom_tools: Vec<CustomTool>,
        active_rules: crate::rules::ResolvedRules,
    ) -> Self {
        Self {
            root,
            policy,
            custom_tools,
            active_rules,
            candidate_mcp: None,
            events: None,
            sub_agents: None,
        }
    }

    /// Let every candidate delegate through the session's sub-agent runner.
    ///
    /// Without this a candidate's `task` tool answers "sub-agents are
    /// unavailable in this session — do the work directly with your own
    /// tools", advice a candidate cannot take: the built-in surface has no
    /// command-executing tool, so a worker there could move the task board
    /// and nothing else. Since an authored witness forces a candidate even at
    /// N=1, that state was reached by ordinary single-candidate runs (#3318).
    ///
    /// Sharing the session's runner rather than building a candidate-rooted
    /// one is deliberate and costs the candidate nothing: children run behind
    /// `ReadOnlyTools`, so a child reads to understand the codebase and the
    /// snapshot it would otherwise read is a copy of the same tree.
    pub(crate) fn with_sub_agents(
        mut self,
        dispatcher: Arc<dyn stella_core::subagent::SubAgentDispatcher>,
    ) -> Self {
        self.sub_agents = Some(dispatcher);
        self
    }

    /// Journal the policy decisions made inside every candidate this port
    /// creates. Best-of-N candidates are the primary *actual* users of the
    /// rule-guard bus, so without this the typed `PolicyDecision` record of
    /// a denial exists for the session but not for the candidates where
    /// most denials actually happen.
    pub(crate) fn with_events(mut self, events: stella_core::EventSender) -> Self {
        self.events = Some(events);
        self
    }

    /// Share the session's connected MCP servers into every candidate this
    /// port creates, filtered to `candidate_safe`-flagged servers only (issue
    /// #248 Phase 1) — see [`stella_mcp::McpToolSet::for_candidates`].
    pub(crate) fn with_candidate_mcp(mut self, mcp: Arc<stella_mcp::McpToolSet>) -> Self {
        self.candidate_mcp = Some(mcp);
        self
    }

    /// The concrete create (the trait impl boxes its result): snapshot the
    /// current tree state into a fresh detached shadow worktree.
    async fn create_workspace(&self) -> Result<GitCandidateWorkspace, WorkspaceError> {
        let snap = |reason: String| WorkspaceError::Snapshot { reason };
        let canon_root = self
            .root
            .canonicalize()
            .map_err(|e| snap(format!("could not canonicalize the workspace root: {e}")))?;
        let toplevel = git(&canon_root, &["rev-parse", "--show-toplevel"])
            .await
            .map_err(snap)?;
        // Kept before canonicalization: it is the spelling the operator and
        // the task statement use.
        let toplevel_as_git_spells_it = PathBuf::from(toplevel.trim());
        let toplevel = toplevel_as_git_spells_it
            .canonicalize()
            .unwrap_or_else(|_| toplevel_as_git_spells_it.clone());
        let head = git(&toplevel, &["rev-parse", "HEAD"]).await.map_err(|e| {
            snap(format!(
                "candidate isolation requires a git repository with at least one commit: {e}"
            ))
        })?;
        let head = head.trim().to_string();
        let root_rel = canon_root
            .strip_prefix(&toplevel)
            .unwrap_or(Path::new(""))
            .to_path_buf();

        let dir = std::env::temp_dir().join(format!(
            "stella_candidate_{}_{}",
            std::process::id(),
            SHADOW_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let dir_str = dir
            .to_str()
            .ok_or_else(|| snap("temp dir path is not valid UTF-8".to_string()))?;
        // The ref namespace this candidate is about to share with the graded
        // tree, recorded before it can move (#2541) — see [`ref_escape`].
        let refs_at_create = ref_escape::shared_refs(&toplevel).await;
        git(&toplevel, &["worktree", "add", "--detach", dir_str, &head])
            .await
            .map_err(snap)?;

        // From here on every failure must tear the shadow down.
        match populate_snapshot(&toplevel, &dir, &root_rel).await {
            Ok((overlay_untracked, baseline)) => {
                let ws_root = dir.join(&root_rel);
                let registry = stella_tools::ToolRegistry::new(ws_root.clone());
                // Same governance as the session registry: workspace rules
                // travel with the tree — best-of-N must not be a way around
                // them. Applied while `registry` is still a plain
                // `ToolRegistry`, before it moves into the `Arc`.
                crate::rules::attach_rule_guards(&registry, &self.active_rules);
                // `attach_rule_guards` is what gives this registry a live
                // HookBus, so the bridge must follow it — and both must
                // happen while `registry` is still a concrete `ToolRegistry`,
                // before the `Arc<dyn ToolExecutor>` coercion below erases it.
                if let Some(events) = &self.events {
                    registry.bridge_policy_plane(events.clone());
                }
                // Same late-attachment window as the two above, and for the
                // same reason: while `registry` is still a concrete
                // `ToolRegistry`. Without it the candidate's `task` tool is
                // dead schema (#3318).
                if let Some(dispatcher) = &self.sub_agents {
                    registry.attach_sub_agent_dispatcher(dispatcher.clone());
                }
                // The candidate's tool surface: the snapshot-rooted registry
                // plus the session's custom script tools, owned as one value
                // (the workspace outlives every borrow). Custom tools re-root
                // to `ws_root`, so their subprocesses run in the shadow.
                let board = registry.task_board();
                let registry: Arc<dyn stella_core::ToolExecutor> = Arc::new(registry);
                // `canon_root`, not `self.root`: the frame screen compares
                // spellings, and the goal's paths are phrased against the
                // real, resolved location of the workspace (#2130). The
                // author's reads honor the operator's tool policy exactly
                // like the worker's do (#1784) — the executor carries the
                // policy itself.
                let witness_tools =
                    WitnessToolExecutor::new(ws_root.clone(), &canon_root, self.policy.clone());
                let native =
                    CustomToolSet::new_owned(registry, self.custom_tools.clone(), ws_root.clone());
                // MCP: layer the candidate_safe-filtered session view on top
                // when the session shared one (issue #248 Phase 1) — the
                // native surface above stays the fallthrough for every
                // non-`mcp__` name either way.
                let tools: Arc<dyn stella_core::ToolExecutor> = match &self.candidate_mcp {
                    Some(mcp) => Arc::new(mcp.for_candidates(Arc::new(native))),
                    None => Arc::new(native),
                };
                // Policy then gate, over registry + customs + candidate MCP
                // alike (#3283) — best-of-N must not be a way around either.
                // The principal is the pipeline's worker role: a candidate's
                // tools act for the execute stage, never the human directly.
                let tools: Box<dyn stella_core::ToolExecutor> =
                    Box::new(crate::agent::tool_stack::policy_stack_owned(
                        tools,
                        self.policy.clone(),
                        stella_core::ports::Principal::Role("worker".into()),
                    ));
                // Outside even the policy wrap, like the deck's session tap
                // (#1719) — see `task_events`'s module doc.
                let (tools, task_board) = task_events::tap(tools, board, self.events.clone());
                let omitted = snapshot_gaps::omitted_ignored_paths(&toplevel, &root_rel).await;
                // Shared between the workspace's drift check and the test
                // runner that fills it — the runner is the only thing that
                // knows *when* the verification run happened (#2877).
                let oracle_writes = Arc::new(OracleWrites::default());
                Ok(GitCandidateWorkspace {
                    toplevel,
                    dir: dir.clone(),
                    root: ws_root.display().to_string(),
                    baseline,
                    refs_at_create,
                    sealed: Mutex::new(None),
                    delivered: Mutex::new(None),
                    tools,
                    witness_tools,
                    diagnostics: GitDiagnosticRunner::new(ws_root.clone()),
                    tests: ObservedTestRunner::new(
                        TypedTestRunner {
                            root: ws_root.clone(),
                        },
                        dir.clone(),
                        oracle_writes.clone(),
                    ),
                    oracle_writes,
                    repo_status: SnapshotRepoStatus {
                        inner: GitRepoStatus {
                            root: ws_root.clone(),
                        },
                        ws_root,
                        overlay: overlay_untracked,
                    },
                    task_board,
                    sole_candidate: AtomicBool::new(false),
                    omitted,
                })
            }
            Err(reason) => {
                cleanup(&toplevel, &dir).await;
                Err(snap(reason))
            }
        }
    }
}

#[async_trait]
impl CandidateWorkspacePort for GitCandidateWorkspaces {
    async fn create(&self) -> Result<Box<dyn CandidateWorkspace>, WorkspaceError> {
        Ok(Box::new(self.create_workspace().await?))
    }
}

/// Adapts the session's shared MCP view into the pipeline's
/// [`McpPrefetchPort`] (issue #248 Phase 1): the orchestrator calls this
/// ONCE before a best-of-N fan-out, never per candidate — see
/// [`stella_mcp::McpToolSet::prefetch_candidate_context`] for what actually
/// gets called and why it is safe to call blind. Goal-blind by the port's
/// contract (#1779): the sweep only ever calls zero-argument tools with
/// `{}`, so there is no goal to deliver anywhere.
pub(crate) struct McpPrefetchAdapter(Arc<stella_mcp::McpToolSet>);

impl McpPrefetchAdapter {
    pub(crate) fn new(mcp: Arc<stella_mcp::McpToolSet>) -> Self {
        Self(mcp)
    }
}

#[async_trait]
impl McpPrefetchPort for McpPrefetchAdapter {
    async fn prefetch(&self) -> Option<String> {
        self.0.prefetch_candidate_context().await
    }
}

/// Overlay the user's uncommitted state onto the fresh shadow at `dir` and
/// seal it as the baseline commit. Returns the ws-root-relative paths of the
/// untracked files that were copied in (the [`SnapshotRepoStatus`] overlay
/// set).
async fn populate_snapshot(
    toplevel: &Path,
    dir: &Path,
    root_rel: &Path,
) -> Result<(Vec<String>, String), String> {
    // 1. The uncommitted tracked delta — staged and unstaged both (`git diff
    //    HEAD` sees the union), `--binary` so non-text files survive.
    let patch_file = std::env::temp_dir().join(format!(
        "stella_candidate_overlay_{}_{}.patch",
        std::process::id(),
        SHADOW_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    git_stdout_to_file(toplevel, &["diff", "--binary", "HEAD"], &patch_file).await?;
    let patch_len = std::fs::metadata(&patch_file).map(|m| m.len()).unwrap_or(0);
    let applied = if patch_len > 0 {
        let patch_str = patch_file
            .to_str()
            .ok_or_else(|| "patch path is not valid UTF-8".to_string())?;
        git(dir, &["apply", "--whitespace=nowarn", patch_str]).await
    } else {
        Ok(String::new())
    };
    let _ = tokio::fs::remove_file(&patch_file).await;
    applied?;

    // 2. Untracked, non-ignored files — `git diff` is blind to them, so they
    //    ride as real copies. `-z` NUL-delimits (spaces/newlines in paths),
    //    quotePath off keeps non-ASCII literal.
    let listing = git(
        toplevel,
        &[
            "-c",
            "core.quotePath=false",
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )
    .await?;
    let mut overlay: Vec<String> = Vec::new();
    for rel in listing.split('\0').filter(|p| !p.is_empty()) {
        match copy_preserving_mtime(&toplevel.join(rel), &dir.join(rel)).await {
            Ok(()) => {}
            // A file that vanished between listing and copy is dirty-state
            // churn, not a snapshot failure.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(format!(
                    "could not copy untracked file `{rel}` into the candidate snapshot: {e}"
                ));
            }
        }
        // Only files under the workspace root enter the overlay set — the
        // real-tree RepoStatusPort (rooted at the workspace root) cannot see
        // the others either.
        if let Ok(ws_rel) = Path::new(rel).strip_prefix(root_rel) {
            overlay.push(ws_rel.to_string_lossy().into_owned());
        }
    }

    // 3. Seal the snapshot as ONE baseline commit in the shadow's PRIVATE
    //    index (detached HEAD — no branch, no user index, no stash). The
    //    blanket `git add -A` the fleet bans for shared/contested trees is
    //    safe — and wanted — here: this worktree was created microseconds
    //    ago and is owned by exactly this candidate, so there is nothing of
    //    anyone else's to sweep. `--no-verify`/`--no-gpg-sign` keep user
    //    hooks and signing out of snapshot plumbing.
    git(dir, &["add", "-A"]).await?;
    let mut commit_args: Vec<&str> = SNAPSHOT_IDENT.to_vec();
    commit_args.extend([
        "commit",
        "--allow-empty",
        "--no-verify",
        "--no-gpg-sign",
        "-q",
        "-m",
        "stella: candidate baseline snapshot",
    ]);
    git(dir, &commit_args).await?;
    let baseline = git(dir, &["rev-parse", "HEAD"]).await?.trim().to_string();
    Ok((overlay, baseline))
}

/// The snapshot's untracked view. Inside the shadow, files that were
/// untracked in the REAL tree are baseline-committed (winner adoption needs
/// them diffable), so plain `git ls-files --others` no longer reports them —
/// and the witness tamper watchlist, recorded against the real tree's
/// untracked fingerprints, would read every witness file as deleted. This
/// port reports the union: the shadow's own untracked files (the
/// candidate's new work) plus the overlay set, fingerprinted from the
/// shadow's filesystem — content hashes make an untouched overlay match the
/// real-tree fingerprint exactly, while an edited one differs,
/// and a deleted one is absent: the same semantics the real tree shows.
struct SnapshotRepoStatus {
    inner: GitRepoStatus,
    ws_root: PathBuf,
    /// Ws-root-relative paths of the untracked files copied into the shadow.
    overlay: Vec<String>,
}

#[async_trait]
impl RepoStatusPort for SnapshotRepoStatus {
    async fn untracked_fingerprints(&self) -> HashMap<String, String> {
        let mut map = self.inner.untracked_fingerprints().await;
        for rel in &self.overlay {
            if map.contains_key(rel) {
                continue;
            }
            if let Some(fp) = fs_fingerprint(&self.ws_root.join(rel)) {
                map.insert(rel.clone(), fp);
            }
        }
        map
    }

    async fn tracked_fingerprints(&self) -> HashMap<String, String> {
        self.inner.tracked_fingerprints().await
    }

    async fn artifact_identity(&self, path: &str) -> Option<stella_pipeline::ArtifactIdentity> {
        fs_artifact_identity(&self.ws_root, path)
    }
}

/// One live candidate shadow — see the module docs for the lifecycle.
pub(crate) struct GitCandidateWorkspace {
    /// The real repo's toplevel (canonicalized): the adoption target.
    toplevel: PathBuf,
    /// The shadow worktree directory.
    dir: PathBuf,
    /// Workspace root under the shadow (the session root may be a subdir of
    /// the repository toplevel).
    root: String,
    /// Immutable baseline commit representing the session tree at creation.
    baseline: String,
    /// The real repository's refs, and their values, as of this candidate's
    /// creation. A candidate worktree shares `.git` with the graded tree, so
    /// this is the only record of what the shared ref namespace looked like
    /// before the worker could reach it (#2541) — see [`ref_escape`].
    refs_at_create: BTreeMap<String, String>,
    /// Latest candidate commit whose exact bytes were verified.
    sealed: Mutex<Option<String>>,
    /// Latest candidate commit whose bytes have already been applied to the
    /// real tree — by [`CandidateWorkspace::deliver_checkpoint`] mid-run or
    /// by [`CandidateWorkspace::adopt`] at the end — or `None` if nothing
    /// has been delivered yet. The anchor [`GitCandidateWorkspace::escaped_paths_inner`]
    /// and [`GitCandidateWorkspace::adopt_inner`] diff *from*, in place of
    /// the workspace's original `baseline`, so a second delivery only ever
    /// sends its own remainder and the escape check only ever looks at bytes
    /// nothing has legitimately sent yet (#2941).
    delivered: Mutex<Option<String>>,
    /// The candidate's tool surface: snapshot-rooted registry + custom tools,
    /// optionally layered under the session's `candidate_safe`-filtered MCP
    /// view (issue #248 Phase 1, see [`GitCandidateWorkspaces::with_candidate_mcp`]).
    /// Boxed (not the concrete `CustomToolSet`) because the two cases have
    /// different concrete types; owned so the boxed workspace can hand out
    /// `&dyn ToolExecutor`.
    tools: Box<dyn stella_core::ToolExecutor>,
    /// Constructed before dispatch and incapable of general mutation or egress.
    witness_tools: WitnessToolExecutor,
    diagnostics: GitDiagnosticRunner,
    /// The candidate's test runner, bracketed so the paths a verification run
    /// writes are attributed to that run rather than read as the worker
    /// tampering with a verified tree (#2877) — see [`oracle_writes`].
    tests: ObservedTestRunner,
    /// What those brackets recorded, shared with [`Self::tests`]. Consulted
    /// by [`Self::sealed_unchanged_inner`]; cleared by every seal.
    oracle_writes: Arc<OracleWrites>,
    repo_status: SnapshotRepoStatus,
    /// This candidate's private task board plus its announce latch — the
    /// pipeline drives it through [`CandidateWorkspace::seed_task_board`]
    /// right after `create` (#1719); see [`task_events`].
    task_board: task_events::CandidateTaskBoard,
    /// Whether the pipeline marked this candidate as the sole one in its
    /// fan-out ([`CandidateWorkspace::announce_changes`]) — the fact
    /// [`GitCandidateWorkspace::deliver_checkpoint_inner`] gates on (#2941).
    sole_candidate: AtomicBool,
    /// Ignored paths present in the real tree and elided from this snapshot —
    /// see [`snapshot_gaps`]. A display list for the candidate's context, not
    /// a set anything branches on.
    omitted: Vec<String>,
}

impl GitCandidateWorkspace {
    #[cfg(test)]
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    async fn seal_inner(&self) -> Result<(), WorkspaceError> {
        let fail = |reason: String| WorkspaceError::Seal {
            reason,
            workspace: self.dir.display().to_string(),
        };
        // A worker may have attached HEAD to a branch the user keeps, which
        // would land this machinery commit — witness scaffolding included —
        // in a history nobody asked to change. Detaching rewrites HEAD only;
        // the candidate's tree and index are exactly as the worker left them.
        //
        // This runs BEFORE the ref audit, and the order is load-bearing
        // (#2641): the audit now RESTORES what it finds, and a restore issued
        // from the main worktree drags an attached candidate HEAD back with
        // the branch — orphaning the candidate's own commits from its own
        // checkout. Detaching first pins the candidate's work at the value
        // the worker left before the branch is yanked back. Attribution is
        // unaffected: `--no-deref` writes the commit HEAD already resolved
        // to, so the audit's `rev-parse HEAD` reads the same value either
        // way, and a `HEAD` too broken to detach is also too broken to
        // attribute — the audit reports nothing in that case regardless.
        ref_escape::detach_head(&self.dir).await.map_err(fail)?;
        // #2541: the seal is the moment this candidate's bytes are certified,
        // so it is also where a candidate that wrote into the GRADED tree's
        // ref namespace is refused. A worktree isolates files, not refs; the
        // audit runs before the commit below so a candidate that already
        // escaped never gets a further commit built on top of the damage.
        let audit = self.escaped_refs_inner().await;
        if !audit.escaped.is_empty() {
            let restored = self.restore(&audit.escaped).await;
            return Err(fail(ref_escape::refusal(&restored, audit.unexamined)));
        }
        git(&self.dir, &["add", "-A"]).await.map_err(fail)?;
        let mut commit_args: Vec<&str> = SNAPSHOT_IDENT.to_vec();
        commit_args.extend([
            "commit",
            "--allow-empty",
            "--no-verify",
            "--no-gpg-sign",
            "-q",
            "-m",
            CANDIDATE_SEAL_SUBJECT,
        ]);
        git(&self.dir, &commit_args).await.map_err(fail)?;
        let sealed = git(&self.dir, &["rev-parse", "HEAD"])
            .await
            .map_err(fail)?
            .trim()
            .to_string();
        // Recover from a poisoned lock rather than re-panicking. The guarded
        // value is one `Option<String>` that no path can leave half-written,
        // so there is no broken invariant to protect — and a panic elsewhere
        // in the fan-out would otherwise convert this candidate's seal (and
        // the `sealed_unchanged`/`adopt` reads below) from a named
        // `WorkspaceError` into a second panic. Same convention as
        // `main::test_env::lock` and the deck's session-id mutex.
        *self.sealed.lock().unwrap_or_else(|p| p.into_inner()) = Some(sealed);
        // The `add -A` above swept every previously-attributed path into this
        // commit, so those attributions can no longer excuse anything except a
        // mutation made *after* this seal — which is exactly what the drift
        // check exists to catch (#2877).
        self.oracle_writes.forget();
        Ok(())
    }

    async fn sealed_unchanged_inner(&self) -> Result<bool, WorkspaceError> {
        let fail = |reason: String| WorkspaceError::Seal {
            reason,
            workspace: self.dir.display().to_string(),
        };
        let sealed = self
            .sealed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .ok_or_else(|| fail("candidate has no verified seal".to_string()))?;
        let head = git(&self.dir, &["rev-parse", "HEAD"]).await.map_err(fail)?;
        // `-z` and `--no-renames` so [`adopt::drifted_paths`] gets a total
        // record shape: unquoted paths, one path per record. The scratch
        // subtraction it applies is argued there — in short, a mutation
        // adoption already excludes by pathspec cannot make the sealed tree
        // disagree with the tree that was verified, and the oracle runs in
        // this worktree immediately before this check.
        //
        // The second subtraction, of paths the verification run was *observed*
        // writing, is argued in [`oracle_writes`]: a fixed scratch list can
        // only ever name the artifact types someone already lost work to, and
        // `.gcda` was the next one (#2877). Everything not attributed to an
        // observed run remains fatal, so the check still refuses a worker that
        // moved a verified tree.
        let status = git(
            &self.dir,
            &[
                "status",
                "--porcelain",
                "--no-renames",
                "-z",
                "--untracked-files=all",
            ],
        )
        .await
        .map_err(fail)?;
        let unattributed = adopt::drifted_paths(&status)
            .into_iter()
            .filter(|path| !self.oracle_writes.wrote(path))
            .count();
        Ok(head.trim() == sealed && unattributed == 0)
    }

    /// The commit [`Self::adopt_inner`] and [`Self::escaped_paths_inner`]
    /// diff *from*: the last commit this workspace actually delivered to the
    /// real tree, or the original snapshot `baseline` if nothing has been
    /// delivered yet. Reading `self.baseline` directly in either of those
    /// would re-offer already-delivered bytes to `git apply` (a guaranteed
    /// conflict) and would misread the real tree legitimately holding them
    /// as an escape (#2941) — this is the one place either question is
    /// answered, so the two callers cannot drift.
    pub(super) fn delivery_anchor(&self) -> String {
        self.delivered
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .unwrap_or_else(|| self.baseline.clone())
    }

    /// [`CandidateWorkspace::deliver_checkpoint`]: seal the current state,
    /// fail closed on an escape exactly as the verify-stage seal does, then
    /// adopt the remainder since [`Self::delivery_anchor`]. A no-op for any
    /// candidate that is not the sole one in its fan-out — see the trait
    /// doc for why that is decided here rather than by the caller.
    async fn deliver_checkpoint_inner(&self) -> Result<Vec<AdoptedChange>, WorkspaceError> {
        if !self.sole_candidate.load(Ordering::Relaxed) {
            return Ok(Vec::new());
        }
        self.seal_inner().await?;
        let escaped = self.escaped_paths_inner().await;
        if !escaped.is_empty() {
            return Err(WorkspaceError::Adopt {
                reason: format!(
                    "candidate wrote through its isolation into the real tree at: {}",
                    escaped.join(", ")
                ),
                paths: escaped,
                workspace: self.dir.display().to_string(),
            });
        }
        self.adopt_inner(&[]).await
    }

    /// The teardown half of the ref repair (#2641): audit and restore the
    /// shared ref namespace one last time, before `cleanup` removes the
    /// candidate.
    ///
    /// [`Self::seal_inner`]'s repair only fires when a seal is reached. A run
    /// that dies on the turn budget — the shape that cost a measured
    /// Terminal-Bench trial its score — is torn down through
    /// [`CandidateWorkspace::remove`] having never sealed, so without this the
    /// graded tree keeps the moved ref.
    ///
    /// Order matters twice. It must precede `cleanup`, because the
    /// attribution probes name the candidate's `HEAD` and private baseline
    /// and are only resolvable while the worktree and its shared object store
    /// are still there. And it must not detach `HEAD` first the way the seal
    /// does: that write exists to keep the candidate's own commits reachable
    /// from a checkout that is about to be deleted anyway, and the candidate's
    /// value stays in the restored ref's reflog regardless.
    ///
    /// Cost on a clean candidate: one `for-each-ref`. Best-effort and silent
    /// — [`CandidateWorkspace::remove`] returns `()`, so there is no channel
    /// to report a teardown repair on; surfacing it is #2813.
    ///
    /// Both repair seams are in-process, and `refs_at_create` lives only in
    /// this struct: a `stella` killed outright leaves the moved ref with
    /// nothing on disk that knows what it should be. That residual gap — a
    /// durable per-candidate record and a cross-process sweep, with the
    /// liveness and consent questions it raises — is #2813 as well.
    async fn restore_escaped_refs_inner(&self) {
        let audit = self.escaped_refs_inner().await;
        if !audit.escaped.is_empty() {
            let _ = self.restore(&audit.escaped).await;
        }
    }
}

#[async_trait]
impl CandidateWorkspace for GitCandidateWorkspace {
    fn root(&self) -> &str {
        &self.root
    }

    fn tools(&self) -> &dyn stella_core::ToolExecutor {
        &*self.tools
    }

    fn witness_tools(&self) -> &dyn stella_core::ToolExecutor {
        &self.witness_tools
    }

    fn diagnostics(&self) -> &dyn DiagnosticRunner {
        &self.diagnostics
    }

    fn tests(&self) -> &dyn TestRunner {
        &self.tests
    }

    fn repo_status(&self) -> &dyn RepoStatusPort {
        &self.repo_status
    }

    fn omitted_paths(&self) -> &[String] {
        &self.omitted
    }

    fn seed_task_board(&self, steps: &[String], announce: bool) {
        self.task_board.seed(steps, announce);
    }

    fn announce_changes(&self, announce: bool) {
        self.sole_candidate.store(announce, Ordering::Relaxed);
    }

    fn mark_plan_step(&self, id: &str, status: stella_protocol::TaskStatus) {
        self.task_board.mark(id, status);
    }

    async fn seal(&self) -> Result<(), WorkspaceError> {
        self.seal_inner().await
    }

    async fn sealed_is_unchanged(&self) -> Result<bool, WorkspaceError> {
        self.sealed_unchanged_inner().await
    }

    async fn escaped_paths(&self) -> Vec<String> {
        self.escaped_paths_inner().await
    }

    async fn deliver_checkpoint(&self) -> Result<Vec<AdoptedChange>, WorkspaceError> {
        self.deliver_checkpoint_inner().await
    }

    async fn adopt(&self, withhold: &[String]) -> Result<Vec<AdoptedChange>, WorkspaceError> {
        self.adopt_inner(withhold).await
    }

    async fn graft_witness(&self, source_root: &str, path: &str) -> Result<(), WorkspaceError> {
        witness_tools::graft(&self.root, &self.dir, source_root, path).await
    }

    async fn remove(&self) {
        self.restore_escaped_refs_inner().await;
        cleanup(&self.toplevel, &self.dir).await;
    }
}

#[cfg(test)]
mod tests;
