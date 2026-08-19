//! The pipeline's port boundary. Like
//! `stella-core`, `stella-pipeline` never imports a provider SDK, a shell, a
//! context store, or a terminal — it orchestrates the staged turn flow
//! entirely through the traits defined here. The
//! `stella-cli` glue layer implements each of them against the real
//! subsystems (`stella-model`, `stella-tools`, `stella-context`, the TUI's
//! approval prompt); tests implement them with scripted doubles.
//!
//! Why these live here and not in `stella-core`: the engine (`stella-core`)
//! drives *one turn* through `Provider`/`ToolExecutor`. The pipeline sits
//! *above* the engine and needs three things the engine deliberately does
//! not: a way to pick which provider a role resolved to
//! ([`ProviderResolver`]), the surrounding context/repo material that shapes
//! triage and planning ([`ContextRecallPort`], [`RepoStructurePort`]), and
//! the deterministic verification substrate ([`TestRunner`], [`DiagnosticRunner`],
//! [`ApprovalGate`]). None of these belong inside the step-driver.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use stella_core::Router;
use stella_core::hooks::{HookRunner, Hooks};
use stella_core::retry::Sleeper;
use stella_protocol::{ContextUsage, ModelRef, Provider, ScopeProposal};

/// Candidate isolation — see [`workspace`]. Re-exported below so every
/// `ports::CandidateWorkspace` path in the crate still resolves.
pub mod workspace;

/// The serializable half of candidate isolation — see [`handle`]. The six
/// operations of [`workspace::CandidateWorkspace`]'s nineteen that a caller in
/// another process can address, and the host-side fence that bounds them
/// (#3380, `doc:pipeline-as-plugins` §4 A10).
pub mod handle;

pub use handle::{
    CandidateHandles, CandidateOpError, HOST_TREE_HANDLE, host_tree_grant, resolve_in_root,
    test_plan,
};
pub use workspace::{AdoptedChange, CandidateWorkspace, CandidateWorkspacePort, WorkspaceError};

/// Maps a router-resolved [`ModelRef`] to the concrete provider adapter that
/// serves it. This is the one seam that connects `stella-core`'s
/// catalog-free [`stella_core::Router`] (which only ever produces *data* — a
/// `ModelRef`) to `stella-model`'s concrete adapters, without either crate
/// depending on the other. The CLI glue owns the adapter set and answers
/// this query; the pipeline never constructs an adapter.
///
/// Returning `&dyn Provider` (a borrow, not an owned box) keeps the adapter
/// set owned by the caller for the pipeline run's lifetime — exactly how
/// `stella-core::Engine` already borrows its `&dyn Provider`.
pub trait ProviderResolver: Send + Sync {
    /// The provider adapter that will serve `model`, or `None` if no
    /// configured adapter matches (a resolution the glue reports as a hard
    /// error — never a silent fallback; L-M1).
    fn provider_for(&self, model: &ModelRef) -> Option<&dyn Provider>;
}

/// [`RecalledFrame`], [`ContextRecallPort`], and [`Recall`] moved to
/// `stella_protocol::recall` (removal census for `stella-pipeline`,
/// `docs/spec/pipeline-as-plugins.md` §7 slice 1) — every door's recall
/// injection depended on them regardless of which pipeline choice ran, so
/// they were a boundary type living here only because this was where they
/// were first needed. Re-exported so every `crate::ports::RecalledFrame`
/// (etc.) path in this crate still resolves unchanged.
pub use stella_protocol::{ContextRecallPort, Recall, RecalledFrame};

/// A repository-structure summary for the planner's **split context** (L-E6):
/// the planner receives goal + recall + this structure summary, never the
/// accumulated transcript. Kept a plain string (a tree/outline the glue
/// renders) so the pipeline stays agnostic to how structure is computed
/// (`stella-graph` code index, a `git ls-files` outline, etc.). A caller
/// with nothing to offer supplies [`NoRepoStructure`].
#[async_trait]
pub trait RepoStructurePort: Send + Sync {
    /// A bounded, human-readable summary of the repo's structure for
    /// planning. Empty string is valid (plan from goal + recall alone).
    async fn structure_summary(&self) -> String;
}

/// A snapshot of the working tree's untracked (git-ignored-aware) files, each
/// with a content fingerprint. The verification ladder diffs a before-turn and
/// after-turn snapshot to find files the turn **created or modified**, since
/// `git diff` alone is blind to untracked files.
///
/// Distinct from [`DiagnosticRunner`] on purpose: the listing must be COMPLETE —
/// diagnostic output is middle-out truncated (L-S3), so a large untracked
/// set would lose files and corrupt the diff-size accounting — and it must
/// carry per-file fingerprints so a modification (not just a creation) to an
/// already-untracked file is visible. A caller without a git working tree
/// supplies [`NoRepoStatus`].
#[async_trait]
pub trait RepoStatusPort: Send + Sync {
    /// Untracked files as `path -> fingerprint`, where the fingerprint changes
    /// whenever the file's content does (a complete content hash). Complete and
    /// never truncated. Empty when the workspace is not a git repo or on any
    /// failure — the guard then rests on the tracked `git diff` alone.
    async fn untracked_fingerprints(&self) -> std::collections::HashMap<String, String>;

