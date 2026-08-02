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
use stella_core::Router;
use stella_core::hooks::{HookRunner, Hooks};
use stella_core::retry::Sleeper;
use stella_protocol::{ContextUsage, FileChangeKind, ModelRef, Provider, ScopeProposal};

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

/// One frame recalled from the context plane at turn start (the "context
/// recall" stage). A deliberately minimal local shape: the real
/// `stella-context` crate is being built in parallel and owns the rich
/// `ContextFrame`/retrieval types; the CLI glue adapts its frames down to
/// this at the seam so `stella-pipeline` takes **no** dependency on
/// `stella-context` (dependency direction discipline).
/// `citation_label` is mandatory and human-readable (L-C4); a
/// not-yet-materialized frame carries `id: None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecalledFrame {
    /// Human-readable citation, never a raw id (L-C4).
    pub citation_label: String,
    /// The CGP provider leg that returned this frame.
    pub provider: String,
    /// The record's original source from its provenance chain. This can
    /// differ from `provider` when an adapter fronts another context store.
    pub source: String,
    /// Protocol frame kind (`symbol`, `memory`, `graph`, ...).
    pub kind: String,
    /// Canonical source URI, when declared.
    pub uri: Option<String>,
    /// Most-derived provenance method, when declared.
    pub method: Option<String>,
    /// The actual text injected into the volatile recall message.
    pub content: String,
    /// Token cost of this frame, for the `ContextRecall` event's budget
    /// report (L-C5).
    pub token_cost: u32,
    /// Present only when the frame is materialized in the store; a candidate
    /// frame carries `None` (L-C4).
    pub id: Option<String>,
    /// `"sha256:<hex>"` over the exact bytes of [`Self::content`], as the
    /// source frame declared it — Phase 2 (#713).
    ///
    /// The digest was already minted at the store boundary and was then thrown
    /// away one layer up, at the CGP→pipeline projection, so every
    /// `ContextFrameRef` on every recall event carried `content_digest: null`.
    /// It is what makes a frame reference *resolve*: with it, a receipt names
    /// a record and the exact revision of that record's content, which is the
    /// identity ADR 0004 defines. Without it, a reference names a row whose
    /// text may since have been superseded, and a past turn's context is
    /// reconstructed rather than verified.
    ///
    /// `None` when the serving provider declared none — per
    /// `docs/context-reuse.md` §1 such a frame is *not verifiable* and a host
    /// must re-query rather than reuse it, so the absence is meaningful and is
    /// carried rather than papered over with a locally recomputed hash.
    pub content_digest: Option<String>,
}

/// Context recall at turn start (L-E8): a *live provider query*, never a
/// cached prompt block. The recalled frames ride as a volatile message
/// **after** the byte-stable system prefix so prompt-cache hits on that
/// prefix are preserved — the assembly discipline is documented and enforced
/// in [`crate::pipeline`]. A caller with no context plane wired yet supplies
/// [`NoContextRecall`].
#[async_trait]
pub trait ContextRecallPort: Send + Sync {
    /// Recall material relevant to `goal`. Returns an empty frame list when
    /// nothing is relevant or no plane is configured — never an error the
    /// pipeline has to special-case (weak/absent context degrades to "no
    /// frames", L-C6).
    async fn recall(&self, goal: &str) -> Recall;
}

/// The outcome of one context recall: the frames that reached the prompt, and
/// what producing them cost.
///
/// The two travel together because they are answers to different questions
/// about the same request — "what will the model see?" and "what did this turn
/// spend on context, and which providers drove it?" — and separating them
/// would mean either re-running recall to bill it or losing the cost entirely,
/// which is how context cost stayed unmeterable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recall {
    /// The frames selected for this turn, in the host's canonical render
    /// order.
    pub frames: Vec<RecalledFrame>,
    /// The CGP usage report for the request (`docs/context-reuse.md` §2).
    /// `None` when the port has no CGP host behind it to report one.
    pub usage: Option<ContextUsage>,
    /// Wall-clock milliseconds this recall took, measured by the port that
    /// performed it (#875). Recall is on the first-token path of every turn,
    /// so this is the one number that says whether context retrieval is the
    /// reason a turn felt slow. `0` means "not measured" — the ports with
    /// nothing to recall do not time an empty answer.
    pub latency_ms: u32,
    /// Whether the IVF approximate-nearest-neighbour accelerator fired, or
    /// `None` when this recall path does not report it. Paired with
    /// `latency_ms` because a slow recall with a cold index is a different
    /// problem, with a different fix, from a slow recall that used the index
    /// and was still slow. Tri-state because the CGP host fan-out does not
    /// carry the flag across the provider-result boundary — see the protocol
    /// event for why `false` would be a worse answer than `None`.
    pub used_ann_index: Option<bool>,
}

