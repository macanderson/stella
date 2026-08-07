//! Tool-registry options and workspace port adapters.
//!
//! `registry_options` translates the host/media half of settings into
//! `RegistryOptions`. Tool on/off policy no longer rides here — it is
//! enforced above the whole session stack by `PolicyToolSet` (see
//! `session_tool_policy`). The rest are the pipeline's filesystem/VCS/command
//! ports.

use super::*;
// `DiagnosticInvocation`/`DiagnosticRunner` are deliberately absent: the impl
// that used them moved to `super::diagnostics`, and only this file's tests
// still name them. They are imported per test module rather than here so a
// non-test build does not carry an unused import.
use stella_pipeline::{ArtifactIdentity, ArtifactKind, CmdKind, TestInvocation, TestRunner};

/// Apply the cross-crate policy shared by every model/repository-controlled
/// subprocess. Kept as a named seam so the CLI's pipeline-only spawns have a
/// direct regression test rather than relying only on stella-tools tests.
pub(super) fn scrub_model_subprocess(command: &mut tokio::process::Command) {
    stella_tools::subprocess_env::scrub_sensitive_env(command);
}

/// The single filesystem-isolation seam for developer script-tool discovery.
/// The session stack goes through this report; candidate workspaces still
/// call `discover_in_scopes` directly (see `workspace_ports`) and therefore
/// miss the isolation gate.
#[cfg(test)]
pub(crate) fn custom_tool_report_for_workspace(
    root: &std::path::Path,
) -> stella_tools::custom::DiscoveryReport {
    crate::tool_foundry::adopt::gate_discovery(custom_tool_report_for_scopes(root, true), root)
}

/// Discover only the custom-tool scopes permitted by the current authority.
/// Filesystem-isolated benchmark runs omit both workspace and user-global
/// executable extensions regardless of the ordinary authority policy.
pub(crate) fn custom_tool_report_for_scopes(
    root: &std::path::Path,
    include_workspace: bool,
) -> stella_tools::custom::UngatedDiscovery {
    if crate::settings::filesystem_settings_disabled() {
        stella_tools::custom::UngatedDiscovery::default()
    } else {
        let home = crate::paths::user_extension_home();
        stella_tools::custom::discover_in_scopes(root, home.as_deref(), include_workspace)
    }
}

/// Repo-structure summary for the planner's split context: the `git
/// ls-files` tree plus, when a code-graph index exists, the graph-derived
/// orientation complement (languages, entry points, storage relations) —
/// so the plan names the relevant files instead of leaving the worker to
/// rediscover them (#342 seam 2).
pub(crate) struct GitRepoStructure {
    pub(crate) root: std::path::PathBuf,
}

/// Compose the planner's structure summary from the raw file tree and the
/// optional graph-derived orientation block. Pure so the composition is
/// directly testable: no orientation (no index yet) leaves the tree
/// byte-identical, and an empty tree (not a git repo) still surfaces the
/// orientation alone.
fn compose_structure_summary(tree: String, orientation: Option<String>) -> String {
    match orientation {
        Some(block) if tree.is_empty() => block,
        Some(block) => format!("{tree}\n\n{block}"),
        None => tree,
    }
}

#[async_trait::async_trait]
impl RepoStructurePort for GitRepoStructure {
    async fn structure_summary(&self) -> String {
        let mut cmd = tokio::process::Command::new("git");
        cmd.args(["ls-files"]).current_dir(&self.root);
        // Hook-exported GIT_* vars must not re-target this at another repo.
        for var in stella_tools::exec::GIT_REPO_ENV_VARS {
            cmd.env_remove(var);
        }
        scrub_model_subprocess(&mut cmd);
        let output = cmd.output().await;
        let tree = match output {
            Ok(out) if out.status.success() => {
                render_file_tree(&String::from_utf8_lossy(&out.stdout), 200)
            }
            _ => String::new(),
        };
        // Read-only like the system-prompt seam: renders only from an
        // EXISTING index (never builds one inline), is bounded and
        // byte-stable for a given index state, and simply appears once the
        // session's background graph build has landed.
        compose_structure_summary(
            tree,
            stella_tools::overview::render_orientation_block(&self.root),
        )
    }
}