    /// Tracked paths currently changed from the workspace baseline, with a
    /// complete content fingerprint (or a deletion sentinel). Witness
    /// authoring compares before/after maps so existing production edits can
    /// never enter an accepted witness artifact.
    async fn tracked_fingerprints(&self) -> std::collections::HashMap<String, String> {
        std::collections::HashMap::new()
    }

    /// Filesystem identity for one repo-relative artifact, obtained without
    /// following symlinks. Witness acceptance requires a regular single-link
    /// file; callers without filesystem access return `None`.
    async fn artifact_identity(&self, _path: &str) -> Option<ArtifactIdentity> {
        None
    }
}

/// Filesystem object kind captured without following symlinks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    Regular,
    Symlink,
    Other,
}

/// Complete witness artifact identity. Accepted identities are regular,
/// single-link files whose fingerprint commits to bytes, type, mode, and link
/// count from one no-follow file handle — plus the workspace-relative path the
/// observation was actually made at, so a renamed artifact can never equal its
/// accepted baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactIdentity {
    /// The workspace-relative location the artifact was actually observed at,
    /// not the path the caller asked about. The two differ exactly when the
    /// lookup was aliased — a case-folding filesystem or a symlinked parent
    /// directory resolving a pinned path to a file that has since been moved —
    /// which is a rename, and a rename is tampering. Adapters that cannot
    /// attest a location must leave this empty: an empty path can never match
    /// an accepted one, so the identity fails closed.
    pub path: String,
    pub fingerprint: String,
    pub kind: ArtifactKind,
    pub mode: u32,
    pub link_count: u64,
}

impl ArtifactIdentity {
    pub fn is_regular_single_link(&self) -> bool {
        self.kind == ArtifactKind::Regular && self.link_count == 1
    }
}

/// How one local process run ended, as only the runner can know it (#860).
///
/// The runner is the sole party that can tell "the process ran and exited
/// non-zero" from "I killed it at my deadline" or "it never started" — after
/// the fact all three collapse into a non-zero `exit_code`. The distinction is
/// load-bearing for verification: a timed-out baseline is not a failing
/// assertion, and letting it lock the flip oracle onto a command that never
/// really failed manufactures a fake fail→pass flip when the candidate merely
/// runs faster.
/// Serializable alongside [`CmdOutcome`]: a handle-addressed `run_test`
/// (#3380 A10) answers an out-of-process caller with exactly this, so the
/// infra-vs-assertion distinction survives the crossing instead of collapsing
/// back into an exit code on the way out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CmdKind {
    /// The process ran to completion; `exit_code` is its real exit status.
    #[default]
    Completed,
    /// The runner killed the process at its own deadline. `exit_code` is
    /// synthetic, and nothing about the command's assertions was observed.
    TimedOut,
    /// The machine ran out of memory and the run died for it (#1294) — the
    /// kernel's OOM killer, or a runtime that noticed its own allocation
    /// failure first. See [`crate::oom`] for what is observable and what is
    /// deliberately not read.
    ///
    /// Its own variant rather than a shade of [`Self::Infra`] because the two
    /// call for opposite handling. An unspawnable toolchain will be
    /// unspawnable on every retry, so re-running it is spend with no
    /// information; a memory kill is exactly the outcome a human would simply
    /// *try again* (see `PipelineConfig::test_oom_retries`). Collapsing them
    /// would make one of the two behaviors wrong, and the evidence a verifier
    /// reads would say "infra_failure" where the honest word is
    /// "out_of_memory".
    OutOfMemory,
    /// The process never produced a real exit status — it could not be
    /// spawned (missing program/toolchain) or its wait failed.
    Infra,
}

/// The outcome of running one local process through a pipeline runner. Output
/// is pre-truncated by the runner (middle-out, L-S3) into head+tail tails —
/// the pipeline never needs the full stream, only exit status and enough
/// text to summarize evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CmdOutcome {
    /// Process exit code. `0` conventionally means success; any non-zero is
    /// a failure. A signal-killed process reports a conventional 128+n here
    /// (the runner's responsibility, L-L1). Synthetic (typically `-1`) when
    /// [`Self::kind`] is not [`CmdKind::Completed`].
    pub exit_code: i32,
    /// Truncated stdout tail (middle-out elision applied by the runner).
    pub stdout_tail: String,
    /// Truncated stderr tail.
    pub stderr_tail: String,
    /// Whether the process completed, timed out, or never ran (#860). Only
    /// the runner can classify this; everyone downstream must go through
    /// [`Self::assertion_result`] rather than re-deriving pass/fail from the
    /// exit code.
    pub kind: CmdKind,
}

/// One test process invocation after the untrusted command text has crossed
/// the pipeline's strict parser. `program` and `args` are passed directly to a
/// process builder; neither field is ever interpreted by a shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestInvocation {
    /// A known test runner executable (for example `cargo` or `pytest`).
    pub program: String,
    /// The exact argument vector supplied to the test runner.
    pub args: Vec<String>,
}

impl CmdOutcome {
    /// Whether the command succeeded (ran to completion with exit code 0).
    /// The single place the pipeline decides pass/fail for a command — never
    /// string-sniffing the output. A timed-out or unspawnable run never
    /// passed, whatever its synthetic exit code says.
    pub fn passed(&self) -> bool {
        self.kind == CmdKind::Completed && self.exit_code == 0
    }