impl Recall {
    /// A recall of `frames` with no usage report — the shape a port without a
    /// CGP host behind it produces.
    pub fn frames(frames: Vec<RecalledFrame>) -> Self {
        Self {
            frames,
            usage: None,
            latency_ms: 0,
            used_ann_index: None,
        }
    }

    /// Whether nothing was recalled.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// This recall as its telemetry event, or `None` when nothing was recalled.
    ///
    /// Phase 2 (#713) deliverable 3. This lives on [`Recall`] rather than in
    /// the pipeline because the pipeline was, until now, the **only** path that
    /// emitted it: the one-shot run, the interactive REPL, `/goal`, and the
    /// Command Deck all recalled and reported nothing, so most real usage was
    /// invisible to `stella inspect`. Fixing that by copying the pipeline's
    /// projection into five call sites would have made the event's shape a
    /// convention rather than a definition — and the first divergence would be
    /// a provider mix that only some surfaces counted.
    ///
    /// `block_id` is deliberately absent here and present on the receipt side
    /// instead: a recalled frame becomes a context block only once it is
    /// *rendered* into a message, which happens after this event is emitted and
    /// does not happen at all on the planner's structured path. The join to a
    /// block runs through the record — `BlockOrigin::memory_id` — not through
    /// an id fabricated before the block exists.
    #[must_use]
    pub fn telemetry_event(&self) -> Option<stella_protocol::AgentEvent> {
        if self.frames.is_empty() {
            return None;
        }
        let mut provider_mix: Vec<stella_protocol::ProviderShare> = Vec::new();
        for frame in &self.frames {
            match provider_mix
                .iter_mut()
                .find(|share| share.provider == frame.provider)
            {
                Some(share) => share.frames += 1,
                None => provider_mix.push(stella_protocol::ProviderShare {
                    provider: frame.provider.clone(),
                    frames: 1,
                }),
            }
        }
        Some(stella_protocol::AgentEvent::ContextRecall {
            tokens: self.frames.iter().map(|f| f.token_cost).sum(),
            frames: self
                .frames
                .iter()
                .map(|f| stella_protocol::ContextFrameRef {
                    id: f.id.clone(),
                    citation_label: f.citation_label.clone(),
                    provider: f.provider.clone(),
                    source: f.source.clone(),
                    kind: f.kind.clone(),
                    uri: f.uri.clone(),
                    method: f.method.clone(),
                    token_cost: f.token_cost,
                    block_id: None,
                    content_digest: f.content_digest.clone(),
                })
                .collect(),
            provider_mix,
            usage: self.usage.clone(),
            latency_ms: self.latency_ms,
            used_ann_index: self.used_ann_index,
        })
    }
}

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

/// The recorder's own tally of mutating file touches — the count behind the
/// `AgentEvent::FileChange`s a turn emits, read from the object that emitted
/// them.
///
/// The pipeline used to count those events off the sender it hands the engine.
/// It never saw one: `ToolRegistry::record_touch` sends to the channel the
/// *host* attached, and the engine's sender is a different wire. So a turn that
/// created and edited a file, and put six `file_change` events on the stream,
/// told its judge `file_change_events=0` — which the judge reported as "the
/// file likely does not exist" while the file sat in the container (#973).
///
/// Reading a counter on the recorder is immune to which channel the events
/// took, and to there being no channel at all — a candidate registry is
/// deliberately unattached, and its changes still count here. Implementations
/// wire it from the same registry they call `attach_events` on, so the count
/// and the stream have one source by construction.
///
/// Monotonic since construction: the pipeline takes a delta across the window
/// it cares about. Hosts with no registry supply [`NoFileTouches`].
pub trait FileTouchPort: Send + Sync {
    /// Mutating touches recorded so far. Reads are excluded — they are
    /// observability, not change, exactly as `FileChangeKind::is_mutation`
    /// decides for the event.
    fn mutations_recorded(&self) -> u64;

