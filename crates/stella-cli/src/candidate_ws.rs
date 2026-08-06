//! Best-of-N candidate isolation over real git — the production
//! [`CandidateWorkspacePort`]. Each candidate gets a detached shadow
//! worktree (the `verify_done` shadow pattern: temp-dir path, pid+counter
//! name, forced removal + prune on every exit) snapshotting the user's
//! CURRENT tree state:
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
//! The user's working tree, index, and stash are NEVER touched: the stash is
//! shared machine state other sessions rely on, so it is banned outright —
//! no `git stash` appears anywhere in this module, and every command that
//! runs against the real repo is read-only except the final winner-adoption
//! `git apply` (worktree only, no `--index`). Final verification first seals
//! the shadow in a private commit. Adoption requires the shadow to remain
//! byte-identical to that seal, diffs the immutable baseline against that
//! exact verified commit, and applies the result in one `git apply`. The
//! worker cannot race verification with adoption, and a real tree that no
//! longer matches what the candidate started from fails the whole adoption
//! loudly instead of half-applying — naming which of the two divergences it
//! was, a user edit or a candidate that wrote outside its snapshot (see
//! [`adopt::PathDivergence`]).
//!
//! # What a candidate's engine can reach
//!
//! Candidates drive the built-in [`stella_tools::ToolRegistry`] PLUS the session's custom
//! script tools, both rooted at the snapshot (with the session's workspace
//! rules and schema gate applied) — a [`CustomToolSet`] owning the registry
//! by `Arc`. Custom tools spawn subprocesses with the snapshot as cwd
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
//! The interactive layer (`ask_user`) is withheld regardless of phase,
//! unconditionally: a fan-out of N candidates has no single owner for a
//! prompt, so candidates run non-interactively — there is no
//! `InteractiveToolSet` in a candidate's stack for either MCP phase to
//! reintroduce it through.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use std::sync::Arc;
use stella_fleet::git::{GitCli, SystemGitCli};
use stella_pipeline::ports::{
    AdoptedChange, CandidateWorkspace, CandidateWorkspacePort, DiagnosticRunner, McpPrefetchPort,
    RepoStatusPort, TestRunner, WorkspaceError,
};

use stella_tools::RegistryOptions;
use stella_tools::custom::{CustomTool, CustomToolSet};

use crate::agent::{
    GitDiagnosticRunner, GitRepoStatus, TypedTestRunner, fs_artifact_identity, fs_fingerprint,
};

mod adopt;
mod escape;
mod snapshot_gaps;
mod witness_tools;
use witness_tools::WitnessToolExecutor;

/// The commit identity for snapshot plumbing commits (which exist only
/// inside the shadow and are discarded with it) — the user's repo may have
/// no identity configured (CI), and their real identity must never be
/// implied on machinery commits.
const SNAPSHOT_IDENT: [&str; 4] = [
    "-c",
    "user.name=stella-pipeline",
    "-c",
    "user.email=pipeline@stella.invalid",
];

/// Shadow names carry pid + a process-wide counter (the `verify_done`
/// pattern): concurrent candidates must never collide on a path.
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
/// the directory both, then a prune for any stale registration (the
/// `verify_done` cleanup discipline: called on every path, success or
/// failure).
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
    /// toplevel (the `verify_done` canonicalization trap: the shadow mirrors
    /// the TOPLEVEL, and the candidate's ports re-descend into the matching
    /// subdirectory).
    root: PathBuf,
    /// Construction inputs for the per-candidate tool registry — host
    /// attestations and media prerequisites, same as the session's.
    options: RegistryOptions,
    /// The operator's tool switches, applied over each candidate's own tool
    /// stack. A candidate registry is built from the same `RegistryOptions`
    /// as the session's, so without this best-of-N would be a way around a
    /// `"tools": {"bash": "off"}`.
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
}