/// Untracked-file fingerprints for the pipeline's zero-diff guard. Unlike the
/// pipeline's diagnostic runner (whose output is truncated), this captures the
/// COMPLETE `git ls-files --others` listing and fingerprints each file itself
/// (in-process, with real filesystem access), so a large untracked set is not
/// silently clipped and a modification to an already-untracked file is
/// detectable (its complete content hash changes).
pub(crate) struct GitRepoStatus {
    pub(crate) root: std::path::PathBuf,
}

#[async_trait::async_trait]
impl RepoStatusPort for GitRepoStatus {
    async fn untracked_fingerprints(&self) -> std::collections::HashMap<String, String> {
        let mut out = std::collections::HashMap::new();
        // `-z` NUL-delimits paths (robust to spaces/newlines); quotePath off
        // keeps non-ASCII literal. Full stdout is read — never truncated.
        let mut cmd = tokio::process::Command::new("git");
        cmd.args([
            "-c",
            "core.quotePath=false",
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .current_dir(&self.root);
        // Hook-exported GIT_* vars must not re-target this at another repo.
        for var in stella_tools::exec::GIT_REPO_ENV_VARS {
            cmd.env_remove(var);
        }
        scrub_model_subprocess(&mut cmd);
        let output = cmd.output().await;
        let Ok(listing) = output else {
            return out;
        };
        if !listing.status.success() {
            return out; // not a git repo, or git unavailable
        }
        for rel in String::from_utf8_lossy(&listing.stdout)
            .split('\0')
            .filter(|p| !p.is_empty())
        {
            // Unreadable metadata → a sentinel so the file still registers
            // as present.
            let fingerprint =
                fs_fingerprint(&self.root.join(rel)).unwrap_or_else(|| "unreadable".to_string());
            out.insert(rel.to_string(), fingerprint);
        }
        out
    }

    async fn tracked_fingerprints(&self) -> std::collections::HashMap<String, String> {
        let mut out = std::collections::HashMap::new();
        let mut cmd = tokio::process::Command::new("git");
        scrub_model_subprocess(&mut cmd);
        cmd.args([
            "-c",
            "core.quotePath=false",
            "diff",
            "--name-only",
            "--relative",
            "-z",
            "HEAD",
            "--",
        ])
        .current_dir(&self.root);
        for var in stella_tools::exec::GIT_REPO_ENV_VARS {
            cmd.env_remove(var);
        }
        let Ok(listing) = cmd.output().await else {
            return out;
        };
        if !listing.status.success() {
            return out;
        }
        for rel in String::from_utf8_lossy(&listing.stdout)
            .split('\0')
            .filter(|p| !p.is_empty())
        {
            let fingerprint =
                fs_fingerprint(&self.root.join(rel)).unwrap_or_else(|| "deleted".to_string());
            out.insert(rel.to_string(), fingerprint);
        }
        out
    }

    async fn artifact_identity(&self, path: &str) -> Option<stella_pipeline::ArtifactIdentity> {
        fs_artifact_identity(&self.root, path)
    }
}

/// The pipeline's file fingerprint: SHA-256 over the complete bytes. Content
/// hashes are required at the witness authority boundary: size+mtime can be
/// restored after a same-length edit and would incorrectly credit a tampered
/// witness. One definition is shared with candidate snapshots.
pub(crate) fn fs_fingerprint(path: &std::path::Path) -> Option<String> {
    Some(
        OpenedWitnessArtifact::open(path)?
            .identity_for_path(path)?
            .fingerprint,
    )
}

/// Identity for the workspace-relative `rel` under `root`, attesting the
/// location the artifact was actually observed at: `path` carries the opened
/// file's canonical position relative to the canonical root. A witness that
/// was renamed and is still reachable through an aliased lookup (a
/// case-folding filesystem, a symlinked parent directory) therefore reports
/// its real location, which the pipeline's pinned-path equality rejects as
/// tampering. A file whose canonical position cannot be stated inside `root`
/// has no identity at all — fail closed, exactly like a symlink.
pub(crate) fn fs_artifact_identity(
    root: &std::path::Path,
    rel: &str,
) -> Option<stella_pipeline::ArtifactIdentity> {
    let full = root.join(rel);
    let identity = OpenedWitnessArtifact::open(&full)?.identity_for_path(&full)?;
    Some(ArtifactIdentity {
        path: observed_relative_path(root, &full)?,
        ..identity
    })
}

/// The canonical position of `full` relative to the canonical `root`, in the
/// repo-relative `/`-separated form the pipeline pins witness paths in.
fn observed_relative_path(root: &std::path::Path, full: &std::path::Path) -> Option<String> {
    let canonical_root = std::fs::canonicalize(root).ok()?;
    let observed = std::fs::canonicalize(full).ok()?;
    let rel = observed.strip_prefix(&canonical_root).ok()?;
    let mut out = String::new();
    for component in rel.components() {
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(component.as_os_str().to_str()?);
    }
    (!out.is_empty()).then_some(out)
}

struct OpenedWitnessArtifact {
    file: std::fs::File,
    metadata: std::fs::Metadata,
}

impl OpenedWitnessArtifact {
    fn open(path: &std::path::Path) -> Option<Self> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_FLAG_OPEN_REPARSE_POINT opens a link/reparse point itself
            // instead of following it. Link count is unavailable through a
            // stable std API, so Windows still fails closed below.
            options.custom_flags(0x0020_0000);
        }
        let file = options.open(path).ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.file_type().is_file() || opened_metadata(&metadata).is_none() {
            return None;
        }
        Some(Self { file, metadata })
    }