    /// Take a before-image of the workspace for the turn about to run.
    ///
    /// The pair with [`Self::settle_workspace_probe`] exists because a tool's
    /// schema cannot describe what `bash` will touch, so the tree has to be
    /// asked instead. Bracketing the TURN rather than each call is a measured
    /// choice: a single walk of a 20,000-entry tree costs ~1s, a turn issues
    /// hundreds of shell calls, and a trial is killed at 900s — per-call
    /// probing would spend the whole budget observing.
    ///
    /// Calling this commits the host to turn granularity: an implementation
    /// that also probes per call must turn that off, so the tree is never
    /// walked twice to learn one fact.
    ///
    /// Default no-op, so a host with no recorder (and every test double)
    /// keeps working: an unprobed turn simply reports what its other channels
    /// saw, which is the pre-existing behaviour.
    fn begin_workspace_probe(&self) {}

    /// Attribute anything the turn changed that no tool schema described,
    /// then re-arm for the next turn.
    ///
    /// Re-arming is part of settling rather than a second call, because the
    /// after-image of one turn is the before-image of the next and both are
    /// only true at the same instant. Callers bracket a run of turns with one
    /// [`Self::begin_workspace_probe`] and a settle per turn.
    ///
    /// Records through the same emitter as every other touch, so the ledger
    /// keeps exactly one birthplace for a `FileChange`.
    fn settle_workspace_probe(&self) {}
}

/// A [`FileTouchPort`] that never observes anything, for hosts with no file
/// recorder. Reports a constant `0`, so every delta is `0` — "no touches seen
/// here", which leaves the ladder's other channels to speak.
pub struct NoFileTouches;

impl FileTouchPort for NoFileTouches {
    fn mutations_recorded(&self) -> u64 {
        0
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CmdKind {
    /// The process ran to completion; `exit_code` is its real exit status.
    #[default]
    Completed,
    /// The runner killed the process at its own deadline. `exit_code` is
    /// synthetic, and nothing about the command's assertions was observed.
    TimedOut,
    /// The process never produced a real exit status — it could not be
    /// spawned (missing program/toolchain) or its wait failed.
    Infra,
}

/// The outcome of running one local process through a pipeline runner. Output
/// is pre-truncated by the runner (middle-out, L-S3) into head+tail tails —
/// the pipeline never needs the full stream, only exit status and enough
/// text to summarize evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
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
            CmdKind::TimedOut | CmdKind::Infra => None,
        }
    }

    /// A short label for evidence text when [`Self::assertion_result`] is
    /// `None`, so a judge reads "the run timed out" instead of a bare
    /// failure it would treat as an assertion.
    pub fn infra_label(&self) -> Option<&'static str> {
        match self.kind {
            CmdKind::Completed => None,
            CmdKind::TimedOut => Some("timed_out"),
            CmdKind::Infra => Some("infra_failure"),
        }
    }
}

/// Closed diagnostic vocabulary. Every variant maps to fixed executable argv;
/// no caller-provided shell string crosses this boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticInvocation {
    GitDiff,
    UntrackedNumstat { path: String },
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
/// error is exactly the inconclusive case the judge exists for.
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

/// One file the winning best-of-N candidate changed, as applied to the real
/// tree by [`CandidateWorkspace::adopt`]. Paths are repo-relative.
///
/// This is the whole `FileChange` the pipeline emits for adopted work, because
/// the session's own recorder never saw the winner's edits — they happened
/// inside a shadow worktree, against a registry deliberately left unattached
/// (a losing candidate's edits must never be announced as the user's). So the
/// delta has to come from the adoption itself: `git`'s own `--numstat` for the
/// counts, and its patch text, sliced per file, for `diff`.
///
/// `added`/`removed` default to `0` and `diff` to `None` only for an
/// implementation that cannot measure them — which reads in the Files tab as
/// "changed, extent unknown", not as "nothing changed".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptedChange {
    pub path: String,
    pub kind: FileChangeKind,
    pub added: u32,
    pub removed: u32,
    pub diff: Option<String>,
}

