//! `verify_done` — the deterministic definition of done.
//!
//! A change is done when a *witness test* proves it: the test FAILS against
//! the previous version of the code and PASSES against the new version.
//! Either half alone is worthless — a test that passes on the new code but
//! also passed on the old code witnesses nothing (it would have been green
//! without your change: vacuous, or the feature already existed); a test
//! that fails on the new code means the work isn't done. Only the pair
//! (old→fail, new→pass) is a completed unit of work. This is the
//! shadow-revert mutation-witness gate, as a tool.
//!
//! # How the "previous version" is produced — without touching your tree
//!
//! The working tree is NEVER mutated (no stash, no checkout —). Instead a
//! detached shadow git worktree is created at the *witness baseline*, the
//! *test* files (only) are copied from the working tree into it, and the
//! test command runs there. The copied test files are the witness. The
//! shadow worktree is removed afterward, success or failure.
//!
//! # Which commit is the previous version
//!
//! `HEAD` is only the default. Inside a pipeline candidate workspace the
//! pipeline itself commits the agent's work after every verified step, so by
//! witness time `HEAD` *is* the solved tree and a flip against it is
//! structurally impossible (#2067). The baseline is resolved in preference
//! order:
//!
//! 1. [`WITNESS_BASELINE_WORKTREE_REF`] — pinned by the candidate workspace
//!    at creation (per-worktree, so parallel candidates never collide and
//!    nothing leaks into the user's real checkout);
//! 2. [`WITNESS_BASELINE_TASK_REF`] — pinned by an orchestrating harness
//!    when it gives a task workspace its baseline commit;
//! 3. `HEAD`, after walking first-parent history past any pipeline seal
//!    commits sitting on top of it;
//! 4. and when every reachable commit is such a snapshot, an honest refusal:
//!    no true baseline exists, which is a fact about the workspace — never a
//!    `VACUOUS` accusation that sends the author off strengthening a test
//!    that was not the problem.
//!
//! # Cancellation
//!
//! "Success or failure" used to exclude a third outcome: the future being
//! dropped (Ctrl-C, Esc, a session timeout, a caller `select!`). That skipped
//! `cleanup_shadow` entirely and stranded both a `/tmp` directory and a
//! `.git/worktrees` registration that no later run removed (#613). The
//! teardown is now split along the one line that matters — whether the
//! syscall is synchronous:
//!
//! - the `/tmp` directory is removed by `ShadowDirGuard`, a synchronous
//!   `Drop` guard, which is the only kind that still runs during runtime
//!   shutdown;
//! - the git registration is reclaimed by a `git worktree prune` at the start
//!   of the *next* `verify_done`, because removing it requires an awaited
//!   subprocess.
//!
//! # When the setup, not the test, is what defeated the proof
//!
//! A `cd` to an absolute path in `test_cmd` walks the shadow run out of the
//! tree it was aimed at; naming the deliverable in `test_files` layers it onto
//! the baseline. Both make a correct test look vacuous, and the verdict used
//! to blame the test for each. The `defeater` submodule beside this one
//! (`src/verify/defeater.rs`) detects them (#2871).
//!
//! # Reading the verdict
//!
//! The shadow run's output tail is always included: a shadow failure that
//! is a *compile error* (the test references symbols that don't exist on
//! HEAD) is a much weaker witness than an assertion failure — the agent
//! (and any verifier) should check the tail for WHY the old code failed.
//!
//! # Creation tasks
//!
//! A previous-code run that dies while *loading* the test is normally not a
//! flip — a stale build artifact produces the same text. On a creation task
//! it can be the whole proof, because the deliverable is absent at the
//! baseline by construction. The `creation` submodule beside this one
//! (`src/verify/creation.rs`) carries the distinction, which is a git
//! question rather than a textual one, and the cost of getting it wrong in
//! either direction (#2668).
//!
//! # Running the same proof twice
//!
//! The shadow half is a pure function of `(baseline, test files, command,
//! timeout)` — nothing else in the working tree is visible from inside a
//! fresh checkout — so an identical call inside one turn reuses its
//! observation rather than rebuilding for it ([`ShadowMemo`], #2208). The
//! working-tree half is never reused.
//!
//! # Where the wall clock goes
//!
//! Neither half, as it turns out. Every boundary a call crosses is timed and
//! reported to the diagnostic plane as one `tools.verify_done.phases` record.
//! The `phases` submodule beside this one (`src/verify/phases.rs`) carries
//! why the measurement was needed (#2486) and why it cannot ride the tool's
//! own output.

mod cap;
mod creation;
mod defeater;
mod instrument;
mod memo;
mod phases;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::tool::{ToolOutput, ToolSchema};
use tokio::process::Command;

use memo::{ShadowKey, ShadowObservation};
pub use memo::{ShadowMemo, ShadowMemoHandle};
use phases::{Phase, Phases, Verdict};

use crate::registry::Tool;

const DEFAULT_TIMEOUT_SECS: u64 = 300;
const TAIL_BYTES: usize = 4_000;

#[derive(Default)]
pub struct VerifyDone {
    /// This session's turn-scoped shadow-run memo, shared with the
    /// [`crate::registry::ToolRegistry`] that brackets the turn. Default
    /// (unshared, never armed) is inert, so a standalone instance behaves
    /// exactly as it did before the memo existed.
    memo: ShadowMemoHandle,
    /// Where this call's [`Phases`] record goes, when the host wired a
    /// diagnostic handle through
    /// [`RegistryOptions::diagnostics`](crate::RegistryOptions::diagnostics).
    /// `None` is inert: the phases are still measured (an `Instant` costs
    /// nothing worth branching on) and simply not reported, so an unwired
    /// host and a wired one execute the same code.
    diagnostics: Option<Arc<stella_diag::Dx>>,
}

impl VerifyDone {
    /// Register with the memo the owning registry brackets.
    pub fn with_memo(memo: ShadowMemoHandle) -> Self {
        Self {
            memo,
            diagnostics: None,
        }
    }

    /// The full seam: the turn's memo, and where to report per-call timings.
    pub fn new(memo: ShadowMemoHandle, diagnostics: Option<Arc<stella_diag::Dx>>) -> Self {
        Self { memo, diagnostics }
    }
}

/// Run `command` via `bash -c` in `dir` with a process-group kill on
/// timeout — the shared runner in [`crate::exec`]. Its 30k middle-out cap is
/// invisible here: `tail()` keeps only the last [`TAIL_BYTES`], which the
/// cap's preserved tail half always contains.
async fn run(
    command: &str,
    dir: &std::path::Path,
    timeout_secs: u64,
) -> Result<(i32, String), String> {
    crate::exec::run(command, dir, timeout_secs, None).await
}