    /// What this run says about the command's *assertions* (#860):
    /// `Some(true)` = ran and passed, `Some(false)` = ran and genuinely
    /// failed, `None` = infrastructure noise (timeout, missing toolchain) —
    /// the assertions were never observed.
    ///
    /// This is the only reading the flip oracle may consume. Feeding it a raw
    /// `!passed()` is exactly the bug this type exists to close: an infra
    /// failure would satisfy the oracle's `Failing` precondition and a later
    /// clean run would be credited as a verified fix.
    pub fn assertion_result(&self) -> Option<bool> {
        match self.kind {
            CmdKind::Completed => Some(self.exit_code == 0),
            CmdKind::TimedOut | CmdKind::OutOfMemory | CmdKind::Infra => None,
        }
    }

    /// A short label for evidence text when [`Self::assertion_result`] is
    /// `None`, so a verifier reads "the run timed out" instead of a bare
    /// failure it would treat as an assertion.
    pub fn infra_label(&self) -> Option<&'static str> {
        match self.kind {
            CmdKind::Completed => None,
            CmdKind::TimedOut => Some("timed_out"),
            CmdKind::OutOfMemory => Some("out_of_memory"),
            CmdKind::Infra => Some("infra_failure"),
        }
    }

    /// Whether this run died for want of memory (#1294) — the one
    /// unobservable outcome worth *retrying* rather than reporting, because
    /// re-running is exactly what a human would do by hand.
    ///
    /// Kept beside [`Self::infra_label`] rather than open-coded at the call
    /// sites so "which outcomes are retryable" stays one answer: a future
    /// variant that is also worth a retry joins it here, not in five
    /// pipeline stages.
    #[must_use]
    pub fn is_out_of_memory(&self) -> bool {
        self.kind == CmdKind::OutOfMemory
    }
}

/// Closed diagnostic vocabulary. Every variant maps to fixed executable argv;
/// no caller-provided shell string crosses this boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticInvocation {
    GitDiff,
    UntrackedNumstat {
        path: String,
    },
    /// The full patch for one untracked file — its *content*, not just its
    /// shape.
    ///
    /// [`Self::UntrackedNumstat`] answers "how many lines", which is all the
    /// diff-size budget and the zero-diff guard ever needed. A verifier needs
    /// the other half: for a task whose entire deliverable IS an untracked
    /// file, a marker naming the path and a line count is a review of a
    /// filename. Graded that way, a verdict can only restate that something
    /// was written — which is what one did, in as many words, before this
    /// variant existed ("the unseen regex content cannot itself justify a
    /// FAIL").
    ///
    /// Git renders binary content as `Binary files ... differ`, so the bytes
    /// of a database sidecar or a compiled artifact can never reach a prompt
    /// through here.
    UntrackedPatch {
        path: String,
    },
}

#[async_trait]
pub trait DiagnosticRunner: Send + Sync {
    /// Run one fixed diagnostic invocation directly, without a shell.
    async fn run_diagnostic(&self, invocation: &DiagnosticInvocation) -> CmdOutcome;
}

/// Runs only already-validated [`TestInvocation`] values. Kept separate from
/// [`DiagnosticRunner`] so model-authored test text can never retarget the fixed
/// Git diagnostic vocabulary.
#[async_trait]
pub trait TestRunner: Send + Sync {
    /// Spawn `invocation.program` with `invocation.args` directly.
    async fn run_test(&self, invocation: &TestInvocation) -> CmdOutcome;
    /// Whether this probe invocation completes with exit 0 — "is this
    /// runner actually usable here?", never a test run (#1539).
    /// Version-style for every language runner; the cheapest no-op
    /// (`sh -c 'exit 0'`) for the shell, whose `--version` is non-portable
    /// (#2064).
    ///
    /// The witness author's tool surface is deliberately unable to execute
    /// anything, so runner existence is a fact only the pipeline can
    /// establish; this is where it asks. Defaults to `true` ("assume
    /// present") so a substrate that cannot check — a scripted double, a
    /// remoted host — only ever skips the availability steering, never
    /// blocks an author whose runner exists.
    async fn runner_available(&self, probe: &TestInvocation) -> bool {
        let _ = probe;
        true
    }
}

/// One parsed lint/typecheck record for the regression veto (#861).
///
/// Identity deliberately excludes line/column: the candidate's edits shift
/// positions, and a pre-existing diagnostic that moved is the same fact, not
/// a new regression. What remains — file, severity class, rule code, message
/// — is what a set-difference against the baseline can honestly call "new".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LintRecord {
    /// Repo-relative file the diagnostic points at.
    pub file: String,
    /// `true` for an error, `false` for a warning.
    pub error: bool,
    /// The toolchain's rule/code (`E0308`, `TS2322`, an eslint rule), when
    /// the dialect carries one.
    pub code: Option<String>,
    /// The diagnostic message.
    pub message: String,
}