/// A typed candidate-isolation failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkspaceError {
    /// The current tree state could not be snapshotted (not a git repository,
    /// no commits yet, git unavailable, worktree creation failed). The
    /// pipeline scores the affected candidate as aborted — it never falls
    /// back to running that candidate in the shared tree.
    #[error("could not snapshot the working tree: {reason}")]
    Snapshot { reason: String },
    /// Candidate state could not be committed into the immutable tree that
    /// final verification and adoption must share.
    #[error("could not seal candidate workspace `{workspace}`: {reason}")]
    Seal { reason: String, workspace: String },
    /// Applying the winning candidate's changes to the real tree failed —
    /// typically because the user edited the same files mid-run. Adoption is
    /// all-or-nothing: NOTHING was applied, `paths` names the conflicts, and
    /// the candidate workspace is preserved at `workspace` so the winning
    /// work is recoverable by hand.
    #[error(
        "adopting the winning candidate's changes failed ({reason}); conflicting paths: {}; \
         nothing was applied — the candidate's work is preserved at `{workspace}`",
        paths.join(", ")
    )]
    Adopt {
        reason: String,
        paths: Vec<String>,
        workspace: String,
    },
    /// An accepted witness artifact could not be copied out of the pristine
    /// authoring snapshot into the candidate that executed. The candidate is
    /// left exactly as the worker made it and the run degrades to the
    /// unauthored ladder: a witness that cannot be placed proves nothing, but
    /// the work it would have pinned is real and already done.
    #[error("could not graft witness artifact `{path}` into `{workspace}`: {reason}")]
    Graft {
        reason: String,
        path: String,
        workspace: String,
    },
}

/// One isolated best-of-N candidate workspace: a snapshot of the working
/// tree's *current* state (HEAD plus uncommitted and untracked files) that a
/// candidate executes and verifies inside, so sibling candidates never see
/// each other's edits and losers leave no residue in the real tree. The
/// bundled ports are rooted at the snapshot — the pipeline threads them (not
/// the session ports) through the candidate's engine turns and verification
/// runs.
#[async_trait]
pub trait CandidateWorkspace: Send + Sync {
    /// Absolute workspace root used to bind engine hook payloads and hook
    /// process execution to this candidate rather than the session tree.
    fn root(&self) -> &str;
    /// The tool executor rooted at this workspace: every engine-turn tool
    /// call (read/edit/shell) lands in the snapshot, never in the real tree.
    fn tools(&self) -> &dyn stella_core::ToolExecutor;
    /// Capability-minimal witness executor, constructed with the candidate:
    /// candidate-root reads plus one atomic, create-only witness test action.
    /// It must never expose general writes, edits, processes, hooks, MCP,
    /// custom tools, credentials, or external adapters.
    fn witness_tools(&self) -> &dyn stella_core::ToolExecutor;
    /// Runs closed diagnostic invocations inside the snapshot.
    fn diagnostics(&self) -> &dyn DiagnosticRunner;
    /// Typed test-process runner rooted at this workspace.
    fn tests(&self) -> &dyn TestRunner;
    /// Untracked-file fingerprints of the snapshot, mirroring the real
    /// tree's semantics (the tamper watchlist and zero-diff guard must keep
    /// working unchanged inside a candidate).
    fn repo_status(&self) -> &dyn RepoStatusPort;
    /// Commit the current candidate bytes into its private immutable history
    /// immediately before a final verification observation.
    async fn seal(&self) -> Result<(), WorkspaceError>;
    /// Whether the live worktree and HEAD still exactly match the last seal.
    async fn sealed_is_unchanged(&self) -> Result<bool, WorkspaceError>;
    /// Apply this workspace's changes — relative to its starting snapshot —
    /// to the real tree. All-or-nothing: on conflict the real tree is left
    /// byte-identical and the error names the conflicting paths
    /// ([`WorkspaceError::Adopt`]); the user's index and stash are never
    /// touched on any path.
    ///
    /// `withhold` names workspace-relative paths to leave behind — they are
    /// excluded from both the returned change list and the applied patch, so
    /// the two can never disagree. This is how an authored witness stays
    /// ephemeral: it must exist inside the candidate (it *is* the test the
    /// flip oracle runs), but it is scaffolding for the run, not a change the
    /// user asked for, so by default it dies with the workspace instead of
    /// being copied into the real tree. Empty means adopt everything.
    async fn adopt(&self, withhold: &[String]) -> Result<Vec<AdoptedChange>, WorkspaceError>;
    /// Copy one accepted witness artifact from another workspace's root into
    /// this one at the same relative `path`, and fail if anything is already
    /// there.
    ///
    /// Witness authoring runs in a *pristine* sibling snapshot so the author
    /// never reads the implementation it is meant to pin (see
    /// [`crate::witness`]). The artifact it produces has to end up in the
    /// candidate that executed, because that is the tree the flip oracle
    /// observes the pass in. This is the one byte-for-byte crossing between
    /// the two, and it carries exactly one already-validated file.
    ///
    /// Create-only, and no link is followed on either side. A destination that
    /// already exists is [`WorkspaceError::Graft`], never an overwrite: the
    /// worker may legitimately have written a test at that path, and silently
    /// replacing it would destroy the candidate's own work to make room for
    /// scaffolding. The caller re-validates the identity of the *written* copy
    /// against the candidate's own repo status, so the bytes tamper exclusion
    /// pins are the bytes that will actually run.
    async fn graft_witness(&self, source_root: &str, path: &str) -> Result<(), WorkspaceError>;
    /// Remove the workspace. Best-effort and infallible — the cleanup
    /// discipline is the pipeline's (every workspace is removed on every
    /// path, except a winner whose adoption failed, which is preserved for
    /// recovery).
    async fn remove(&self);
}