    fn identity_for_path(mut self, path: &std::path::Path) -> Option<ArtifactIdentity> {
        use std::fmt::Write as _;
        use std::io::Read as _;

        use sha2::{Digest, Sha256};

        let (mode, link_count) = opened_metadata(&self.metadata)?;
        if link_count != 1 || !path_resolves_to_opened_file(path, &self.metadata) {
            return None;
        }
        let mut payload = Vec::new();
        self.file.read_to_end(&mut payload).ok()?;
        let final_metadata = self.file.metadata().ok()?;
        if opened_metadata(&final_metadata) != Some((mode, link_count))
            || !path_resolves_to_opened_file(path, &final_metadata)
        {
            return None;
        }
        let mut hasher = Sha256::new();
        hasher.update(b"regular");
        hasher.update(mode.to_le_bytes());
        hasher.update(link_count.to_le_bytes());
        hasher.update(payload);
        let mut fingerprint = String::from("sha256:");
        for byte in hasher.finalize() {
            write!(&mut fingerprint, "{byte:02x}").ok()?;
        }
        Some(ArtifactIdentity {
            // The observed location is attested by `fs_artifact_identity`,
            // which knows the workspace root. Left empty here, a bare content
            // identity can never satisfy the pipeline's pinned-path equality.
            path: String::new(),
            fingerprint,
            kind: ArtifactKind::Regular,
            mode,
            link_count,
        })
    }
}

#[cfg(unix)]
fn opened_metadata(metadata: &std::fs::Metadata) -> Option<(u32, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.mode(), metadata.nlink()))
}

#[cfg(not(unix))]
fn opened_metadata(_metadata: &std::fs::Metadata) -> Option<(u32, u64)> {
    // Stable Rust does not expose a by-handle link count on Windows. Never
    // manufacture `1`: without proof that no hardlink aliases exist, witness
    // identity is unavailable and acceptance fails closed.
    None
}

#[cfg(unix)]
fn path_resolves_to_opened_file(path: &std::path::Path, opened: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    let Ok(current) = std::fs::symlink_metadata(path) else {
        return false;
    };
    current.file_type().is_file() && current.dev() == opened.dev() && current.ino() == opened.ino()
}

#[cfg(not(unix))]
fn path_resolves_to_opened_file(_path: &std::path::Path, _opened: &std::fs::Metadata) -> bool {
    false
}