/// Lint/typecheck snapshots for the regression veto (#861). Lint is rightly
/// excluded from the flip oracle — a lint pass verifies nothing — but it can
/// *veto*: a candidate that flips its witness while introducing a fresh type
/// error is exactly the inconclusive case the verifier exists for.
///
/// The host implementation resolves the workspace's own diagnostics plan
/// (closed toolchain vocabulary, fixed argv — no model text crosses this
/// boundary) and returns parsed records. `None` — no toolchain, no parse,
/// probe failure — degrades open: lint can only ever withhold a fast-submit,
/// never grant one, so an absent probe restores the pre-veto behavior.
#[async_trait]
pub trait LintProbe: Send + Sync {
    /// Parsed diagnostics of the tree rooted at `root` (`None` = process
    /// cwd). The root parameter is what lets one probe serve both the
    /// session tree and an isolated candidate worktree.
    async fn snapshot(&self, root: Option<&str>) -> Option<Vec<LintRecord>>;
}

/// Runs a test invocation under coverage instrumentation and reports which
/// lines it executed (#1291) — the host half of the diff-coverage check.
///
/// `None` is the answer for every way the measurement cannot be made: no
/// coverage tool for this dialect, the tool not installed, an instrumented run
/// that produced no readable report. It is deliberately indistinguishable from
/// "not wired", because the ladder treats all of them identically — as
/// [`crate::verify::coverage::DiffCoverage::Unmeasured`], which is a statement
/// about the instrument and never about the work.
///
/// Called only from the pre-submit audit, where a deterministic pass is about
/// to be credited, so a run that never reaches a fast-submit pays no
/// instrumented suite run at all. The cost when it does fire is one extra
/// (slower) run of the tracked command — which is why the strict reading is
/// opt-in and why a host is free never to implement this port.
#[async_trait]
pub trait CoverageProbe: Send + Sync {
    /// Executed lines by repo-relative path, for a run of `invocation`
    /// against the tree at `root` (`None` = process cwd).
    async fn covered_lines(
        &self,
        root: Option<&str>,
        invocation: &TestInvocation,
    ) -> Option<crate::verify::coverage::CoverageReport>;
}

/// One single-line mutant for the witness mutation check (#870), proposed by
/// `verify::mutation::mutants_from_diff` from the candidate's own diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineMutation {
    /// Repo-relative file to mutate.
    pub path: String,
    /// 1-based line number in the file's current (post-candidate) state.
    pub line: u32,
    /// The line's expected current content — the host verifies it before
    /// mutating, so a drifted diff aborts the mutant instead of corrupting
    /// an unrelated line.
    pub original: String,
    /// The broken replacement line.
    pub mutated: String,
}

/// What one mutant run established (#870).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutantOutcome {
    /// The witness ran to completion against the mutant, and this is what it
    /// said. `passed: true` — the witness stayed green while the change was
    /// broken under it — is the tautology signal.
    Witness { passed: bool },
    /// The mutant could not be staged or observed (line drifted, file
    /// unreadable, infra outcome). Counts neither way; the check degrades
    /// open.
    Unavailable,
    /// The mutation was applied and the ORIGINAL BYTES COULD NOT BE
    /// RESTORED. The tree is no longer the verified candidate; the caller
    /// must fail the candidate closed rather than ship it.
    TreePoisoned,
}

/// Runs one witness invocation against a single-line mutant of the tree at
/// `root` (#870). The host owns the mechanics — stage the mutation, run,
/// restore the original bytes — and must report [`MutantOutcome::TreePoisoned`]
/// if restoration cannot be guaranteed, because verification's seal
/// discipline depends on the tree ending byte-identical.
#[async_trait]
pub trait MutationProbe: Send + Sync {
    async fn run_mutant(
        &self,
        root: Option<&str>,
        mutation: &LineMutation,
        invocation: &TestInvocation,
    ) -> MutantOutcome;
}

/// Orchestrator MCP pre-fetch (issue #248): gathered ONCE before a
/// best-of-N fan-out and folded into every candidate's shared message
/// history, instead of N candidates each independently paying to look up the
/// same external context — the common "candidates all need the same DB
/// schema / ticket context" case.
///
/// Consulted exactly where the isolated-candidate path runs
/// (`Pipeline::run_best_of_n`): `candidates > 1`, the single-candidate
/// authored-witness run (an authored witness needs a disposable workspace
/// even at N=1), and a single-shot run isolated into a throwaway worktree
/// (`isolate_in_worktree` answering yes). Only a single-shot run executing
/// in the session tree never reaches it.
///
/// Deliberately goal-blind (#1779): the sweep calls every candidate-safe
/// zero-argument tool with `{}` and concatenates the output, so there is
/// nothing a goal could steer — a `goal` parameter here would advertise
/// relevance no implementor delivers. A goal-aware prefetch that runs ahead
/// of planning is the research-stage design deferred to #1778, which may
/// subsume this port entirely.
#[async_trait]
pub trait McpPrefetchPort: Send + Sync {
    /// Best-effort: `None` when there is nothing worth injecting (no
    /// candidate-safe servers connected, every call failed, or every call
    /// returned nothing) — a prefetch miss never aborts the run.
    async fn prefetch(&self) -> Option<String>;
}