/// Candidate isolation (L-E7): snapshots the current working-tree state into
/// one isolated [`CandidateWorkspace`] per candidate. Best-of-N uses it when
/// available; authored witnesses require it even for one candidate.
#[async_trait]
pub trait CandidateWorkspacePort: Send + Sync {
    /// Snapshot the current tree state into a fresh isolated workspace.
    async fn create(&self) -> Result<Box<dyn CandidateWorkspace>, WorkspaceError>;
}

/// Orchestrator MCP pre-fetch (issue #248): gathered ONCE before a
/// best-of-N fan-out and folded into every candidate's shared message
/// history, instead of N candidates each independently paying to look up the
/// same external context — the common "candidates all need the same DB
/// schema / ticket context" case.
///
/// Consulted exactly where the isolated-candidate path runs
/// (`Pipeline::run_best_of_n`), which is `candidates > 1` **and** the
/// single-candidate authored-witness run — an authored witness needs a
/// disposable workspace even at N=1, so it takes the same route. The plain
/// single-shot path never reaches it.
#[async_trait]
pub trait McpPrefetchPort: Send + Sync {
    /// Best-effort: `None` when there is nothing worth injecting (no
    /// candidate-safe servers connected, every call failed, or every call
    /// returned nothing) — a prefetch miss never aborts the run.
    async fn prefetch(&self, goal: &str) -> Option<String>;
}

/// A human's decision at the scope-review gate (L-E5). `Trim` carries the
/// indices (into the proposed plan) the user chose to keep, so the pipeline
/// executes a reduced plan rather than the whole thing.
#[derive(Debug, Clone, PartialEq, Eq)]
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
        let answer = line.trim();
        match answer.to_ascii_lowercase().as_str() {
            "y" | "yes" => ScopeDecision::Approve,
            "" | "n" | "no" => ScopeDecision::Abort,
            _ => ScopeDecision::Revise {
                note: answer.to_string(),
            },
        }
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

/// The ports the pipeline orchestrates over. The `stella-cli` glue fills this
/// with real subsystem adapters; tests fill it with scripted doubles. Grouped
/// into one struct so [`crate::pipeline::Pipeline::new`] stays a two-argument
/// constructor rather than a nine-parameter one.
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
    /// The file recorder's own mutation tally, for the evidence the judge is
    /// shown. Wire it from the registry the host attaches its event stream to;
    /// hosts with no recorder pass [`NoFileTouches`].
    pub touches: &'a dyn FileTouchPort,
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
    /// witness author, and the judge are autonomous sub-steps with no
    /// user-facing "steer this" moment. The pipeline's stop remains the
    /// caller's hard cancel — a pipeline is triage→…→judge, so a
    /// mid-execute soft stop has no single obvious continuation.
    pub steering: Option<&'a dyn stella_core::ports::TurnSteering>,
}

#[cfg(test)]
mod tests {
    use super::*;

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
        for kind in [CmdKind::TimedOut, CmdKind::Infra] {
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
        };
        assert_eq!(
            AlwaysAbortGate.review(&proposal).await,
            ScopeDecision::Abort
        );
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