impl GitCandidateWorkspaces {
    pub(crate) fn new(
        root: PathBuf,
        options: RegistryOptions,
        policy: stella_tools::policy::ToolPolicy,
        custom_tools: Vec<CustomTool>,
        active_rules: crate::rules::ResolvedRules,
    ) -> Self {
        Self {
            root,
            options,
            policy,
            custom_tools,
            active_rules,
            candidate_mcp: None,
            events: None,
        }
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
        let toplevel = PathBuf::from(toplevel.trim());
        let toplevel = toplevel.canonicalize().unwrap_or(toplevel);
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
        git(&toplevel, &["worktree", "add", "--detach", dir_str, &head])
            .await
            .map_err(snap)?;

        // From here on every failure must tear the shadow down.
        match populate_snapshot(&toplevel, &dir, &root_rel).await {
            Ok((overlay_untracked, baseline)) => {
                let ws_root = dir.join(&root_rel);
                let registry =
                    crate::agent::new_tool_registry(ws_root.clone(), self.options.clone()).await;
                // Same governance as the session registry: workspace rules
                // and the schema gate travel with the tree — best-of-N must
                // not be a way around them. Applied while `registry` is still
                // a plain `ToolRegistry`, before it moves into the `Arc`.
                //
                // A schema-gate failure is still a failure PAST the worktree
                // registration, so it takes the same teardown as
                // `populate_snapshot`'s errors below — a bare `?` here leaked
                // both the registered worktree and its temp directory, and the
                // leak survived the process (only a later `git worktree prune`
                // would notice).
                if let Err(reason) = crate::agent::populate_schema_index(&registry, &ws_root) {
                    cleanup(&toplevel, &dir).await;
                    return Err(snap(reason));
                }
                crate::rules::attach_rule_guards(&registry, &self.active_rules);
                // `attach_rule_guards` is what gives this registry a live
                // HookBus, so the bridge must follow it — and both must
                // happen while `registry` is still a concrete `ToolRegistry`,
                // before the `Arc<dyn ToolExecutor>` coercion below erases it.
                if let Some(events) = &self.events {
                    registry.bridge_policy_plane(events.clone());
                    // Reads only. A candidate's *mutations* are re-emitted by
                    // adoption against the real tree, so announcing them here
                    // would claim edits the user's checkout has not received —
                    // but silencing the whole stream (what this used to do)
                    // also swallowed every read, leaving an isolated run's
                    // Files tab reading "no files touched yet" through
                    // hundreds of tool calls.
                    registry.attach_read_events(events.clone());
                }
                // The candidate's tool surface: the snapshot-rooted registry
                // plus the session's custom script tools, owned as one value
                // (the workspace outlives every borrow). Custom tools re-root
                // to `ws_root`, so their subprocesses run in the shadow.
                let registry: Arc<dyn stella_core::ToolExecutor> = Arc::new(registry);
                let witness_tools = WitnessToolExecutor::new(ws_root.clone(), registry.clone());
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
                // Outermost, over registry + customs + candidate MCP alike.
                let tools: Box<dyn stella_core::ToolExecutor> = Box::new(
                    crate::agent::PolicyToolSet::new_owned(tools, self.policy.clone()),
                );
                let omitted = snapshot_gaps::omitted_ignored_paths(&toplevel, &root_rel).await;
                Ok(GitCandidateWorkspace {
                    toplevel,
                    dir: dir.clone(),
                    root: ws_root.display().to_string(),
                    baseline,
                    sealed: Mutex::new(None),
                    tools,
                    witness_tools,
                    diagnostics: GitDiagnosticRunner::new(ws_root.clone()),
                    tests: TypedTestRunner {
                        root: ws_root.clone(),
                    },
                    repo_status: SnapshotRepoStatus {
                        inner: GitRepoStatus {
                            root: ws_root.clone(),
                        },
                        ws_root,
                        overlay: overlay_untracked,
                    },
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
    /// Latest candidate commit whose exact bytes were verified.
    sealed: Mutex<Option<String>>,
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
    tests: TypedTestRunner,
    repo_status: SnapshotRepoStatus,
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
        git(&self.dir, &["add", "-A"]).await.map_err(fail)?;
        let mut commit_args: Vec<&str> = SNAPSHOT_IDENT.to_vec();
        commit_args.extend([
            "commit",
            "--allow-empty",
            "--no-verify",
            "--no-gpg-sign",
            "-q",
            "-m",
            "stella: candidate verified snapshot",
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
        let status = git(
            &self.dir,
            &["status", "--porcelain", "--untracked-files=all"],
        )
        .await
        .map_err(fail)?;
        Ok(head.trim() == sealed && status.is_empty())
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

    async fn seal(&self) -> Result<(), WorkspaceError> {
        self.seal_inner().await
    }

    async fn sealed_is_unchanged(&self) -> Result<bool, WorkspaceError> {
        self.sealed_unchanged_inner().await
    }

    async fn escaped_paths(&self) -> Vec<String> {
        self.escaped_paths_inner().await
    }

    async fn adopt(&self, withhold: &[String]) -> Result<Vec<AdoptedChange>, WorkspaceError> {
        self.adopt_inner(withhold).await
    }

    async fn graft_witness(&self, source_root: &str, path: &str) -> Result<(), WorkspaceError> {
        witness_tools::graft(&self.root, &self.dir, source_root, path).await
    }

    async fn remove(&self) {
        cleanup(&self.toplevel, &self.dir).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::FileChangeKind;

    #[test]
    fn candidate_port_keeps_the_exact_host_operation_journal() {
        let journal: Arc<dyn stella_media::MediaOperationJournal> = Arc::new(
            stella_media::SqliteMediaOperationJournal::open_in_memory(Default::default()).unwrap(),
        );
        let options = RegistryOptions {
            media_operation_journal: Some(journal.clone()),
            ..Default::default()
        };

        let port = GitCandidateWorkspaces::new(
            PathBuf::from("unused"),
            options,
            Default::default(),
            Vec::new(),
            crate::rules::ResolvedRules::default(),
        );

        assert!(Arc::ptr_eq(
            &journal,
            port.options.media_operation_journal.as_ref().unwrap()
        ));
    }

    /// Run `git <args>` in `root` with hook-exported `GIT_*` vars scrubbed
    /// (the verify_done test discipline — without it, running the suite from
    /// inside a git hook re-targets every command at the HOST repo) and
    /// return stdout. Panics on failure: these are test fixtures.
    fn scratch_git(root: &Path, args: &[&str]) -> String {
        let mut cmd = std::process::Command::new("git");
        stella_tools::exec::scrub_sensitive_std_env(&mut cmd);
        cmd.args(args).current_dir(root);
        for var in stella_tools::exec::GIT_REPO_ENV_VARS {
            cmd.env_remove(var);
        }
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Build the canonical dirty repo every test starts from:
    /// - committed: `tracked.txt` ("base\n"), `.gitignore` (ignores
    ///   `ignored.txt`)
    /// - a pre-existing stash entry (fixture only — the production code must
    ///   never touch it)
    /// - uncommitted tracked edit: `tracked.txt` = "base\ndirty\n"
    /// - staged-but-uncommitted new file: `staged.txt`
    /// - untracked: `untracked.txt`; ignored: `ignored.txt`
    pub(super) fn scaffold(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "stella_cwstest_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "t@t.t"],
            &["config", "user.name", "t"],
            &["config", "commit.gpgsign", "false"],
        ] {
            scratch_git(&root, args);
        }
        std::fs::write(root.join("tracked.txt"), "base\n").unwrap();
        std::fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
        scratch_git(&root, &["add", "."]);
        scratch_git(&root, &["commit", "-q", "-m", "base"]);
        // The shared-stash fixture: other sessions' stashes must survive
        // best-of-N byte-identically.
        std::fs::write(root.join("tracked.txt"), "stash-fixture\n").unwrap();
        scratch_git(&root, &["stash", "push", "-q", "-m", "fixture"]);
        // The dirty state candidates must see.
        std::fs::write(root.join("tracked.txt"), "base\ndirty\n").unwrap();
        std::fs::write(root.join("staged.txt"), "staged\n").unwrap();
        scratch_git(&root, &["add", "staged.txt"]);
        std::fs::write(root.join("untracked.txt"), "hello\n").unwrap();
        std::fs::write(root.join("ignored.txt"), "secret\n").unwrap();
        root
    }

    /// The observable state of the real repo that candidate isolation must
    /// leave byte-identical: worktree status, staged diff, stash list, HEAD.
    fn tree_state(root: &Path) -> (String, String, String, String) {
        (
            scratch_git(root, &["status", "--porcelain"]),
            scratch_git(root, &["diff", "--cached"]),
            scratch_git(root, &["stash", "list"]),
            scratch_git(root, &["rev-parse", "HEAD"]),
        )
    }

    fn assert_no_candidate_worktrees(root: &Path) {
        let listing = scratch_git(root, &["worktree", "list", "--porcelain"]);
        assert!(
            !listing.contains("stella_candidate_"),
            "candidate worktrees must not stay registered: {listing}"
        );
    }

    pub(super) fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    #[tokio::test]
    async fn snapshot_mirrors_dirty_staged_and_untracked_state_without_touching_the_real_tree() {
        let root = scaffold("snap");
        let before = tree_state(&root);
        let port = GitCandidateWorkspaces::new(
            root.clone(),
            RegistryOptions::default(),
            Default::default(),
            Vec::new(),
            crate::rules::ResolvedRules::default(),
        );

        let ws = port.create_workspace().await.unwrap();
        // Uncommitted tracked edit, staged-but-uncommitted new file, and the
        // untracked file are all visible; the ignored file is not.
        assert_eq!(read(&ws.dir().join("tracked.txt")), "base\ndirty\n");
        assert_eq!(read(&ws.dir().join("staged.txt")), "staged\n");
        assert_eq!(read(&ws.dir().join("untracked.txt")), "hello\n");
        assert!(
            !ws.dir().join("ignored.txt").exists(),
            "ignored files are not part of the snapshot"
        );

        // Fingerprint parity: the snapshot reports the overlay untracked file
        // with the REAL tree's complete content hash, so the
        // witness tamper watchlist keeps working inside candidates.
        let real = GitRepoStatus { root: root.clone() }
            .untracked_fingerprints()
            .await;
        let snap = ws.repo_status().untracked_fingerprints().await;
        assert_eq!(
            snap.get("untracked.txt"),
            real.get("untracked.txt"),
            "overlay fingerprints must match the real tree's"
        );

        std::fs::create_dir_all(ws.dir().join("tests")).unwrap();
        std::fs::write(
            ws.dir().join("tests/witness.rs"),
            "#[test] fn witness() {}\n",
        )
        .unwrap();
        let witness_fingerprint = ws
            .repo_status()
            .untracked_fingerprints()
            .await
            .remove("tests/witness.rs")
            .expect("new witness is visible in the candidate delta");
        let identity = ws
            .repo_status()
            .artifact_identity("tests/witness.rs")
            .await
            .expect("candidate status exposes the no-follow artifact identity");
        assert!(identity.is_regular_single_link());
        assert_eq!(identity.fingerprint, witness_fingerprint);

        // The real tree, index, stash, and HEAD are untouched by creation.
        assert_eq!(tree_state(&root), before);

        ws.remove().await;
        assert_no_candidate_worktrees(&root);
        assert!(!ws.dir().exists(), "the shadow directory is removed");
        assert_eq!(
            tree_state(&root),
            before,
            "removal is also side-effect free"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn authored_witness_keeps_identity_through_seal_verification_and_exact_adoption() {
        let root = scaffold("witness-seal");
        let port = GitCandidateWorkspaces::new(
            root.clone(),
            RegistryOptions::default(),
            Default::default(),
            Vec::new(),
            crate::rules::ResolvedRules::default(),
        );
        let ws = port.create_workspace().await.unwrap();
        std::fs::create_dir_all(ws.dir().join("tests")).unwrap();
        std::fs::write(
            ws.dir().join("tests/authority_witness.rs"),
            "#[test] fn authority_witness() {}\n",
        )
        .unwrap();

        let authored = ws
            .repo_status()
            .artifact_identity("tests/authority_witness.rs")
            .await
            .expect("authored witness has an identity");
        ws.seal().await.unwrap();
        assert!(
            !ws.repo_status()
                .untracked_fingerprints()
                .await
                .contains_key("tests/authority_witness.rs"),
            "the seal intentionally reclassifies the witness as tracked"
        );
        let verified = ws
            .repo_status()
            .artifact_identity("tests/authority_witness.rs")
            .await;
        assert!(stella_pipeline::witness_identity_matches(
            &authored,
            verified.as_ref()
        ));

        let adopted = ws.adopt(&[]).await.unwrap();
        assert_eq!(adopted.len(), 1);
        assert_eq!(
            read(&root.join("tests/authority_witness.rs")),
            "#[test] fn authority_witness() {}\n"
        );
        ws.remove().await;
        assert_no_candidate_worktrees(&root);
        std::fs::remove_dir_all(&root).ok();
    }

    /// Withholding the witness must remove ONLY the witness. The whole risk of
    /// filtering the adoption patch is that an over-broad pathspec silently
    /// swallows real work, which would look like the model simply not doing
    /// the task — so this asserts both halves: the witness is absent from the
    /// real tree AND from the reported change list, while the production edit
    /// beside it lands byte-exactly.
    #[tokio::test]
    async fn withholding_the_witness_still_adopts_the_work_it_verified() {
        let root = scaffold("witness-withhold");
        let port = GitCandidateWorkspaces::new(
            root.clone(),
            RegistryOptions::default(),
            Default::default(),
            Vec::new(),
            crate::rules::ResolvedRules::default(),
        );
        let ws = port.create_workspace().await.unwrap();
        std::fs::create_dir_all(ws.dir().join("tests")).unwrap();
        std::fs::write(
            ws.dir().join("tests/authority_witness.rs"),
            "#[test] fn authority_witness() {}\n",
        )
        .unwrap();
        // The real work the witness exists to prove.
        std::fs::write(ws.dir().join("tracked.txt"), "the actual fix\n").unwrap();
        ws.seal().await.unwrap();

        let withhold = vec!["tests/authority_witness.rs".to_string()];
        let adopted = ws.adopt(&withhold).await.unwrap();

        assert_eq!(
            adopted.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(),
            vec!["tracked.txt"],
            "the withheld witness must not be reported as adopted"
        );
        assert_eq!(read(&root.join("tracked.txt")), "the actual fix\n");
        assert!(
            !root.join("tests/authority_witness.rs").exists(),
            "the witness must never reach the real tree"
        );
        ws.remove().await;
        assert_no_candidate_worktrees(&root);
        std::fs::remove_dir_all(&root).ok();
    }

    /// A witness whose filename contains glob metacharacters must exclude
    /// exactly itself. `:(exclude)` without `literal` would treat `[` and `*`
    /// as a pattern and could withhold production files that happen to match.
    #[tokio::test]
    async fn withholding_treats_the_path_literally_not_as_a_glob() {
        let root = scaffold("witness-glob");
        let port = GitCandidateWorkspaces::new(
            root.clone(),
            RegistryOptions::default(),
            Default::default(),
            Vec::new(),
            crate::rules::ResolvedRules::default(),
        );
        let ws = port.create_workspace().await.unwrap();
        std::fs::create_dir_all(ws.dir().join("tests")).unwrap();
        // `t*.rs` as a glob would also match `tracked_rs.rs`; as a literal it
        // matches only the file actually named `t*.rs`.
        std::fs::write(ws.dir().join("tests/t*.rs"), "#[test] fn w() {}\n").unwrap();
        std::fs::write(ws.dir().join("tests/tracked_rs.rs"), "// real work\n").unwrap();
        ws.seal().await.unwrap();

        let adopted = ws.adopt(&["tests/t*.rs".to_string()]).await.unwrap();

        assert_eq!(
            adopted.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(),
            vec!["tests/tracked_rs.rs"],
            "only the literally-named witness may be withheld"
        );
        assert!(root.join("tests/tracked_rs.rs").exists());
        assert!(!root.join("tests/t*.rs").exists());
        ws.remove().await;
        assert_no_candidate_worktrees(&root);
        std::fs::remove_dir_all(&root).ok();
    }

    /// A candidate's tool surface includes the session's custom script tools,
    /// and running one executes in the SNAPSHOT (cwd = shadow), never the real
    /// tree — the isolation guarantee for the grown tool surface.
    #[tokio::test]
    async fn custom_tools_reach_the_candidate_and_run_in_the_snapshot() {
        let root = scaffold("customtools");
        // A custom tool that writes a file into its cwd. Discovered from the
        // real root exactly as the session discovers it.
        std::fs::create_dir_all(root.join(".stella/tools")).unwrap();
        std::fs::write(
            root.join(".stella/tools/writer.toml"),
            "name = \"writer\"\n\
             description = \"write a marker file into the cwd\"\n\
             command = [\"sh\", \"-c\", \"printf candidate > candidate_wrote.txt\"]\n\
             [input_schema]\n\
             type = \"object\"\n",
        )
        .unwrap();
        let found = stella_tools::custom::discover(&root);
        let custom_tools = crate::tool_foundry::adopt::gate_discovery(found, &root).tools;
        assert_eq!(custom_tools.len(), 1, "the writer tool must be discovered");

        let port = GitCandidateWorkspaces::new(
            root.clone(),
            RegistryOptions::default(),
            Default::default(),
            custom_tools,
            crate::rules::ResolvedRules::default(),
        );
        let ws = port.create_workspace().await.unwrap();

        // The candidate model sees the custom tool in its schema…
        let names: Vec<String> = ws.tools().schemas().into_iter().map(|s| s.name).collect();
        assert!(
            names.iter().any(|n| n == "writer"),
            "candidate schemas must include the custom tool: {names:?}"
        );
        // …and it also still sees a built-in (the registry is the inner set).
        assert!(
            names.iter().any(|n| n == "read_file"),
            "candidate must still have the built-in registry"
        );

        // Executing it writes into the SNAPSHOT, not the real tree.
        let out = ws.tools().execute("writer", &serde_json::json!({})).await;
        assert!(!out.is_error(), "custom tool run failed: {out:?}");
        assert_eq!(
            read(&ws.dir().join("candidate_wrote.txt")),
            "candidate",
            "the custom tool must write inside the snapshot"
        );
        assert!(
            !root.join("candidate_wrote.txt").exists(),
            "the custom tool must NOT touch the real tree"
        );

        ws.remove().await;
        assert_no_candidate_worktrees(&root);
        std::fs::remove_dir_all(&root).ok();
    }

    /// A minimal [`stella_mcp::Transport`] fake for the wiring test below —
    /// no process, no socket; just enough of the MCP handshake plus one tool
    /// to prove a schema makes it through `GitCandidateWorkspaces::create`
    /// end to end, not just the isolated `CandidateMcpView` unit tests in
    /// `stella-mcp`.
    struct FakeMcpTransport;

    #[async_trait]
    impl stella_mcp::Transport for FakeMcpTransport {
        async fn request(
            &self,
            method: &str,
            _params: serde_json::Value,
        ) -> Result<serde_json::Value, stella_mcp::McpError> {
            Ok(match method {
                "initialize" => serde_json::json!({
                    "protocolVersion": stella_mcp::protocol::PREFERRED_PROTOCOL_VERSION
                }),
                "tools/list" => serde_json::json!({
                    "tools": [{ "name": "search", "inputSchema": { "type": "object" } }]
                }),
                "tools/call" => {
                    serde_json::json!({ "content": [{ "type": "text", "text": "docs ok" }] })
                }
                _ => serde_json::Value::Null,
            })
        }
        async fn notify(
            &self,
            _method: &str,
            _params: serde_json::Value,
        ) -> Result<(), stella_mcp::McpError> {
            Ok(())
        }
        async fn close(&self) -> Result<(), stella_mcp::McpError> {
            Ok(())
        }
    }

    /// Issue #248 Phase 1's wiring witness: a session that shares its MCP
    /// toolset via `.with_candidate_mcp` must have the allowlisted server's
    /// tools actually reach a REAL candidate workspace's advertised schema —
    /// closing the gap `stella-mcp`'s `CandidateMcpView` tests can't (they
    /// exercise the view in isolation, never through this port).
    #[tokio::test]
    async fn candidate_mcp_reaches_the_candidate_when_the_session_shares_it() {
        let root = scaffold("mcpwiring");
        let mut docs_client = stella_mcp::McpClient::new("docs", Box::new(FakeMcpTransport));
        docs_client.initialize().await.unwrap();
        let mcp = Arc::new(
            stella_mcp::McpToolSet::from_clients(vec![docs_client])
                .with_candidate_safe_servers(["docs"]),
        );
        let port = GitCandidateWorkspaces::new(
            root.clone(),
            RegistryOptions::default(),
            Default::default(),
            Vec::new(),
            crate::rules::ResolvedRules::default(),
        )
        .with_candidate_mcp(mcp);
        let ws = port.create_workspace().await.unwrap();

        let names: Vec<String> = ws.tools().schemas().into_iter().map(|s| s.name).collect();
        assert!(
            names.iter().any(|n| n == "mcp__docs__search"),
            "the allowlisted server's tool must reach the candidate: {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "read_file"),
            "the candidate must still have its snapshot-rooted built-ins: {names:?}"
        );

        ws.remove().await;
        assert_no_candidate_worktrees(&root);
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn store_backed_guard_survives_candidate_snapshot_and_winner_adoption() {
        let root = scaffold("storeguard");
        std::fs::write(root.join(".gitignore"), "ignored.txt\n.stella/private/\n").unwrap();
        {
            let store = stella_store::Store::open(&root).unwrap();
            store
                .upsert_rule(
                    "protect-store-path",
                    "---\nguard-tool: Write\nguard-deny-path: protected/**\n---\nStore guard remains binding in candidates.",
                    "ext:policy",
                )
                .unwrap();
        }
        let authority = crate::settings::AuthorityPolicy {
            project_prompts_allowed: true,
            ..crate::settings::AuthorityPolicy::default()
        };
        let active_rules = crate::rules::load_workspace_rules(&root, &authority);
        let port = GitCandidateWorkspaces::new(
            root.clone(),
            RegistryOptions::default(),
            Default::default(),
            Vec::new(),
            active_rules,
        );
        let ws = port.create_workspace().await.unwrap();

        let output = ws
            .tools()
            .execute(
                "write_file",
                &serde_json::json!({"path": "protected/store.txt", "content": "no\n"}),
            )
            .await;
        ws.seal().await.unwrap();
        let adopted = ws.adopt(&[]).await.unwrap();
        let landed = root.join("protected/store.txt").exists();
        ws.remove().await;
        assert_no_candidate_worktrees(&root);
        std::fs::remove_dir_all(&root).ok();

        assert!(
            output.is_error(),
            "candidate bypassed store guard: {output:?}"
        );
        assert!(
            adopted.is_empty(),
            "prohibited change was adoptable: {adopted:?}"
        );
        assert!(!landed, "winner adopted a store-guarded change");
    }

    #[tokio::test]
    async fn ignored_rule_guard_survives_candidate_snapshot_and_winner_adoption() {
        let root = scaffold("ignoredguard");
        std::fs::write(
            root.join(".gitignore"),
            "ignored.txt\n.stella/rules/protect-ignored.md\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".stella/rules")).unwrap();
        std::fs::write(
            root.join(".stella/rules/protect-ignored.md"),
            "---\nguard-tool: Write\nguard-deny-path: protected/**\n---\nIgnored file guard remains binding in candidates.",
        )
        .unwrap();
        let authority = crate::settings::AuthorityPolicy {
            project_prompts_allowed: true,
            ..crate::settings::AuthorityPolicy::default()
        };
        let active_rules = crate::rules::load_workspace_rules(&root, &authority);
        let port = GitCandidateWorkspaces::new(
            root.clone(),
            RegistryOptions::default(),
            Default::default(),
            Vec::new(),
            active_rules,
        );
        let ws = port.create_workspace().await.unwrap();

        let output = ws
            .tools()
            .execute(
                "write_file",
                &serde_json::json!({"path": "protected/ignored.txt", "content": "no\n"}),
            )
            .await;
        ws.seal().await.unwrap();
        let adopted = ws.adopt(&[]).await.unwrap();
        let landed = root.join("protected/ignored.txt").exists();
        ws.remove().await;
        assert_no_candidate_worktrees(&root);
        std::fs::remove_dir_all(&root).ok();

        assert!(
            output.is_error(),
            "candidate bypassed ignored guard: {output:?}"
        );
        assert!(
            adopted.is_empty(),
            "prohibited change was adoptable: {adopted:?}"
        );
        assert!(!landed, "winner adopted an ignored-rule-guarded change");
    }

    #[tokio::test]
    async fn winner_adoption_lands_only_the_winners_changes() {
        let root = scaffold("adopt");
        let port = GitCandidateWorkspaces::new(
            root.clone(),
            RegistryOptions::default(),
            Default::default(),
            Vec::new(),
            crate::rules::ResolvedRules::default(),
        );
        let loser = port.create_workspace().await.unwrap();
        let winner = port.create_workspace().await.unwrap();

        // The loser edits a tracked file and creates a new one.
        std::fs::write(loser.dir().join("tracked.txt"), "base\ndirty\nloser\n").unwrap();
        std::fs::write(loser.dir().join("loser.txt"), "residue\n").unwrap();
        // The winner edits the tracked file, creates a file, and deletes the
        // pre-existing untracked file.
        std::fs::write(winner.dir().join("tracked.txt"), "base\ndirty\nwinner\n").unwrap();
        std::fs::write(winner.dir().join("winner.txt"), "new\n").unwrap();
        std::fs::remove_file(winner.dir().join("untracked.txt")).unwrap();
        winner.seal().await.unwrap();

        let (_, before_cached, before_stash, before_head) = tree_state(&root);
        loser.remove().await;

        let mut adopted = winner.adopt(&[]).await.unwrap();
        adopted.sort_by(|a, b| a.path.cmp(&b.path));
        let described: Vec<(String, FileChangeKind)> =
            adopted.into_iter().map(|c| (c.path, c.kind)).collect();
        assert_eq!(
            described,
            vec![
                ("tracked.txt".to_string(), FileChangeKind::Modified),
                ("untracked.txt".to_string(), FileChangeKind::Deleted),
                ("winner.txt".to_string(), FileChangeKind::Created),
            ]
        );

        // Winner's changes landed; loser's never touched the real tree.
        assert_eq!(read(&root.join("tracked.txt")), "base\ndirty\nwinner\n");
        assert_eq!(read(&root.join("winner.txt")), "new\n");
        assert!(!root.join("untracked.txt").exists());
        assert!(!root.join("loser.txt").exists());

        // Index, stash, and HEAD are byte-identical: adoption writes only
        // the working tree.
        let (_, after_cached, after_stash, after_head) = tree_state(&root);
        assert_eq!(after_cached, before_cached, "the index is never touched");
        assert_eq!(after_stash, before_stash, "the stash is never touched");
        assert_eq!(after_head, before_head, "HEAD never moves");

        winner.remove().await;
        assert_no_candidate_worktrees(&root);
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn post_verification_worktree_drift_is_rejected_and_never_adopted() {
        let root = scaffold("sealed-drift");
        let before = tree_state(&root);
        let port = GitCandidateWorkspaces::new(
            root.clone(),
            RegistryOptions::default(),
            Default::default(),
            Vec::new(),
            crate::rules::ResolvedRules::default(),
        );
        let ws = port.create_workspace().await.unwrap();
        std::fs::write(ws.dir().join("verified.txt"), "verified bytes\n").unwrap();

        ws.seal()
            .await
            .expect("candidate state seals before verification");
        assert!(ws.sealed_is_unchanged().await.unwrap());
        std::fs::write(
            ws.dir().join("verified.txt"),
            "mutated after verification\n",
        )
        .unwrap();

        let error = ws.adopt(&[]).await.expect_err("drift must reject adoption");
        assert!(
            error.to_string().contains("changed after verification"),
            "{error}"
        );
        assert!(!root.join("verified.txt").exists());
        assert_eq!(
            tree_state(&root),
            before,
            "real tree remains byte-identical"
        );

        ws.remove().await;
        assert_no_candidate_worktrees(&root);
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn a_mid_run_user_edit_fails_adoption_atomically_naming_the_path() {
        let root = scaffold("conflict");
        let port = GitCandidateWorkspaces::new(
            root.clone(),
            RegistryOptions::default(),
            Default::default(),
            Vec::new(),
            crate::rules::ResolvedRules::default(),
        );
        let ws = port.create_workspace().await.unwrap();

        std::fs::write(ws.dir().join("tracked.txt"), "base\ndirty\ncandidate\n").unwrap();
        std::fs::write(ws.dir().join("second.txt"), "must not land\n").unwrap();
        ws.seal().await.unwrap();
        // The user edits the same file while the candidate runs.
        std::fs::write(root.join("tracked.txt"), "base\nuser-edit\n").unwrap();

        match ws.adopt(&[]).await.unwrap_err() {
            WorkspaceError::Adopt {
                paths, workspace, ..
            } => {
                assert!(
                    paths.iter().any(|p| p.contains("tracked.txt")),
                    "the conflict must name the path: {paths:?}"
                );
                assert_eq!(workspace, ws.dir().display().to_string());
            }
            other => panic!("expected an adoption conflict, got {other:?}"),
        }
        // Atomicity: NOTHING was applied — not even the conflict-free file.
        assert_eq!(read(&root.join("tracked.txt")), "base\nuser-edit\n");
        assert!(
            !root.join("second.txt").exists(),
            "a rejected adoption must not half-apply"
        );
        // The workspace is preserved for recovery until removed explicitly.
        assert!(ws.dir().exists());

        ws.remove().await;
        assert_no_candidate_worktrees(&root);
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn a_repo_without_commits_is_a_clean_snapshot_error() {
        let root = std::env::temp_dir().join(format!(
            "stella_cwstest_nocommit_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        scratch_git(&root, &["init", "-q"]);
        let port = GitCandidateWorkspaces::new(
            root.clone(),
            RegistryOptions::default(),
            Default::default(),
            Vec::new(),
            crate::rules::ResolvedRules::default(),
        );
        match port.create_workspace().await {
            Err(WorkspaceError::Snapshot { reason }) => {
                assert!(reason.contains("at least one commit"), "{reason}")
            }
            Err(other) => panic!("expected a snapshot error, got {other:?}"),
            Ok(_) => panic!("expected a snapshot error, got a workspace"),
        }
        assert_no_candidate_worktrees(&root);
        std::fs::remove_dir_all(&root).ok();
    }
}