/// A human's decision at the scope-review gate (L-E5). `Trim` carries the
/// indices (into the proposed plan) the user chose to keep, so the pipeline
/// executes a reduced plan rather than the whole thing.
///
/// Serde because a decision can cross a *process* boundary, not just a crate
/// one: a supervised run's gate parks the proposal in its session sidecar and
/// an attached terminal answers by writing a `ScopeDecision` back (#1585).
/// Snake-case tags so the file reads like every other sidecar document.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeDecision {
    /// Execute the plan as proposed.
    Approve,
    /// Execute only the steps at these indices (into the proposed step list),
    /// in the given order. Out-of-range indices are ignored by the pipeline.
    Trim { keep_steps: Vec<usize> },
    /// Re-plan: the reviewer rejected *this* scope and said what they want
    /// instead. `note` is their own words, folded into the planner's next
    /// prompt; the fresh plan is gated again (bounded — see
    /// `MAX_SCOPE_REVISIONS` in `crate::pipeline`).
    ///
    /// Approve/Trim/Abort are the three answers a *yes-or-no* gate can take,
    /// and a review is not a yes-or-no question: the common human reply to a
    /// plan is neither "run it" nor "run less of it" but "not like that — do
    /// this". Without this variant every such reply has to be spelled as an
    /// abort, which throws away both the turn and the reviewer's reasoning.
    Revise { note: String },
    /// Abandon the turn cleanly — no execution happens.
    Abort,
}

/// The interactive scope-review gate (L-E5). Above configured thresholds the
/// pipeline emits a `ScopeReview` event and blocks on this port for the
/// user's [`ScopeDecision`]. Headless runs supply [`AlwaysAbortGate`]; the
/// explicit config bypass (`PipelineConfig::headless_bypass_scope_review`)
/// skips the gate entirely rather than consulting one that would abort, and
/// a headless run that hits the gate with **no** bypass configured is a
/// named error, never a silent auto-approve (see [`crate::PipelineConfig`]).
#[async_trait]
pub trait ApprovalGate: Send + Sync {
    /// Present `proposal` and await the user's decision.
    async fn review(&self, proposal: &ScopeProposal) -> ScopeDecision;

    /// Ask a plain yes/no question and await the answer — today, whether to
    /// isolate this run in a worktree ([`WorktreePolicy::Ask`]).
    ///
    /// Defaulted to `false` rather than left abstract, and that default is the
    /// answer, not a placeholder: a gate that cannot ask has nobody to ask, and
    /// declining leaves the run doing exactly what it did before the question
    /// existed. Silently answering *yes* would relocate somebody's work on the
    /// strength of an unwired port.
    async fn confirm(&self, _question: &str) -> bool {
        false
    }
}

/// Whether a run does its work in a throwaway git worktree instead of the
/// user's own checkout.
///
/// The two are not equivalent, which is why this is a policy and not a
/// preference. Working in the tree is what the user sees happening and can
/// interrupt; working in a worktree means nothing lands until the run adopts
/// it, so a half-finished run leaves the checkout untouched — at the cost that
/// `git status` shows nothing while it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorktreePolicy {
    /// Isolate whenever the run is going to change files and isolation is
    /// available.
    Always,
    /// Ask, once, at triage — after the class is known, so a lookup that will
    /// not touch anything never raises the question. The default, and what an
    /// absent, null, or empty setting means.
    #[default]
    Ask,
    /// Never isolate; work in the checkout.
    Never,
}

// No-op / default port implementations
//
// These let a caller run the pipeline before every subsystem is wired (the
// whole point of the port boundary) and give tests trivial doubles for the
// ports they aren't exercising.

/// A [`ContextRecallPort`] that recalls nothing — the correct default before
/// `stella-context` is wired, and for tasks where context grounding is off.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoContextRecall;

#[async_trait]
impl ContextRecallPort for NoContextRecall {
    async fn recall(&self, _goal: &str) -> Recall {
        Recall::default()
    }
}

/// A [`RepoStructurePort`] that offers no structure — the planner falls back
/// to goal + recall alone.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoRepoStructure;

#[async_trait]
impl RepoStructurePort for NoRepoStructure {
    async fn structure_summary(&self) -> String {
        String::new()
    }
}

/// A [`RepoStatusPort`] that reports no untracked files — for callers with no
/// git working tree (the zero-diff guard then uses the tracked diff alone).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoRepoStatus;

#[async_trait]
impl RepoStatusPort for NoRepoStatus {
    async fn untracked_fingerprints(&self) -> std::collections::HashMap<String, String> {
        std::collections::HashMap::new()
    }
}

/// An [`ApprovalGate`] that aborts every proposal — a conservative headless
/// default (never runs large work unattended).
#[derive(Debug, Default, Clone, Copy)]
pub struct AlwaysAbortGate;

#[async_trait]
impl ApprovalGate for AlwaysAbortGate {
    async fn review(&self, _proposal: &ScopeProposal) -> ScopeDecision {
        ScopeDecision::Abort
    }
}

/// An [`ApprovalGate`] that prints the proposal to stdout and reads an answer
/// from stdin: `y`/`yes` approves, a bare `n`/`no`/empty line aborts, **EOF and
/// read errors abort** (fail-closed), and any other typed line is the
/// reviewer's revision note ([`ScopeDecision::Revise`]) — the same reading the
/// TUI's card gives typed text, so the two interactive surfaces answer the gate
/// the same way.
///
/// Deliberately without a [`ScopeDecision::Trim`] path: per-step selection
/// needs a real prompt, and half-implementing it here would let a typo drop
/// steps silently. (A note asking for fewer steps reaches the same place by
/// re-planning, without inventing an index syntax over a `read_line`.) For
/// interactive text mode only — headless runs use [`AlwaysAbortGate`] (with the
/// config bypass skipping the gate outright), and the TUI supplies its own gate.
///
/// Note that [`ApprovalGate::review`] is `async` while this reads stdin with
/// blocking I/O, so it parks the calling runtime thread until the user
/// answers.
pub struct StdioApprovalGate;