fn tail(s: &str) -> &str {
    if s.len() <= TAIL_BYTES {
        return s;
    }
    let start = s.len() - TAIL_BYTES;
    // Snap forward to a char boundary.
    let mut idx = start;
    while !s.is_char_boundary(idx) {
        idx += 1;
    }
    &s[idx..]
}

/// A `git` command aimed at `dir` and ONLY `dir`: repo-targeting env vars a
/// surrounding git hook may have exported are scrubbed so they cannot
/// redirect the invocation at the outer repository — the shared spawn policy
/// ([`crate::subprocess_env::scrub_spawn_env`]).
fn git_in(dir: &std::path::Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir);
    crate::subprocess_env::scrub_spawn_env(&mut cmd);
    cmd
}

/// Per-worktree ref a pipeline candidate workspace pins at creation, naming
/// the session-baseline commit — the tree as it stood before the agent's
/// work began. Lives under `refs/worktree/` so each candidate carries its
/// own pin and nothing ever appears in the user's real checkout.
pub const WITNESS_BASELINE_WORKTREE_REF: &str = "refs/worktree/stella/witness-baseline";

/// Repo-wide ref an orchestrating harness pins when it creates a task
/// workspace's baseline commit (the bench adapter's
/// `stella-harbor: task workspace baseline`). Only consulted when no
/// per-worktree pin exists.
pub const WITNESS_BASELINE_TASK_REF: &str = "refs/stella/task-baseline";

/// The committer email on candidate snapshot plumbing commits —
/// `stella-cli`'s `SNAPSHOT_IDENT`, parity-tested there so the two
/// spellings cannot drift.
pub const CANDIDATE_SNAPSHOT_EMAIL: &str = "pipeline@stella.invalid";

/// The subject of the seal commits the pipeline stacks onto a candidate
/// workspace after each verified step — the auto-snapshots that advance
/// `HEAD` past the session baseline. `stella-cli`'s `seal_inner` uses this
/// constant directly, so the walk below and the writer share one spelling.
pub const CANDIDATE_SEAL_SUBJECT: &str = "stella: candidate verified snapshot";

/// How far back the seal walk looks for a true baseline. A session seals
/// once per verified step, so a history that is snapshots 500 deep with no
/// base underneath is not a walk cut short — it is a workspace with no
/// baseline to find.
const SEAL_WALK_LIMIT: &str = "500";

/// The commit the flip is measured against, plus the provenance a reader
/// needs to audit that choice — embedded in every verdict.
struct WitnessBaseline {
    sha: String,
    provenance: String,
}

impl WitnessBaseline {
    fn short(&self) -> &str {
        &self.sha[..self.sha.len().min(8)]
    }
}

/// Resolve `name` to a commit SHA, or `None` when the ref does not exist.
async fn pinned_baseline(root: &std::path::Path, name: &str) -> Option<String> {
    let mut cmd = git_in(root);
    cmd.args(["rev-parse", "--verify", "--quiet"]);
    cmd.arg(format!("{name}^{{commit}}"));
    match crate::exec::run_captured(cmd, 30).await {
        crate::exec::Captured::Done(out) if out.status.success() => {
            let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (!sha.is_empty()).then_some(sha)
        }
        _ => None,
    }
}