/// The workspace-rooted pipeline ports every session driver constructs the
/// same way — repo structure/status, the verification command runner, and
/// best-of-N candidate isolation, all rooted at the same tree. One bundle
/// and one constructor so the four drivers (one-shot, goal loop, deck,
/// fleet worker) can never drift apart on the wiring.
pub(crate) struct WorkspacePorts {
    pub(crate) repo_structure: GitRepoStructure,
    pub(crate) repo_status: GitRepoStatus,
    pub(crate) diagnostic_runner: GitDiagnosticRunner,
    pub(crate) lint_probe: ToolchainLintProbe,
    /// The witness mutation check (#870), rooted at the session tree.
    pub(crate) mutation_probe: FsMutationProbe,
    /// The diff-coverage check (#1291), rooted at the session tree. Spawns
    /// nothing until a fast-submit is imminent, and answers "unmeasured" for
    /// every dialect it cannot instrument.
    pub(crate) coverage_probe: super::coverage::ToolchainCoverageProbe,
    pub(crate) test_runner: TypedTestRunner,
    /// Used for best-of-N and for candidate-local authored witnesses at N=1.
    pub(crate) candidate_workspaces: crate::candidate_ws::GitCandidateWorkspaces,
    /// The orchestrator's Best-of-N MCP pre-fetch adapter (issue #248
    /// Phase 1), sharing the same MCP toolset threaded into
    /// `candidate_workspaces` — `None` when the session has no MCP
    /// servers connected. Inert unless `candidates > 1`, same as above.
    pub(crate) mcp_prefetch: Option<crate::candidate_ws::McpPrefetchAdapter>,
}

/// The verification ladder's view of the session file recorder's own mutation
/// tally ([`stella_tools::ToolRegistry::mutations_recorded`]).
///
/// Deliberately points at the **same** `ToolRegistry` the driver calls
/// `attach_events` on, and is constructed beside that call for exactly that
/// reason. The bug this closes was two numbers describing one event from two
/// wires: `record_touch` announced six changes down the session channel while
/// the pipeline, counting on a sender it had handed the *engine*, told its
/// verifier `file_change_events=0` — and the verifier reported the file as probably
/// absent while it sat in the container (#973).
///
/// Not part of [`WorkspacePorts`]: that bundle is rooted at a path, and this is
/// bound to a live registry instance. Folding it in would invite a future
/// caller to build the bundle from one registry and run the turn on another,
/// which is the very confusion being removed.
pub(crate) struct RegistryTouches<'a>(pub(crate) &'a stella_tools::ToolRegistry);

impl stella_pipeline::FileTouchPort for RegistryTouches<'_> {
    fn mutations_recorded(&self) -> u64 {
        self.0.mutations_recorded()
    }

    /// Rendered here rather than borrowed, because the two crates on either
    /// side of this bridge do not depend on each other: the tool crate owns the
    /// ledger, the pipeline crate owns the question, and this is the only place
    /// that can see both.
    fn authored_diff(&self) -> stella_pipeline::AuthoredChange {
        let rendered = self.0.authored_diff();
        stella_pipeline::AuthoredChange {
            text: rendered.text,
            lines: rendered.lines,
        }
    }

    fn begin_workspace_probe(&self) {
        self.0.begin_workspace_probe();
    }

    fn settle_workspace_probe(&self) {
        self.0.settle_workspace_probe();
    }
}