#[async_trait]
impl ApprovalGate for StdioApprovalGate {
    async fn review(&self, proposal: &ScopeProposal) -> ScopeDecision {
        use std::io::{self, BufRead, Write};

        println!();
        println!("  ┌─ Scope Review ──────────────────────────────");
        println!("  │ {}", proposal.summary);
        for (i, step) in proposal.steps.iter().enumerate() {
            println!("  │   {}. {}", i + 1, step);
        }
        if let Some(cost) = proposal.estimated_cost_usd {
            println!("  │ est. cost: ${cost:.4}");
        }
        println!("  └──────────────────────────────────────────────");
        // Every branch offered below exists: `y` approves, a bare no/empty
        // aborts, and anything else is read as a note (which is why the
        // prompt has to say so — a reviewer who types a sentence at a
        // `[y/N]` prompt would otherwise expect it to be ignored, not to
        // become the instruction the next plan is built from).
        print!("  Approve? [y/N, or type what to change]: ");
        let _ = io::stdout().flush();

        let stdin = io::stdin();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line).is_err() {
            return ScopeDecision::Abort;
        }
        decision_from_line(&line)
    }

    /// `[y/N]` on stdin. Fail-closed on EOF and read errors, and on a bare
    /// Enter, matching [`Self::review`] — an unanswered question must not
    /// relocate where somebody's work happens.
    async fn confirm(&self, question: &str) -> bool {
        use std::io::{self, BufRead, Write};

        println!();
        println!("  {question}");
        print!("  [y/N]: ");
        let _ = io::stdout().flush();

        let mut line = String::new();
        if io::stdin().lock().read_line(&mut line).is_err() {
            return false;
        }
        matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    }
}

/// One typed line of a scope-review answer, as a [`ScopeDecision`].
///
/// The single reading every line-based approval surface shares:
/// [`StdioApprovalGate`] here, and the supervised sidecar transport's attached
/// terminal in `stella-cli` (#1585). `y`/`yes` approves, a bare `n`/`no`/empty
/// line aborts, and any other text is the reviewer's revision note — shared so
/// the same keystrokes can never mean different things depending on whether
/// the run happened to be supervised.
pub fn decision_from_line(line: &str) -> ScopeDecision {
    let answer = line.trim();
    match answer.to_ascii_lowercase().as_str() {
        "y" | "yes" => ScopeDecision::Approve,
        "" | "n" | "no" => ScopeDecision::Abort,
        _ => ScopeDecision::Revise {
            note: answer.to_string(),
        },
    }
}