/// Walk first-parent history from `HEAD`, skipping the pipeline's seal
/// commits, and return the first real commit plus how many seals were
/// skipped — or `None` when every visited commit was a seal.
async fn walk_past_seals(root: &std::path::Path) -> Result<Option<(String, usize)>, String> {
    let mut cmd = git_in(root);
    cmd.args([
        "log",
        "--first-parent",
        "-n",
        SEAL_WALK_LIMIT,
        "--format=%H%x1f%ae%x1f%s",
        "HEAD",
    ]);
    let out = match crate::exec::run_captured(cmd, 30).await {
        crate::exec::Captured::Done(out) if out.status.success() => out,
        _ => return Err("git log failed while resolving the witness baseline".to_string()),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut skipped = 0usize;
    for line in text.lines() {
        let mut parts = line.splitn(3, '\u{1f}');
        let sha = parts.next().unwrap_or_default();
        let email = parts.next().unwrap_or_default();
        let subject = parts.next().unwrap_or_default();
        if sha.is_empty() {
            continue;
        }
        if email == CANDIDATE_SNAPSHOT_EMAIL && subject == CANDIDATE_SEAL_SUBJECT {
            skipped += 1;
            continue;
        }
        return Ok(Some((sha.to_string(), skipped)));
    }
    Ok(None)
}

/// Choose the previous-version commit per the module-level preference order.
/// `Err` carries a complete, user-facing refusal message.
async fn resolve_witness_baseline(
    root: &std::path::Path,
    head: &str,
) -> Result<WitnessBaseline, String> {
    if let Some(sha) = pinned_baseline(root, WITNESS_BASELINE_WORKTREE_REF).await {
        return Ok(WitnessBaseline {
            sha,
            provenance: format!("pinned by {WITNESS_BASELINE_WORKTREE_REF}"),
        });
    }
    if let Some(sha) = pinned_baseline(root, WITNESS_BASELINE_TASK_REF).await {
        return Ok(WitnessBaseline {
            sha,
            provenance: format!("pinned by {WITNESS_BASELINE_TASK_REF}"),
        });
    }
    match walk_past_seals(root).await? {
        Some((_, 0)) => Ok(WitnessBaseline {
            sha: head.to_string(),
            provenance: "HEAD".to_string(),
        }),
        Some((sha, skipped)) => Ok(WitnessBaseline {
            sha,
            provenance: format!(
                "HEAD with {skipped} pipeline snapshot commit{} skipped",
                if skipped == 1 { "" } else { "s" }
            ),
        }),
        None => Err(format!(
            "WITNESS BASELINE UNRESOLVED — every commit reachable from HEAD is a pipeline \
             snapshot of the agent's own work (`{CANDIDATE_SEAL_SUBJECT}`), and no baseline \
             ref is pinned, so no pre-change tree exists to measure a flip against. This is \
             a fact about the workspace, NOT about your test — do not weaken or rewrite the \
             test in response. An orchestrator that creates such workspaces should pin the \
             true baseline: `git update-ref {WITNESS_BASELINE_TASK_REF} <baseline-commit>`."
        )),
    }
}

/// Best-effort removal of the shadow worktree — both the registration and
/// the directory.
async fn cleanup_shadow(root: &std::path::Path, shadow: &std::path::Path) {
    let _ = git_in(root)
        .args(["worktree", "remove", "--force"])
        .arg(shadow)
        .output()
        .await;
    let _ = tokio::fs::remove_dir_all(shadow).await;
    let _ = git_in(root).args(["worktree", "prune"]).output().await;
}

/// Drop half of the cancellation fix (#613): removes the shadow worktree's
/// `/tmp` **directory** synchronously.
///
/// [`cleanup_shadow`] cannot be an RAII guard — it awaits three git
/// subprocesses and `tokio::fs::remove_dir_all`, and `Drop` cannot await.
/// Spawning it from `Drop` is worse than useless: during runtime shutdown, the
/// exact case a cancelled `verify_done` is in, the spawn silently does nothing.
/// So the teardown is split by what each half needs:
///
/// - the directory goes here, because `std::fs::remove_dir_all` is one
///   synchronous syscall that needs no runtime;
/// - the `.git/worktrees/<name>` **registration** cannot, so it is reclaimed
///   by the `git worktree prune` that every `verify_done` now runs at start
///   (cheap, idempotent, and it also clears registrations stranded by earlier
///   releases).
///
/// Idempotent with `cleanup_shadow`: on every non-cancelled path the directory
/// is already gone by the time this drops and `remove_dir_all` is a no-op on a
/// missing path, so the guard stays armed rather than carrying a flag that a
/// future early-return could forget to set.
struct ShadowDirGuard {
    shadow: std::path::PathBuf,
}

impl Drop for ShadowDirGuard {
    fn drop(&mut self) {
        // Blocking, but bounded: a shadow worktree is a checkout of HEAD in
        // `std::env::temp_dir()`, and this only runs on the cancellation path.
        let _ = std::fs::remove_dir_all(&self.shadow);
    }
}

/// Run the witness test against the previous version of the code: a detached
/// shadow worktree at `baseline_sha`, with `resolved`'s test files (and
/// nothing else from the working tree) layered in.
///
/// This is the expensive half — a fresh checkout and a cold build — and it is
/// a pure function of its arguments, which is exactly what makes [`memo`]
/// sound. Keep it that way: a reader added here would silently widen what a
/// memo key has to cover.
///
/// `phases` is the one exception, and it is not one: it is written and never
/// read, so nothing it carries can reach the result. A memo key covers what
/// *determines* the observation, and how long producing it took is the one
/// fact about a run that must never be part of that.
///
/// `Err` carries a complete, user-facing message; `Ok` is the run's
/// `(exit code, output)`.
async fn run_against_baseline(
    root: &std::path::Path,
    baseline_sha: &str,
    resolved: &[(String, std::path::PathBuf, std::path::PathBuf)],
    root_rel: &std::path::Path,
    test_cmd: &str,
    timeout_secs: u64,
    phases: &mut Phases,
) -> Result<(i32, String), String> {
    let add = Phase::start();
    // Shadow names carry pid + a process-wide counter: two concurrent
    // verify_done calls (parallel tools, parallel tests) must never collide on
    // the same worktree path — a timestamp alone can.
    static SHADOW_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let shadow = std::env::temp_dir().join(format!(
        "stella_verify_{}_{}",
        std::process::id(),
        SHADOW_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let added = git_in(root)
        .args(["worktree", "add", "--detach"])
        .arg(&shadow)
        .arg(baseline_sha)
        .output()
        .await;
    // Stopped before the match, so a checkout that failed *slowly* — the
    // interesting case, and the one a lock contention or a cold object store
    // produces — is measured rather than discarded with its error.
    phases.worktree_add = add.stop();
    match added {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            return Err(format!(
                "could not create the shadow worktree for the previous version: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Err(e) => return Err(format!("could not run git worktree add: {e}")),
    }
    // Armed the instant the directory exists. Everything below awaits — the
    // file copies, and above all the shadow test run, which is where a
    // cancelled turn actually lands — and none of it is reached when this
    // future is dropped.
    let _shadow_dir = ShadowDirGuard {
        shadow: shadow.clone(),
    };

    let stage = Phase::start();
    let staged = stage_witness_files(&shadow, resolved).await;
    phases.stage_tests = stage.stop();
    if let Err(message) = staged {
        let teardown = Phase::start();
        cleanup_shadow(root, &shadow).await;
        phases.cleanup = teardown.stop();
        return Err(message);
    }

    // Run in the shadow subdirectory matching the workspace root, so a
    // relative `test_cmd` (e.g. `cargo test`) resolves the same package it
    // would in the real working tree — not the repo toplevel.
    let half_two = Phase::start();
    let shadow_run = run(test_cmd, &shadow.join(root_rel), timeout_secs).await;
    phases.shadow_run = half_two.stop();
    let teardown = Phase::start();
    cleanup_shadow(root, &shadow).await;
    phases.cleanup = teardown.stop();
    shadow_run.map_err(|e| format!("shadow run against the previous version failed to run: {e}"))
}

/// Layer ONLY the test files onto the previous version.
///
/// Split out of [`run_against_baseline`] so the copy loop has one exit and can
/// therefore be timed as one phase — the caller owns the shadow's teardown on
/// both paths, which it already did on every other error.
async fn stage_witness_files(
    shadow: &std::path::Path,
    resolved: &[(String, std::path::PathBuf, std::path::PathBuf)],
) -> Result<(), String> {
    for (rel, src, relpath) in resolved {
        let dst = shadow.join(relpath);
        if let Some(parent) = dst.parent()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return Err(format!(
                "could not stage test file `{rel}` in the shadow: {e}"
            ));
        }
        if let Err(e) = tokio::fs::copy(src, &dst).await {
            return Err(format!(
                "could not copy test file `{rel}` into the shadow: {e}"
            ));
        }
    }
    Ok(())
}

#[async_trait]
impl Tool for VerifyDone {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "verify_done".into(),
            description: "Prove a change is done: test_cmd must PASS on your code and FAIL on \
                          the pre-change baseline (git HEAD, or the pinned session/task \
                          baseline in orchestrated workspaces) with your test files layered \
                          in. Call before declaring any implementation complete. Never \
                          mutates the working tree."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "test_cmd": { "type": "string", "description": "Command that runs the witness test(s), e.g. `cargo test -p my-crate my_test` or `pnpm vitest run path/to/file.test.ts`" },
                    "test_files": { "type": "array", "items": { "type": "string" }, "description": "Workspace-relative paths of the NEW or CHANGED test files that witness this change" },
                    "timeout_secs": { "type": "integer", "description": "Per-run timeout in seconds (default 300, max 600)" }
                },
                "required": ["test_cmd", "test_files"]
            }),
            read_only: false,
            speculation_safe: false,
        }
    }

    /// Time the whole call, then report one `tools.verify_done.phases` record
    /// — whatever the verdict, including the refusals that never build a
    /// shadow at all.
    /// Reporting only the calls that got far enough to be slow would censor
    /// the fast end of the distribution and flatter every percentile #2486
    /// exists to measure.
    async fn execute(&self, input: &Value, root: &std::path::Path) -> ToolOutput {
        let call = Phase::start();
        let mut phases = Phases::default();
        let (output, verdict) = match self.verify(input, root, &mut phases).await {
            Ok(pair) => pair,
            Err(message) => (ToolOutput::Error { message }, Verdict::Error),
        };
        phases.verdict = verdict;
        // The turn's rejection run (#2863), folded in at the one exit every
        // verdict passes through — so no future arm can forget to count.
        self.memo.record_verdict(verdict);
        // Last, so it spans everything above it including the teardown.
        phases.total = call.stop();
        if let Some(dx) = &self.diagnostics {
            phases.emit(dx);
        }
        output
    }
}