/// Build the [`WorkspacePorts`] bundle rooted at `root` (the session
/// workspace, or a fleet worker's own worktree). `mcp`, when the caller has
/// one connected, is shared into both the candidate tool surface
/// (`candidate_safe`-filtered) and the orchestrator pre-fetch hook — the
/// same live connections, no new subprocess (issue #248 Phase 1).
pub(crate) fn workspace_ports(
    root: std::path::PathBuf,
    cfg: &Config,
    registry_options: stella_tools::RegistryOptions,
    active_rules: crate::rules::ResolvedRules,
    mcp: Option<Arc<stella_mcp::McpToolSet>>,
    events: Option<stella_core::EventSender>,
) -> Result<WorkspacePorts, String> {
    crate::enterprise_telemetry::authorize_execution_surface(
        crate::enterprise_telemetry::ExecutionSurface::WorkspacePorts,
    )?;
    crate::enterprise_telemetry::authorize_execution_surface(
        crate::enterprise_telemetry::ExecutionSurface::CandidateWorkspace,
    )?;
    // The candidate registry mirrors the session's custom tool surface —
    // discovered from the same root, so a candidate sees exactly the custom
    // tools the session does (re-rooted at its snapshot at create time).
    let home = crate::paths::home();
    let custom_tools = crate::tool_foundry::adopt::gate_discovery(
        stella_tools::custom::discover_in_scopes(
            &root,
            home.as_deref(),
            cfg.authority.project_custom_tools_allowed,
        ),
        &root,
    )
    .tools;
    let mut candidate_workspaces = crate::candidate_ws::GitCandidateWorkspaces::new(
        root.clone(),
        registry_options,
        session_tool_policy(cfg),
        custom_tools,
        active_rules,
    );
    if let Some(mcp) = &mcp {
        candidate_workspaces = candidate_workspaces.with_candidate_mcp(Arc::clone(mcp));
    }
    if let Some(events) = events {
        candidate_workspaces = candidate_workspaces.with_events(events);
    }
    Ok(WorkspacePorts {
        repo_structure: GitRepoStructure { root: root.clone() },
        repo_status: GitRepoStatus { root: root.clone() },
        diagnostic_runner: GitDiagnosticRunner::new(root.clone()),
        lint_probe: ToolchainLintProbe { root: root.clone() },
        mutation_probe: FsMutationProbe { root: root.clone() },
        coverage_probe: super::coverage::ToolchainCoverageProbe { root: root.clone() },
        test_runner: TypedTestRunner { root },
        candidate_workspaces,
        mcp_prefetch: mcp.map(crate::candidate_ws::McpPrefetchAdapter::new),
    })
}

/// The regression-veto lint probe (#861): the workspace's own diagnostics
/// plan (cargo check / tsc / eslint / ruff — closed vocabulary, fixed argv),
/// run at a caller-chosen root and returned as parsed, root-relative
/// records. The `root` override is what lets one probe serve the session
/// tree and every isolated candidate worktree.
pub(crate) struct ToolchainLintProbe {
    pub(crate) root: std::path::PathBuf,
}

/// Bounded like the diagnostics tool itself, well under the pipeline's own
/// patience: a lint pass that needs longer than this is not a cheap veto
/// probe any more.
const LINT_PROBE_TIMEOUT_SECS: u64 = 240;

/// The witness mutation check's host half (#870): apply one single-line
/// mutant IN PLACE, run the witness invocation, and restore the original
/// bytes. In-place with restore — instead of a scratch copy — because the
/// tree at `root` is the sealed candidate itself and a full copy per mutant
/// would dwarf the cost of the check; the restore is byte-exact and its
/// failure is reported as `TreePoisoned` so the pipeline fails the candidate
/// closed rather than shipping a mutated tree.
pub(crate) struct FsMutationProbe {
    pub(crate) root: std::path::PathBuf,
}