/// The ports the pipeline orchestrates over. The `stella-cli` glue fills this
/// with real subsystem adapters; tests fill it with scripted doubles. Grouped
/// into one struct so [`crate::pipeline::Pipeline::new`] stays a
/// three-argument constructor rather than a twenty-parameter one.
pub struct PipelinePorts<'a> {
    /// Role → model resolution (`stella-core`). Held immutably; see the
    /// module's "breaker feedback boundary" note.
    pub router: &'a Router,
    /// Maps a resolved [`ModelRef`] to its concrete provider adapter.
    pub providers: &'a dyn ProviderResolver,
    /// The tool registry the execute engine drives.
    pub tools: &'a dyn stella_core::ToolExecutor,
    /// Context recall at turn start (L-E8).
    pub recall: &'a dyn ContextRecallPort,
    /// Repo-structure summary for the planner's split context (L-E6).
    pub repo: &'a dyn RepoStructurePort,
    /// Untracked-file snapshots for the zero-diff guard (`git diff` can't see
    /// untracked files; this makes new/modified untracked files visible).
    pub repo_status: &'a dyn RepoStatusPort,
    /// Runs closed, typed diagnostic invocations.
    pub diagnostics: &'a dyn DiagnosticRunner,
    /// Runs validated test invocations directly, without a shell.
    pub tests: &'a dyn TestRunner,
    /// Lint/typecheck snapshots for the regression veto (#861). `None` —
    /// hosts without a diagnostics toolchain — disables the veto and nothing
    /// else.
    pub lint: Option<&'a dyn LintProbe>,
    /// The witness mutation check (#870). `None` disables the check and
    /// nothing else.
    pub mutation: Option<&'a dyn MutationProbe>,
    /// The diff-coverage check (#1291) — did the passing test run the changed
    /// lines? `None` leaves every candidate's overlap `Unmeasured`, which is
    /// stated in the verdict and (by default) withholds nothing.
    pub coverage: Option<&'a dyn CoverageProbe>,
    /// The interactive scope-review gate (L-E5).
    pub approvals: &'a dyn ApprovalGate,
    /// The delay port for retry backoff — the same testability seam
    /// `stella-core` uses; production passes the CLI's tokio-backed
    /// sleeper, tests a no-op.
    pub sleeper: &'a dyn Sleeper,
    /// Lifecycle hooks for the execute engine — the parsed config plus the
    /// runner that spawns hook commands (`stella_core::hooks`). `None` runs
    /// the exact pre-hooks pipeline; the CLI passes its settings-chain hooks
    /// so `PreToolUse` gating also covers the default `stella run` path.
    pub hooks: Option<(&'a Hooks, &'a dyn HookRunner)>,
    /// Candidate isolation (L-E7): one snapshot per candidate and passing-only
    /// adoption. Also required for authored witnesses at `candidates = 1`.
    pub candidate_workspaces: Option<&'a dyn CandidateWorkspacePort>,
    /// Orchestrator MCP pre-fetch (issue #248) — see `crate::mcp_prefetch::fold`.
    pub mcp_prefetch: Option<&'a dyn McpPrefetchPort>,
    /// Step-boundary steering for the EXECUTE engine only — mid-turn user
    /// messages injected as the model's next observation (`stella_core`'s
    /// `TurnSteering`). `None` on non-interactive paths (headless `run`,
    /// fleet). Attached to execute turns alone: triage, planning, the
    /// witness author, and the verifier are autonomous sub-steps with no
    /// user-facing "steer this" moment. The pipeline's stop remains the
    /// caller's hard cancel — a pipeline is triage→…→verifier, so a
    /// mid-execute soft stop has no single obvious continuation.
    pub steering: Option<&'a dyn stella_core::ports::TurnSteering>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Invariant 4: a decision that crosses the supervised sidecar boundary
    /// (#1585) must round-trip byte-for-byte — the writing terminal and the
    /// parked run are different processes, possibly different builds.
    #[test]
    fn scope_decision_round_trips_through_json() {
        let decisions = [
            ScopeDecision::Approve,
            ScopeDecision::Trim {
                keep_steps: vec![0, 2],
            },
            ScopeDecision::Revise {
                note: "smaller: docs only".to_string(),
            },
            ScopeDecision::Abort,
        ];
        for decision in decisions {
            let json = serde_json::to_string(&decision).expect("encode");
            let back: ScopeDecision = serde_json::from_str(&json).expect("decode");
            assert_eq!(back, decision, "{json}");
        }
    }

    #[test]
    fn cmd_outcome_passed_is_exit_zero_only() {
        assert!(
            CmdOutcome {
                exit_code: 0,
                stdout_tail: String::new(),
                stderr_tail: String::new(),
                kind: CmdKind::Completed,
            }
            .passed()
        );
        for code in [1, 2, 127, 130, -1] {
            assert!(
                !CmdOutcome {
                    exit_code: code,
                    stdout_tail: String::new(),
                    stderr_tail: String::new(),
                    kind: CmdKind::Completed,
                }
                .passed(),
                "exit {code} must not be treated as passing"
            );
        }
    }

    /// The #860 boundary: an infra outcome is neither a pass nor a failing
    /// assertion, whatever exit code the runner synthesized — including the
    /// pathological `exit_code: 0` shapes a buggy runner could produce.
    #[test]
    fn infra_outcomes_are_never_assertions() {
        for kind in [CmdKind::TimedOut, CmdKind::OutOfMemory, CmdKind::Infra] {
            for code in [0, 1, -1, 124] {
                let out = CmdOutcome {
                    exit_code: code,
                    stdout_tail: String::new(),
                    stderr_tail: String::new(),
                    kind,
                };
                assert!(!out.passed(), "{kind:?}/exit {code} must not pass");
                assert_eq!(
                    out.assertion_result(),
                    None,
                    "{kind:?}/exit {code} observed no assertion either way"
                );
                assert!(out.infra_label().is_some());
            }
        }
    }

    /// #1294: an out-of-memory kill is its OWN outcome — not a failure, and
    /// not the same word as an unspawnable toolchain. The label is what a
    /// verifier reads, so it has to say which of the two happened.
    #[test]
    fn an_out_of_memory_kill_is_its_own_outcome() {
        let oom = CmdOutcome {
            // Whatever the runner synthesized: an OOM kill leaves no real
            // code, and the exit code must not be what anyone reads.
            exit_code: -1,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            kind: CmdKind::OutOfMemory,
        };
        assert!(!oom.passed());
        assert_eq!(
            oom.assertion_result(),
            None,
            "a run the kernel killed observed no assertion, so it is not a failing test"
        );
        assert_eq!(oom.infra_label(), Some("out_of_memory"));
        assert!(oom.is_out_of_memory());
        for kind in [CmdKind::Completed, CmdKind::TimedOut, CmdKind::Infra] {
            assert!(
                !CmdOutcome {
                    kind,
                    ..oom.clone()
                }
                .is_out_of_memory(),
                "{kind:?} is not an out-of-memory kill"
            );
            assert_ne!(
                CmdOutcome {
                    kind,
                    ..oom.clone()
                }
                .infra_label(),
                Some("out_of_memory"),
                "{kind:?} must not borrow the out-of-memory label"
            );
        }
    }

    #[test]
    fn completed_runs_report_their_assertion() {
        let ok = CmdOutcome {
            exit_code: 0,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            kind: CmdKind::Completed,
        };
        assert_eq!(ok.assertion_result(), Some(true));
        assert_eq!(ok.infra_label(), None);
        let fail = CmdOutcome {
            exit_code: 101,
            ..ok.clone()
        };
        assert_eq!(fail.assertion_result(), Some(false));
    }

    #[tokio::test]
    async fn no_op_ports_are_inert() {
        assert!(NoContextRecall.recall("anything").await.is_empty());
        assert!(
            NoContextRecall.recall("anything").await.usage.is_none(),
            "a port with no CGP host behind it reports no usage"
        );
        assert!(NoRepoStructure.structure_summary().await.is_empty());
        let proposal = ScopeProposal {
            summary: "x".into(),
            steps: vec!["a".into()],
            estimated_files: 1,
            estimated_cost_usd: None,
            ..Default::default()
        };
        assert_eq!(
            AlwaysAbortGate.review(&proposal).await,
            ScopeDecision::Abort
        );
    }

    /// #2476's predicate: a label is dropped for exactly the two shapes the
    /// memory mint (`truncate_label`) produces, and nothing else.
    #[test]
    fn a_label_minted_from_the_content_is_not_distinct() {
        let frame = |label: &str, content: &str| RecalledFrame {
            citation_label: label.into(),
            content: content.into(),
            ..recalled("workspace-memory", "nod_x", None)
        };
        // ≤80 chars: the mint copies the content verbatim.
        let short = frame("prefer rg over grep", "prefer rg over grep");
        assert_eq!(short.distinct_label(), None);
        // >80 chars: the mint keeps the first 79 chars plus `…`.
        let long_content = "a".repeat(120);
        let minted: String = format!("{}…", "a".repeat(79));
        let long = frame(&minted, &long_content);
        assert_eq!(long.distinct_label(), None);
        // Whitespace differences do not defeat the match — both sides render
        // trimmed, so they compare trimmed.
        let padded = frame("  prefer rg over grep  ", "prefer rg over grep\n");
        assert_eq!(padded.distinct_label(), None);
        // An empty label was never citable text at all.
        assert_eq!(frame("", "some content").distinct_label(), None);
    }

    #[test]
    fn an_author_chosen_label_stays_distinct() {
        let frame = |label: &str, content: &str| RecalledFrame {
            citation_label: label.into(),
            content: content.into(),
            ..recalled("code-graph", "nod_y", None)
        };
        // A path label over a code excerpt — the ordinary code-graph shape.
        let hit = frame("src/lib.rs", "fn main() {}");
        assert_eq!(hit.distinct_label(), Some("src/lib.rs"));
        // A hand-picked label that happens to open the content is NOT the
        // mint's shape (no `…`), so it is the author's choice and it stays.
        let prefix = frame("prefer rg", "prefer rg over grep here");
        assert_eq!(prefix.distinct_label(), Some("prefer rg"));
        // `…`-terminated, but over different text: distinct.
        let elsewhere = frame("retry policy…", "the backoff doubles per attempt");
        assert_eq!(elsewhere.distinct_label(), Some("retry policy…"));
        // A frame with no content cannot cover any label.
        let bare = frame("an episode title", "");
        assert_eq!(bare.distinct_label(), Some("an episode title"));
    }

    fn recalled(provider: &str, id: &str, digest: Option<&str>) -> RecalledFrame {
        RecalledFrame {
            citation_label: format!("label for {id}"),
            provider: provider.into(),
            source: "stella-context".into(),
            kind: "memory".into(),
            uri: None,
            method: None,
            content: "body".into(),
            token_cost: 7,
            id: Some(id.into()),
            content_digest: digest.map(str::to_owned),
        }
    }

    #[test]
    fn a_recall_projects_to_one_telemetry_event_with_provenance_intact() {
        // Phase 2 (#713) deliverable 3. The projection lives here, once, so
        // the five surfaces that recall cannot disagree about the shape of the
        // event they report — the failure mode of copying it into each.
        let recall = Recall {
            frames: vec![
                recalled("workspace-memory", "nod_a", Some("sha256:aa")),
                recalled("workspace-memory", "nod_b", None),
                recalled("code-graph", "sym_c", Some("sha256:cc")),
            ],
            usage: None,
            latency_ms: 42,
            used_ann_index: Some(true),
        };
        let Some(stella_protocol::AgentEvent::ContextRecall {
            frames,
            provider_mix,
            tokens,
            ..
        }) = recall.telemetry_event()
        else {
            panic!("a non-empty recall reports an event");
        };
        assert_eq!(tokens, 21, "the summed per-frame cost");
        // The mix counts frames that reached the prompt, per provider, in
        // first-seen order.
        assert_eq!(provider_mix.len(), 2);
        assert_eq!(provider_mix[0].provider, "workspace-memory");
        assert_eq!(provider_mix[0].frames, 2);
        assert_eq!(provider_mix[1].frames, 1);
        // The digest survives the projection — the whole point of deliverable
        // 2. Before this it was hard-coded `None` at the one emission site.
        assert_eq!(frames[0].content_digest.as_deref(), Some("sha256:aa"));
        assert_eq!(
            frames[1].content_digest, None,
            "a provider that declared none keeps none — that absence is the \
             signal that the frame is not verifiable and must be re-queried"
        );
        assert_eq!(frames[2].content_digest.as_deref(), Some("sha256:cc"));
    }

    #[test]
    fn an_empty_recall_reports_nothing_rather_than_an_empty_event() {
        // A turn that recalled nothing did not have a recall stage worth a
        // receipt; an empty event would be a row that means "we looked" and
        // reads as "we found nothing", which are different claims.
        assert!(Recall::default().telemetry_event().is_none());
    }
}