impl VerifyDone {
    /// The call itself, with each phase's wall clock written into `phases`.
    ///
    /// `Err` is a named refusal — an input this tool cannot act on, or a
    /// workspace it cannot measure a flip in — and reaches the model as
    /// `ToolOutput::Error` exactly as it did before, through the one arm in
    /// [`Self::execute`]. `Ok` carries the four real verdicts *with* the
    /// [`Verdict`] each one is, because the alternative is re-deriving it by
    /// sniffing the prose this function just wrote, which is what
    /// `ToolOutput`'s own doc comment forbids.
    async fn verify(
        &self,
        input: &Value,
        root: &std::path::Path,
        phases: &mut Phases,
    ) -> Result<(ToolOutput, Verdict), String> {
        // Before the argument is even read: this turn may already have bought
        // this answer twice, and the saving is only real if the refused call
        // resolves no baseline and checks out no shadow (#2863).
        if let Some(class) = self.memo.gate_closed() {
            return Err(cap::refusal(class));
        }
        let setup = Phase::start();
        let test_cmd = crate::input::required_str(input, "test_cmd").map_err(|e| e.to_string())?;
        // Before anything expensive, and refused rather than rewritten (#2871).
        if let Some(refusal) = defeater::cwd_refusal(test_cmd) {
            return Err(refusal);
        }
        // A mistyped `test_files` (a bare string, or an array carrying a
        // non-string element) must be refused as the type problem it is —
        // silently emptying it via `filter_map(as_str)` reported the field
        // as missing when it was present (#3144).
        let test_files: Vec<String> = crate::input::optional_str_array(input, "test_files")
            .map_err(|err| err.to_string())?
            .map(|files| files.into_iter().map(str::to_string).collect())
            .unwrap_or_default();
        if test_files.is_empty() {
            return Err(
                "missing required field `test_files` — name the new/changed test \
                        file(s) that witness this change"
                    .to_string(),
            );
        }
        phases.test_files = u32::try_from(test_files.len()).unwrap_or(u32::MAX);
        let timeout_secs = crate::exec::timeout_from(input, DEFAULT_TIMEOUT_SECS);

        // The shadow-worktree copy destination must be derived from the
        // canonical path *relative to the root*, never the raw model-supplied
        // string: an absolute `file` would make `shadow.join(file)` discard the
        // shadow prefix and resolve back to the real working-tree file, and
        // `fs::copy(src, src)` truncates it — silently emptying the user's test
        // file and violating the "NEVER mutates the working tree" contract.
        let canon_root = root
            .canonicalize()
            .map_err(|e| format!("could not canonicalize the workspace root: {e}"))?;
        // The shadow worktree mirrors the git TOPLEVEL, not the workspace root
        // — which may be a subdirectory of the repo. So test-file destinations
        // must be relative to the toplevel, and the shadow test must run in the
        // subdirectory that corresponds to the workspace root. Assuming
        // root == toplevel produced a false verdict (a false WITNESS CONFIRMED
        // or false VACUOUS) whenever verify_done ran from a repo subdirectory.
        // Parsed from stdout ALONE ([`crate::exec::run_captured`]): the shared
        // `run` merges stderr into its output, so any git chatter on stderr
        // (GIT_TRACE, advice hints) corrupted the parsed path.
        let canon_toplevel = {
            let mut cmd = git_in(root);
            cmd.args(["rev-parse", "--show-toplevel"]);
            match crate::exec::run_captured(cmd, 30).await {
                crate::exec::Captured::Done(out) if out.status.success() => {
                    let p = std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
                    p.canonicalize().unwrap_or(p)
                }
                // Not a git repo (or older git): the HEAD check below reports it.
                _ => canon_root.clone(),
            }
        };
        let root_rel = canon_root
            .strip_prefix(&canon_toplevel)
            .unwrap_or(std::path::Path::new(""))
            .to_path_buf();
        // Every test file must resolve inside the workspace and exist. Each
        // entry is `(display_name, canonical_src, toplevel_relative_dst)` — the
        // destination is relative to the git TOPLEVEL so `shadow.join(dst)`
        // lands at the file's real position in the repo tree.
        let mut resolved: Vec<(String, std::path::PathBuf, std::path::PathBuf)> = Vec::new();
        for file in &test_files {
            match crate::resolve_within_root(root, file) {
                Some(path) if path.is_file() => {
                    let relpath = path.strip_prefix(&canon_toplevel).map_err(|_| {
                        format!("test file `{file}` resolved outside the git repository")
                    })?;
                    resolved.push((file.clone(), path.clone(), relpath.to_path_buf()));
                }
                Some(_) => {
                    return Err(format!(
                        "test file `{file}` does not exist in the workspace"
                    ));
                }
                None => {
                    return Err(format!("test file `{file}` escapes the workspace root"));
                }
            }
        }

        // The previous version is git HEAD — a repo is required. Same
        // stdout-only capture as the toplevel above: stderr chatter appended
        // to the SHA made every downstream git call fail on a garbage ref.
        let head = {
            let mut cmd = git_in(root);
            cmd.args(["rev-parse", "HEAD"]);
            match crate::exec::run_captured(cmd, 30).await {
                crate::exec::Captured::Done(out) if out.status.success() => {
                    String::from_utf8_lossy(&out.stdout).trim().to_string()
                }
                other => {
                    // stderr is kept for diagnostics — it says WHY (no repo,
                    // no commits yet) without polluting the parsed value.
                    let detail = match &other {
                        crate::exec::Captured::Done(out) => {
                            let stderr = String::from_utf8_lossy(&out.stderr);
                            let stderr = stderr.trim();
                            if stderr.is_empty() {
                                String::new()
                            } else {
                                format!("\n--- git stderr ---\n{}", tail(stderr))
                            }
                        }
                        _ => String::new(),
                    };
                    return Err(format!(
                        "verify_done requires a git repository: the previous version of the \
                         code is defined as git HEAD{detail}"
                    ));
                }
            }
        };
        phases.setup = setup.stop();

        // Prune-on-start (#613): reclaim `.git/worktrees` registrations left
        // by a PREVIOUS run whose future was dropped. Such a run could only
        // remove its `/tmp` directory synchronously ([`ShadowDirGuard`]);
        // unregistering needs an awaited git call, which a `Drop` cannot make.
        // It runs here — before either half, as soon as the repo is known to
        // exist — so a NOT DONE or VACUOUS verdict, which never reaches
        // `cleanup_shadow`, still collects the debris. `prune` drops only
        // registrations whose directory is already gone, so it is cheap,
        // idempotent, and can never disturb a live worktree (this run's or a
        // concurrent one's).
        let prune = Phase::start();
        let _ = git_in(root).args(["worktree", "prune"]).output().await;
        phases.prune = prune.stop();

        // Which commit is "the previous version" — see the module docs. HEAD
        // is only the default: inside a candidate workspace the pipeline has
        // already committed the work under proof on top of the baseline, and
        // comparing against it makes every real flip structurally impossible
        // while a missing build artifact fakes one (#2067).
        let resolve = Phase::start();
        let baseline = resolve_witness_baseline(root, &head).await;
        phases.baseline = resolve.stop();
        let baseline = baseline?;

        // Half 1: the new code must pass.
        let half_one = Phase::start();
        let new_run = run(test_cmd, root, timeout_secs).await;
        phases.working_tree_run = half_one.stop();
        let (new_exit, new_output) = new_run?;
        if new_exit != 0 {
            // Did it run and fail, or never run? Only the first is evidence
            // about the code; answering both with `NOT DONE` sent an agent
            // after its own harness for fifteen consecutive calls (#2872).
            if let Some(broken) = instrument::working_tree_failure(new_exit, &new_output) {
                return Ok((
                    ToolOutput::Error {
                        message: instrument::refusal(broken, test_cmd, new_exit, tail(&new_output)),
                    },
                    Verdict::InstrumentBroken,
                ));
            }
            return Ok((
                ToolOutput::Error {
                    message: format!(
                        "NOT DONE — the witness test fails on your NEW code (exit {new_exit}). \
                         Fix the implementation (or the test) and retry.\n--- output tail \
                         ---\n{}",
                        tail(&new_output)
                    ),
                },
                Verdict::NotDone,
            ));
        }

        // Half 2: the previous code (the baseline + your new tests) must fail.
        // An identical call earlier in this turn already answered this: the
        // shadow sees only the baseline commit, the copied test files, and the
        // command, so a memo over exactly those is the whole determinant of
        // the run rather than a guess at it (#2208 — 250s, 24% of a trial's
        // budget, spent re-deriving one observation on `extract-elf`). No key
        // means no memo: the run happens.
        let memo_key = ShadowKey::compute(
            &canon_toplevel,
            &root_rel,
            &baseline.sha,
            test_cmd,
            timeout_secs,
            &resolved,
        )
        .await;
        let (old_exit, old_output, reused) =
            match memo_key.as_ref().and_then(|key| self.memo.get(key)) {
                Some(observed) => (observed.exit, observed.output, true),
                None => {
                    let (exit, output) = run_against_baseline(
                        root,
                        &baseline.sha,
                        &resolved,
                        &root_rel,
                        test_cmd,
                        timeout_secs,
                        phases,
                    )
                    .await?;
                    if let Some(key) = memo_key {
                        self.memo.insert(
                            key,
                            ShadowObservation {
                                exit,
                                output: output.clone(),
                            },
                        );
                    }
                    (exit, output, false)
                }
            };
        phases.shadow_reused = reused;
        // Every verdict below says whether the previous-code run behind it
        // happened on this call. A reader auditing a proof must never have to
        // guess that, and the truncated tail it is reading is now evidence
        // from a run they cannot see in this call's wall clock.
        let reuse_note = if reused {
            "\n(previous-code observation REUSED from an identical verify_done call earlier in \
             this turn — same baseline, same test files, same command, and nothing else in the \
             working tree is visible from the shadow)"
        } else {
            ""
        };

        if old_exit == 0 {
            // Which defeater this names comes from observations of the
            // workspace, never from the guesses it used to offer (#2871).
            let message = defeater::vacuous_verdict(
                root,
                &baseline,
                &resolved,
                reuse_note,
                tail(&old_output),
            )
            .await;
            return Ok((ToolOutput::Error { message }, Verdict::Vacuous));
        }

        // A failure while LOADING the test is not a failure OF the test:
        // classify before crediting, so a missing build artifact in the fresh
        // shadow checkout cannot fake a flip (#2067). Exception (#2668): on a
        // creation task the "missing" thing named in the failure can BE the
        // deliverable, absent at baseline by construction — mechanically
        // distinguished from a stale artifact by asking git whether the named
        // file is new relative to the baseline, never by trusting a claim.
        if let Some(label) = instrument::import_failure_signature(&old_output) {
            let flip = creation::ImportFlip {
                label,
                test_cmd,
                baseline: &baseline,
                old_exit,
                old_output: &old_output,
                reuse_note,
            };
            if let Some(content) = creation::confirm(root, &flip).await {
                return Ok((ToolOutput::Ok { content }, Verdict::Confirmed));
            }
            return Ok((
                ToolOutput::Error {
                    message: format!(
                        "WITNESS_INCONCLUSIVE_BUILD_ERROR — the previous-code run failed with \
                     {label}, so the baseline could not be built or imported and no \
                     behavioural assertion ever ran: this failure is NOT evidence that your \
                     change is what flipped it. Two common causes, each with a fix:\n\
                     - a build artifact (compiled `.so`, `node_modules/`, a target dir) \
                     exists in your working tree but not in a fresh checkout → make \
                     `test_cmd` build both sides from source as part of the command;\n\
                     - the missing module IS your change's deliverable → assert its \
                     observable behaviour instead of bare-importing it (e.g. guard the \
                     import and fail on the assertion), so the old code fails on the \
                     assertion rather than the import.\n\
                     - new code:      `{test_cmd}` exit 0 (PASS)\n\
                     - previous code: baseline {} ({}) → exit {old_exit}, before any test \
                     ran{reuse_note}\n\
                     --- previous-code output tail ---\n{}",
                        baseline.short(),
                        baseline.provenance,
                        tail(&old_output)
                    ),
                },
                Verdict::InconclusiveBuildError,
            ));
        }

        Ok((
            ToolOutput::Ok {
                content: format!(
                    "WITNESS CONFIRMED — deterministic definition of done met:\n\
                     - new code:      `{test_cmd}` exit 0 (PASS)\n\
                     - previous code: baseline {} ({}) + your test files → exit {old_exit} \
                     (FAIL){reuse_note}\n\
                     Check the tail below: an assertion failure is a strong witness; a compile \
                     error (test references symbols that don't exist on the baseline) is weaker \
                     — prefer behavioral assertions when possible.\n\
                     --- previous-code failure tail ---\n{}",
                    baseline.short(),
                    baseline.provenance,
                    tail(&old_output)
                ),
            },
            Verdict::Confirmed,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a tiny git repo with a committed "previous version" and an
    /// uncommitted "new version" + witness test, both as shell scripts (no
    /// toolchain dependency: the "test" greps the implementation file).
    pub(super) async fn scaffold(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "stella_verify_test_{tag}_{}_{}",
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
        ] {
            scratch_git(&root, args);
        }
        std::fs::write(root.join("impl.txt"), "old behavior\n").unwrap();
        for args in [
            &["add", "."][..],
            &["commit", "-q", "-m", "previous version"],
        ] {
            scratch_git(&root, args);
        }
        root
    }

    /// Run `git <args>` in `root` with hook-exported GIT_* vars scrubbed
    /// exactly like the production paths (`git_in`) — without this, running
    /// the suite from inside a git hook (the pre-push gate) re-targets every
    /// command at the HOST repo. Returns stdout; panics on failure.
    pub(super) fn scratch_git(root: &std::path::Path, args: &[&str]) -> String {
        let mut cmd = std::process::Command::new("git");
        cmd.args(args).current_dir(root);
        for var in crate::exec::GIT_REPO_ENV_VARS {
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

    /// The #3144 witness: a mistyped `test_files` names the type problem.
    /// On main a bare string (or an array of non-strings) was silently
    /// emptied by `filter_map(as_str)` and refused as "missing required
    /// field `test_files`" — present, mistyped, and misreported.
    #[tokio::test]
    async fn a_mistyped_test_files_is_refused_as_the_type_problem_it_is() {
        let root = tempfile::tempdir().unwrap();
        let out = VerifyDone::default()
            .execute(
                &serde_json::json!({ "test_cmd": "bash w.sh", "test_files": "w.sh" }),
                root.path(),
            )
            .await;
        let ToolOutput::Error { message } = out else {
            panic!("expected a refusal, got {out:?}");
        };
        assert_eq!(
            message,
            "field `test_files` must be an array of strings, got string"
        );

        let out = VerifyDone::default()
            .execute(
                &serde_json::json!({ "test_cmd": "bash w.sh", "test_files": [1, "w.sh"] }),
                root.path(),
            )
            .await;
        let ToolOutput::Error { message } = out else {
            panic!("expected a refusal, got {out:?}");
        };
        assert_eq!(
            message,
            "field `test_files`[0] must be a string, got number"
        );
    }

    #[tokio::test]
    async fn confirmed_witness_when_old_fails_and_new_passes() {
        let root = scaffold("confirmed").await;
        // New implementation (uncommitted) and a test that requires it.
        std::fs::write(root.join("impl.txt"), "new behavior\n").unwrap();
        std::fs::write(root.join("witness.sh"), "grep -q 'new behavior' impl.txt\n").unwrap();

        let out = VerifyDone::default()
            .execute(
                &serde_json::json!({
                    "test_cmd": "bash witness.sh",
                    "test_files": ["witness.sh"],
                    "timeout_secs": 60
                }),
                &root,
            )
            .await;
        match &out {
            ToolOutput::Ok { content } => {
                assert!(content.contains("WITNESS CONFIRMED"), "{content}")
            }
            other => panic!("expected confirmation, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// git chatter on stderr (here GIT_TRACE, in the wild advice/warning
    /// hints) must not corrupt the parsed toplevel path or HEAD SHA: both
    /// used to come from the shared runner's MERGED stdout+stderr, so the
    /// trace lines rode along into `git worktree add <garbage-ref>`.
    #[tokio::test]
    async fn stderr_chatter_does_not_corrupt_rev_parse_results() {
        let root = scaffold("chatter").await;
        std::fs::write(root.join("impl.txt"), "new behavior\n").unwrap();
        std::fs::write(root.join("witness.sh"), "grep -q 'new behavior' impl.txt\n").unwrap();

        let _trace = crate::subprocess_env::test_support::ScopedEnvVar::set("GIT_TRACE", "1");
        let out = VerifyDone::default()
            .execute(
                &serde_json::json!({
                    "test_cmd": "bash witness.sh",
                    "test_files": ["witness.sh"],
                    "timeout_secs": 60
                }),
                &root,
            )
            .await;
        match &out {
            ToolOutput::Ok { content } => {
                assert!(content.contains("WITNESS CONFIRMED"), "{content}")
            }
            other => panic!("stderr chatter broke the verdict: {other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn vacuous_test_is_rejected() {
        let root = scaffold("vacuous").await;
        // A test that passes on old AND new code witnesses nothing.
        std::fs::write(root.join("witness.sh"), "grep -q 'behavior' impl.txt\n").unwrap();

        let out = VerifyDone::default()
            .execute(
                &serde_json::json!({
                    "test_cmd": "bash witness.sh",
                    "test_files": ["witness.sh"],
                    "timeout_secs": 60
                }),
                &root,
            )
            .await;
        match &out {
            ToolOutput::Error { message } => {
                assert!(message.contains("VACUOUS TEST"), "{message}")
            }
            other => panic!("expected vacuous rejection, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// #2067 witness (baseline drift): the pipeline commits the agent's work
    /// on top of the session baseline, so a flip against `HEAD` is
    /// structurally impossible — the per-worktree pin restores the true
    /// previous version.
    #[tokio::test]
    async fn a_pinned_baseline_ref_beats_a_head_that_contains_the_fix() {
        let root = scaffold("pinref").await;
        let base = scratch_git(&root, &["rev-parse", "HEAD"])
            .trim()
            .to_string();
        scratch_git(&root, &["update-ref", WITNESS_BASELINE_WORKTREE_REF, &base]);
        // The auto-snapshot: the fix, already committed past the baseline.
        std::fs::write(root.join("impl.txt"), "new behavior\n").unwrap();
        scratch_git(&root, &["add", "."]);
        scratch_git(&root, &["commit", "-q", "-m", "agent work, sealed"]);
        std::fs::write(root.join("witness.sh"), "grep -q 'new behavior' impl.txt\n").unwrap();

        let out = VerifyDone::default()
            .execute(
                &serde_json::json!({
                    "test_cmd": "bash witness.sh",
                    "test_files": ["witness.sh"],
                    "timeout_secs": 60
                }),
                &root,
            )
            .await;
        match &out {
            ToolOutput::Ok { content } => {
                assert!(content.contains("WITNESS CONFIRMED"), "{content}");
                assert!(content.contains("pinned"), "{content}");
            }
            other => panic!("a pinned baseline must restore the flip, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// #2067: the harness-pinned task baseline (`refs/stella/task-baseline`)
    /// is honored when no per-worktree pin exists.
    #[tokio::test]
    async fn a_task_baseline_ref_is_honored_when_no_worktree_pin_exists() {
        let root = scaffold("taskref").await;
        let base = scratch_git(&root, &["rev-parse", "HEAD"])
            .trim()
            .to_string();
        scratch_git(&root, &["update-ref", WITNESS_BASELINE_TASK_REF, &base]);
        std::fs::write(root.join("impl.txt"), "new behavior\n").unwrap();
        scratch_git(&root, &["add", "."]);
        scratch_git(&root, &["commit", "-q", "-m", "trial work"]);
        std::fs::write(root.join("witness.sh"), "grep -q 'new behavior' impl.txt\n").unwrap();

        let out = VerifyDone::default()
            .execute(
                &serde_json::json!({
                    "test_cmd": "bash witness.sh",
                    "test_files": ["witness.sh"],
                    "timeout_secs": 60
                }),
                &root,
            )
            .await;
        match &out {
            ToolOutput::Ok { content } => {
                assert!(content.contains("WITNESS CONFIRMED"), "{content}");
                assert!(content.contains(WITNESS_BASELINE_TASK_REF), "{content}");
            }
            other => panic!("the task baseline pin must restore the flip, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// #2067: with no pin at all, seal-shaped commits (the snapshot identity
    /// plus [`CANDIDATE_SEAL_SUBJECT`]) are walked past to the first real
    /// commit underneath them.
    #[tokio::test]
    async fn seal_commits_are_walked_past_when_no_ref_is_pinned() {
        let root = scaffold("sealwalk").await;
        let email_arg = format!("user.email={CANDIDATE_SNAPSHOT_EMAIL}");
        std::fs::write(root.join("impl.txt"), "new behavior\n").unwrap();
        scratch_git(&root, &["add", "."]);
        scratch_git(
            &root,
            &[
                "-c",
                "user.name=stella-pipeline",
                "-c",
                &email_arg,
                "commit",
                "-q",
                "-m",
                CANDIDATE_SEAL_SUBJECT,
            ],
        );
        std::fs::write(root.join("witness.sh"), "grep -q 'new behavior' impl.txt\n").unwrap();

        let out = VerifyDone::default()
            .execute(
                &serde_json::json!({
                    "test_cmd": "bash witness.sh",
                    "test_files": ["witness.sh"],
                    "timeout_secs": 60
                }),
                &root,
            )
            .await;
        match &out {
            ToolOutput::Ok { content } => {
                assert!(content.contains("WITNESS CONFIRMED"), "{content}");
                assert!(content.contains("snapshot commit"), "{content}");
            }
            other => panic!("the seal walk must restore the flip, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// #2067 witness (honest degradation): a workspace whose entire history
    /// is pipeline snapshots has no true baseline — that is a fact about the
    /// workspace and must be reported as such, never as a `VACUOUS` verdict
    /// that sends the author off strengthening a test that was never the
    /// problem.
    #[tokio::test]
    async fn an_all_snapshot_history_is_refused_not_called_vacuous() {
        let root = std::env::temp_dir().join(format!(
            "stella_verify_test_allsnap_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        scratch_git(&root, &["init", "-q"]);
        let email_arg = format!("user.email={CANDIDATE_SNAPSHOT_EMAIL}");
        std::fs::write(root.join("impl.txt"), "new behavior\n").unwrap();
        scratch_git(&root, &["add", "."]);
        scratch_git(
            &root,
            &[
                "-c",
                "user.name=stella-pipeline",
                "-c",
                &email_arg,
                "commit",
                "-q",
                "-m",
                CANDIDATE_SEAL_SUBJECT,
            ],
        );
        std::fs::write(root.join("witness.sh"), "grep -q 'new behavior' impl.txt\n").unwrap();

        let out = VerifyDone::default()
            .execute(
                &serde_json::json!({
                    "test_cmd": "bash witness.sh",
                    "test_files": ["witness.sh"],
                    "timeout_secs": 60
                }),
                &root,
            )
            .await;
        match &out {
            ToolOutput::Error { message } => {
                assert!(message.contains("WITNESS BASELINE UNRESOLVED"), "{message}");
                assert!(
                    !message.contains("VACUOUS"),
                    "an unresolved baseline must not accuse the test: {message}"
                );
            }
            other => panic!("expected an honest refusal, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// #2067 witness (false positive): a previous-code run that dies on an
    /// import — the shape a missing build artifact produces in a fresh
    /// checkout — observed no behaviour and must not be credited as a flip.
    #[tokio::test]
    async fn an_import_failure_on_the_previous_code_is_not_a_confirmed_flip() {
        let root = scaffold("importfail").await;
        std::fs::write(root.join("impl.txt"), "new behavior\n").unwrap();
        std::fs::write(
            root.join("witness.sh"),
            "if grep -q 'new behavior' impl.txt; then exit 0; else echo \
             \"ModuleNotFoundError: No module named 'portfolio_optimized_c'\"; exit 1; fi\n",
        )
        .unwrap();

        let out = VerifyDone::default()
            .execute(
                &serde_json::json!({
                    "test_cmd": "bash witness.sh",
                    "test_files": ["witness.sh"],
                    "timeout_secs": 60
                }),
                &root,
            )
            .await;
        match &out {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("WITNESS_INCONCLUSIVE_BUILD_ERROR"),
                    "{message}"
                );
            }
            other => panic!("an import failure must not confirm a witness, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn failing_new_code_is_not_done() {
        let root = scaffold("notdone").await;
        // Test demands behavior nobody implemented.
        std::fs::write(root.join("witness.sh"), "grep -q 'nonexistent' impl.txt\n").unwrap();

        let out = VerifyDone::default()
            .execute(
                &serde_json::json!({
                    "test_cmd": "bash witness.sh",
                    "test_files": ["witness.sh"],
                    "timeout_secs": 60
                }),
                &root,
            )
            .await;
        match &out {
            ToolOutput::Error { message } => assert!(message.contains("NOT DONE"), "{message}"),
            other => panic!("expected NOT DONE, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn missing_inputs_and_missing_files_are_named_errors() {
        let root = scaffold("inputs").await;
        let no_files = VerifyDone::default()
            .execute(
                &serde_json::json!({"test_cmd": "true", "test_files": []}),
                &root,
            )
            .await;
        assert!(no_files.is_error());

        let ghost = VerifyDone::default()
            .execute(
                &serde_json::json!({"test_cmd": "true", "test_files": ["ghost.sh"]}),
                &root,
            )
            .await;
        match ghost {
            ToolOutput::Error { message } => {
                assert!(message.contains("does not exist"), "{message}")
            }
            other => panic!("{other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// Count the live `.git/worktrees/*` registrations in a repo.
    fn registrations(root: &std::path::Path) -> usize {
        std::fs::read_dir(root.join(".git").join("worktrees"))
            .map(|entries| entries.flatten().count())
            .unwrap_or(0)
    }

    /// #613, drop half: the synchronous `Drop` guard removes the shadow's
    /// `/tmp` directory with no runtime, no `await`, and no spawn — the only
    /// shape that still works while the runtime is shutting down.
    #[test]
    fn the_shadow_dir_guard_removes_the_directory_synchronously_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let shadow = dir.path().join("shadow");
        std::fs::create_dir_all(shadow.join("nested")).unwrap();
        std::fs::write(shadow.join("nested").join("f"), b"x").unwrap();

        let guard = ShadowDirGuard {
            shadow: shadow.clone(),
        };
        assert!(shadow.exists());
        drop(guard);
        assert!(!shadow.exists(), "the guard must remove the shadow tree");

        // Idempotent with `cleanup_shadow` having already run: a second drop
        // over a missing path is a no-op, not an error.
        drop(ShadowDirGuard { shadow });
    }

    /// #613, prune half: the `/tmp` directory the drop guard removes leaves a
    /// `.git/worktrees/<name>` registration behind, because unregistering it
    /// needs an awaited git call. The next `verify_done` collects it.
    ///
    /// The stranded state is produced directly — add a worktree, then delete
    /// its directory the way the sync guard does — because racing a real
    /// cancellation into the window between `worktree add` and cleanup is
    /// racy by construction.
    ///
    /// The verdict is deliberately **NOT DONE**: that path returns before any
    /// worktree is created and therefore never reaches `cleanup_shadow`,
    /// whose trailing `prune` would otherwise make this pass vacuously. The
    /// reclamation has to come from the prune at *start*.
    #[tokio::test]
    async fn a_stranded_worktree_registration_is_pruned_on_the_next_start() {
        let root = scaffold("prune").await;
        let stranded = std::env::temp_dir().join(format!(
            "stella_verify_stranded_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let out = git_in(&root)
            .args(["worktree", "add", "--detach"])
            .arg(&stranded)
            .output()
            .await
            .unwrap();
        assert!(out.status.success(), "fixture worktree add failed");
        // What the synchronous drop guard can do, and all it can do.
        std::fs::remove_dir_all(&stranded).unwrap();
        assert_eq!(
            registrations(&root),
            1,
            "the fixture must leave exactly the stranded registration"
        );

        std::fs::write(root.join("witness.sh"), "grep -q 'nonexistent' impl.txt\n").unwrap();
        let input = serde_json::json!({
            "test_cmd": "bash witness.sh",
            "test_files": ["witness.sh"],
            "timeout_secs": 60
        });
        match VerifyDone::default().execute(&input, &root).await {
            ToolOutput::Error { message } => assert!(message.contains("NOT DONE"), "{message}"),
            other => panic!("expected NOT DONE, got {other:?}"),
        }
        assert_eq!(
            registrations(&root),
            0,
            "verify_done must prune the stranded registration on start"
        );

        // Idempotent: a second run over an already-clean repo prunes nothing
        // and reaches the same verdict.
        assert!(
            VerifyDone::default()
                .execute(&input, &root)
                .await
                .is_error()
        );
        assert_eq!(registrations(&root), 0);

        std::fs::remove_dir_all(&root).ok();
    }

    /// #613, end to end: dropping the `verify_done` future while the shadow
    /// run is in flight must not leave the shadow directory behind. The
    /// witness script branches on whether `.git` is a directory — true in the
    /// real workspace, false in a linked worktree, where it is a file — so
    /// only the *shadow* half hangs, after announcing itself through a marker
    /// file in the real root. Waiting on that marker (rather than on the
    /// worktree appearing) is what makes the cancellation land inside the
    /// shadow run every time instead of racing `git worktree add`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_cancelled_verify_done_removes_its_shadow_directory() {
        use std::time::Duration;

        let root = scaffold("cancelled").await;
        let started = root.join("shadow-started");
        std::fs::write(root.join("impl.txt"), "new behavior\n").unwrap();
        std::fs::write(
            root.join("witness.sh"),
            format!(
                "if [ -d .git ]; then grep -q 'new behavior' impl.txt; else touch '{}'; sleep \
                 300; fi\n",
                started.display()
            ),
        )
        .unwrap();

        let run_root = root.clone();
        let handle = tokio::spawn(async move {
            VerifyDone::default()
                .execute(
                    &serde_json::json!({
                        "test_cmd": "bash witness.sh",
                        "test_files": ["witness.sh"],
                        "timeout_secs": 600
                    }),
                    &run_root,
                )
                .await
        });

        for _ in 0..500 {
            if started.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(started.exists(), "the shadow run never started");

        // The shadow's path, read out of THIS repo's registration — sibling
        // tests share the `/tmp` `stella_verify_<pid>_` prefix, so scanning
        // the temp dir would pick up their worktrees too.
        let shadow = std::fs::read_dir(root.join(".git").join("worktrees"))
            .ok()
            .and_then(|entries| {
                entries.flatten().find_map(|entry| {
                    let gitdir = std::fs::read_to_string(entry.path().join("gitdir")).ok()?;
                    // `<shadow>/.git` → `<shadow>`.
                    Some(
                        std::path::PathBuf::from(gitdir.trim())
                            .parent()?
                            .to_path_buf(),
                    )
                })
            })
            .expect("a running shadow test implies a registered worktree");

        handle.abort();
        let _ = handle.await;

        let mut gone = false;
        for _ in 0..250 {
            if !shadow.exists() {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            gone,
            "a cancelled verify_done left the shadow worktree at {}",
            shadow.display()
        );

        // And the registration the synchronous guard could not remove is
        // collected by a prune — the other half of the fix, which every later
        // `verify_done` runs at start.
        assert_eq!(registrations(&root), 1);
        let _ = git_in(&root).args(["worktree", "prune"]).output().await;
        assert_eq!(registrations(&root), 0);

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn non_git_workspace_is_a_named_error() {
        let root = std::env::temp_dir().join(format!("stella_verify_nogit_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("witness.sh"), "true\n").unwrap();
        let out = VerifyDone::default()
            .execute(
                &serde_json::json!({"test_cmd": "true", "test_files": ["witness.sh"]}),
                &root,
            )
            .await;
        match out {
            ToolOutput::Error { message } => {
                assert!(message.contains("requires a git repository"), "{message}")
            }
            other => panic!("{other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }
}