#[async_trait::async_trait]
impl stella_pipeline::MutationProbe for FsMutationProbe {
    async fn run_mutant(
        &self,
        root: Option<&str>,
        mutation: &stella_pipeline::LineMutation,
        invocation: &stella_pipeline::TestInvocation,
    ) -> stella_pipeline::MutantOutcome {
        use stella_pipeline::MutantOutcome;
        let root = root
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| self.root.clone());
        let file = root.join(&mutation.path);
        let Ok(original) = tokio::fs::read_to_string(&file).await else {
            return MutantOutcome::Unavailable;
        };
        // The mutant applies to ONE known line whose content must still
        // match the diff it was proposed from — a drifted line means we
        // would be breaking something other than the candidate's change.
        let mut lines: Vec<&str> = original.split_inclusive('\n').collect();
        let index = (mutation.line as usize).saturating_sub(1);
        let Some(line) = lines.get(index) else {
            return MutantOutcome::Unavailable;
        };
        if line.trim_end_matches(['\n', '\r']) != mutation.original {
            return MutantOutcome::Unavailable;
        }
        let newline = &line[line.trim_end_matches(['\n', '\r']).len()..];
        let mutated_line = format!("{}{newline}", mutation.mutated);
        lines[index] = &mutated_line;
        let mutated = lines.concat();
        if tokio::fs::write(&file, &mutated).await.is_err() {
            // Nothing was changed; the check simply cannot run here.
            return MutantOutcome::Unavailable;
        }
        let outcome = run_command(test_process(invocation, &root)).await;
        // Restore is the safety-critical half: the tree must end
        // byte-identical to the sealed candidate. Retry before declaring
        // the tree poisoned.
        let mut restored = false;
        for _ in 0..3 {
            if tokio::fs::write(&file, &original).await.is_ok() {
                restored = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        if !restored {
            return MutantOutcome::TreePoisoned;
        }
        match outcome.assertion_result() {
            Some(passed) => MutantOutcome::Witness { passed },
            // A timed-out mutant run observed nothing about the witness.
            None => MutantOutcome::Unavailable,
        }
    }
}

#[async_trait::async_trait]
impl stella_pipeline::LintProbe for ToolchainLintProbe {
    async fn snapshot(&self, root: Option<&str>) -> Option<Vec<stella_pipeline::LintRecord>> {
        let root = root
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| self.root.clone());
        let records = stella_tools::diagnostics::snapshot(&root, LINT_PROBE_TIMEOUT_SECS).await?;
        Some(
            records
                .into_iter()
                .map(|d| stella_pipeline::LintRecord {
                    file: d.file,
                    error: d.severity == stella_tools::diagnostics::Severity::Error,
                    code: d.code,
                    message: d.message,
                })
                .collect(),
        )
    }
}

/// Workspace-rooted closed Git diagnostics. Every variant maps to fixed argv;
/// paths remain literal arguments and no shell is involved.
pub(crate) struct GitDiagnosticRunner {
    pub(crate) root: std::path::PathBuf,
    /// The commit the session started on, resolved eagerly at construction.
    ///
    /// A bare `git diff` reports only unstaged working-tree changes, so an
    /// agent that *commits* its work — with `repo_commit`, a tool this very
    /// registry ships — leaves a clean tree and reads as having changed
    /// nothing. Verification then tells it "no changes were made to the
    /// repository" while the files sit on disk, and the honest conclusion
    /// available to the model is that its work was lost. Diffing against the
    /// starting commit instead counts staged, unstaged, and committed work
    /// alike. `None` means no resolvable HEAD (an empty or non-git tree), in
    /// which case the plain working-tree diff is already the whole truth.
    pub(crate) baseline: Option<String>,
}

impl GitDiagnosticRunner {
    /// Resolve the baseline NOW, at session/candidate setup — not on the
    /// first diff.
    ///
    /// Every `GitDiff` runs inside `gather_diff`, which happens *after*
    /// execute. Resolving lazily there would read HEAD once the agent had
    /// already committed, making the baseline the agent's own commit and the
    /// diff empty again — reintroducing exactly the bug this fixes, while a
    /// test that captured early would still pass.
    pub(crate) fn new(root: std::path::PathBuf) -> Self {
        let baseline = resolve_head(&root);
        Self { root, baseline }
    }

    pub(super) fn baseline_commit(&self) -> Option<&str> {
        self.baseline.as_deref()
    }
}

/// `git rev-parse HEAD`, or `None` when there is no resolvable commit (an
/// empty or non-git tree) — where the working-tree diff is already the whole
/// truth.
fn resolve_head(root: &std::path::Path) -> Option<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(["rev-parse", "HEAD"]).current_dir(root);
    for var in stella_tools::exec::GIT_REPO_ENV_VARS {
        cmd.env_remove(var);
    }
    // Same credential-scrub policy as every other git spawn in this file —
    // this was the one site that skipped it (sync `Command`, so the std form).
    stella_tools::exec::scrub_sensitive_std_env(&mut cmd);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // 40 hex chars for a SHA-1 repo, 64 for a `--object-format=sha256` one —
    // rejecting the latter silently degraded the diff baseline to a bare
    // working-tree diff on sha256 repos, reintroducing the "committed work
    // reads as no changes" bug this baseline exists to prevent.
    let valid_len = matches!(sha.len(), 40 | 64);
    (valid_len && sha.chars().all(|c| c.is_ascii_hexdigit())).then_some(sha)
}

/// Workspace-rooted typed test runner. It passes an enumerable argv directly
/// to the OS and never invokes a shell.
pub(crate) struct TypedTestRunner {
    pub(crate) root: std::path::PathBuf,
}

#[async_trait::async_trait]
impl TestRunner for TypedTestRunner {
    async fn run_test(&self, invocation: &TestInvocation) -> CmdOutcome {
        run_command(test_process(invocation, &self.root)).await
    }

    async fn runner_available(&self, probe: &TestInvocation) -> bool {
        let outcome = run_command(test_process(probe, &self.root)).await;
        outcome.kind == CmdKind::Completed && outcome.exit_code == 0
    }
}

fn test_process(invocation: &TestInvocation, root: &std::path::Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(&invocation.program);
    cmd.args(&invocation.args)
        .current_dir(root)
        .env("PWD", root);
    for var in stella_tools::exec::GIT_REPO_ENV_VARS {
        cmd.env_remove(var);
    }
    scrub_model_subprocess(&mut cmd);
    cmd
}

pub(super) async fn run_command(mut cmd: tokio::process::Command) -> CmdOutcome {
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    // Cancellation drops this future without unwinding into the timeout arm
    // below, and a `setsid` child cannot be reached by the terminal's own
    // signals — so a cancelled turn used to leave a full-length test/diagnostic
    // command (up to the 300s bound) running unattended. Reaping the direct
    // child on drop is the same discipline `candidate_ws::git_stdout_to_file`
    // already applies. Grandchildren still outlive it; only the timeout arm
    // signals the whole process group.
    cmd.kill_on_drop(true);
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return CmdOutcome {
                exit_code: -1,
                stdout_tail: String::new(),
                stderr_tail: format!("failed to spawn: {e}"),
                kind: CmdKind::Infra,
            };
        }
    };
    #[cfg(unix)]
    let pid = child.id().unwrap_or(0) as i32;
    // Cancellation backstop, the same guard every other `pre_exec(setsid)`
    // spawn site in the workspace uses (`stella_tools::exec::GroupKillGuard`)
    // rather than a second copy of the shape. The child is in its OWN session,
    // so Ctrl-C's SIGINT never reaches it: without this, dropping the pipeline
    // future mid-test (Esc, a signal, a `select!` losing the race) left a
    // whole `cargo test`/`git diff` tree running against the workspace after
    // the user believed the run had stopped.
    #[cfg(unix)]
    let mut guard = stella_tools::exec::GroupKillGuard::arm(pid);

    let timeout = Duration::from_secs(300);
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            #[cfg(unix)]
            guard.disarm();
            output
        }
        // Wait failure leaves the child's state unknown — the still-armed
        // guard kills the group on return rather than leak it.
        Ok(Err(e)) => {
            return CmdOutcome {
                exit_code: -1,
                stdout_tail: String::new(),
                stderr_tail: format!("command failed: {e}"),
                kind: CmdKind::Infra,
            };
        }
        Err(_) => {
            #[cfg(unix)]
            guard.kill_now();
            return CmdOutcome {
                exit_code: -1,
                stdout_tail: String::new(),
                stderr_tail: format!("command timed out after {}s", timeout.as_secs()),
                kind: CmdKind::TimedOut,
            };
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout_tail = truncate_tail(&stdout, 100_000);
    let stderr_tail = truncate_tail(&stderr, 20_000);
    // #1294: the runner is the only party that can see a Unix termination
    // signal — past this point the outcome carries an `exit_code` and the
    // fact is gone. Classifying here is what lets the pipeline tell "the
    // machine ran out of memory" (nothing was learned; retry) from "the test
    // failed" (the code is wrong; revise), rather than reporting one as the
    // other.
    let kind = out_of_memory_kind(&output.status, &stdout_tail, &stderr_tail);
    CmdOutcome {
        exit_code: output.status.code().unwrap_or(-1),
        stdout_tail,
        stderr_tail,
        kind,
    }
}

/// Classify a finished process as an out-of-memory kill or an ordinary
/// completed run (#1294).
///
/// Both tails are scanned, joined: a runtime that dies of allocation failure
/// may say so on either stream (`cargo` on stderr, a Node harness on stdout),
/// and reading only one would make detection depend on which runner happened
/// to be in use. The signal — the strongest evidence and the only one that is
/// not text — is readable exclusively here; a Windows host reports no signal,
/// so the marker rule is the whole of the detection there.
fn out_of_memory_kind(
    status: &std::process::ExitStatus,
    stdout_tail: &str,
    stderr_tail: &str,
) -> CmdKind {
    #[cfg(unix)]
    let signal = std::os::unix::process::ExitStatusExt::signal(status);
    #[cfg(not(unix))]
    let signal = None;
    let output = format!("{stdout_tail}\n{stderr_tail}");
    let facts = stella_pipeline::ExitFacts {
        signal,
        exit_code: status.code(),
        output: &output,
    };
    if stella_pipeline::killed_by_oom(facts) {
        CmdKind::OutOfMemory
    } else {
        CmdKind::Completed
    }
}

fn truncate_tail(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let start = s.len() - max_bytes;
    let mut idx = start;
    while !s.is_char_boundary(idx) {
        idx += 1;
    }
    s[idx..].to_string()
}

/// The registry's construction inputs for this session's config: host
/// attestations and media prerequisites, and nothing else.
///
/// It used to also carry `bash`/`web` booleans translated from settings —
/// operator policy applied at construction, which covered built-ins and
/// nothing else. Policy now travels as [`Config::tool_policy`] and is enforced
/// once, above the entire session tool stack, by
/// [`crate::agent::PolicyToolSet`]; see [`session_tool_policy`] for the
/// accompanying half every session driver pairs with this call.
pub(crate) fn registry_options(cfg: &Config) -> stella_tools::RegistryOptions {
    let process_free = crate::enterprise_telemetry::process_free_authority_active();
    let media_operation_journal = host_media_operation_journal(&cfg.workspace_root);
    stella_tools::RegistryOptions {
        // The one place the CLI opts into probing this host (#1596).
        issue_backend: stella_tools::IssueBackendSource::ambient(),
        media_requires_host_approval: cfg.authority.media_requires_host_approval,
        media_operation_journal,
        media_host_data_isolation: process_free
            .then_some(stella_tools::media::HostDataIsolation::ProcessFree),
        ..Default::default()
    }
}

/// The session's tool policy — the other half of [`registry_options`].
///
/// Every session driver wraps its assembled tool stack in
/// [`crate::agent::PolicyToolSet`] with this, which is what makes a
/// `"tools"` entry cover built-ins, MCP tools, and customer-registered custom
/// tools identically. Resolved once in `Config::load_with_settings` (managed
/// ceiling already folded in), so this is a clone, not a re-derivation — there
/// is no second place that could disagree about what is switched off.
pub(crate) fn session_tool_policy(cfg: &Config) -> stella_tools::policy::ToolPolicy {
    cfg.tool_policy.clone()
}

fn host_media_operation_journal(
    workspace_root: &std::path::Path,
) -> Option<Arc<dyn stella_media::MediaOperationJournal>> {
    let workspace_root = workspace_root.canonicalize().ok()?;
    let data_dir = std::path::absolute(crate::paths::data_dir()).ok()?;
    if data_dir.starts_with(&workspace_root) {
        return None;
    }
    stella_media::SqliteMediaOperationJournal::open_outside(
        data_dir.join("media-operations.db"),
        workspace_root,
        Default::default(),
    )
    .ok()
    .map(|journal| Arc::new(journal) as Arc<dyn stella_media::MediaOperationJournal>)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod benchmark_tests;

#[cfg(test)]
mod diff_baseline_tests;
