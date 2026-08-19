// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The host-side data and logic behind a [`CandidateGrant`]/[`TestPlan`]: the
//! process-outcome types a local test run produces, the strict test-command
//! parser that turns untrusted text into a [`TestInvocation`], the tamper
//! comparator a flip is credited against, and the path fence that keeps an
//! inbound path inside the granted root.
//!
//! # Where this came from
//!
//! Every name here was first defined in `stella-pipeline` — `ports.rs`'s
//! process-outcome types, `ports/handle.rs`'s grant-minting and path fence,
//! `witness.rs`'s command parser and identity comparator — because that was
//! the only caller at the time. It moved here (`§1.5.6` of the removal
//! census that later deleted that crate outright, #3865) once a second
//! caller needed it: `stella-cli`'s wrapper-socket driver
//! (`wrapper_candidate.rs`) for **every** currently-installed
//! witness-flavoured plugin — the exact shape already established for
//! [`crate::wire`]'s [`CandidateGrant`]/[`TestPlan`]. At the time of the
//! move the staged pipeline's own `CandidateHandles` table (then
//! `stella-pipeline`'s `ports/handle.rs`) was still the other caller, so the
//! move kept one minting implementation shared across the crate boundary
//! rather than two; that crate is gone now (#3865) and this module's own
//! caller is the only one left. Two callers outside the crate that first
//! needed a thing was the textbook boundary-type signal (AGENTS.md
//! invariant 1), and `stella-plugin` is where it now sits: already near-leaf
//! (only `stella-protocol` as a workspace dependency), already hosting the
//! wire types ([`CandidateGrant`], [`crate::TamperFinding`]) these functions
//! build against, and upstream of both `stella-cli` and `stella-runtime` in
//! the dependency graph so either can reach the minting logic without a
//! layering violation.
//!
//! For as long as `stella-pipeline` still existed it re-exported every name
//! below unchanged from `ports`/`ports::handle`/`witness`, so its own call
//! sites — including `CandidateHandles`'s own `grant`/`resolve_path`, which
//! called [`fence`]/[`canonical_root`] here rather than keeping a second copy
//! — needed no logic changes, only import-path updates. "One minting
//! implementation and one fence rather than two" (`wrapper_candidate.rs`'s own
//! module doc) is exactly why the fence stayed a single copy through the move
//! instead of being duplicated at the new call site; that crate was deleted in
//! #3865 and this is the only copy left.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use stella_protocol::{CandidateDenial, CandidateHandle, PathDenial};

use crate::wire::{CandidateGrant, TestBaseline, TestPlan};

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
    /// failure first.
    ///
    /// Its own variant rather than a shade of [`Self::Infra`] because the two
    /// call for opposite handling. An unspawnable toolchain will be
    /// unspawnable on every retry, so re-running it is spend with no
    /// information; a memory kill is exactly the outcome a human would simply
    /// *try again*. Collapsing them would make one of the two behaviors
    /// wrong, and the evidence a verifier reads would say "infra_failure"
    /// where the honest word is "out_of_memory".
    OutOfMemory,
    /// The process never produced a real exit status — it could not be
    /// spawned (missing program/toolchain) or its wait failed.
    Infra,
}

/// The outcome of running one local process through a runner. Output is
/// pre-truncated by the runner (middle-out, L-S3) into head+tail tails — a
/// caller never needs the full stream, only exit status and enough text to
/// summarize evidence.
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
/// [`parse_test_invocation`]. `program` and `args` are passed directly to a
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
    /// The single place pass/fail is decided for a command — never
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
    /// This is the only reading a flip oracle may consume. Feeding it a raw
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
    /// variant that is also worth a retry joins it here, not at every caller.
    #[must_use]
    pub fn is_out_of_memory(&self) -> bool {
        self.kind == CmdKind::OutOfMemory
    }
}

/// Why a model- or user-authored test command was not accepted as a typed
/// test invocation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TestInvocationError {
    /// The command was empty or contained unbalanced quoting.
    #[error("the test command is empty or has invalid quoting")]
    InvalidSyntax,
    /// Shell control syntax is never valid at this boundary.
    #[error("shell operators, redirection, and expansion are not allowed in test commands")]
    ShellSyntax,
    /// Only explicit test-runner forms are accepted.
    #[error("unsupported test command `{0}`")]
    Unsupported(String),
}

/// Parse a deliberately small test-command vocabulary into an enumerable
/// program plus argv. This is quote-aware only to preserve arguments with
/// spaces; it is not a shell parser and rejects every shell control surface.
pub fn parse_test_invocation(command: &str) -> Result<TestInvocation, TestInvocationError> {
    let words = split_test_words(command)?;
    let (program, args) = words
        .split_first()
        .ok_or(TestInvocationError::InvalidSyntax)?;
    let allowed = match program.as_str() {
        "cargo" => {
            matches!(args.first().map(String::as_str), Some("test"))
                || matches!(
                    (
                        args.first().map(String::as_str),
                        args.get(1).map(String::as_str)
                    ),
                    (Some("nextest"), Some("run"))
                )
        }
        "pnpm" => {
            matches!(args.first().map(String::as_str), Some("test"))
                || matches!(
                    (
                        args.first().map(String::as_str),
                        args.get(1).map(String::as_str),
                        args.get(2).map(String::as_str)
                    ),
                    (Some("exec"), Some("vitest"), Some("run"))
                )
        }
        "npm" | "yarn" | "bun" => matches!(args.first().map(String::as_str), Some("test")),
        "vitest" => matches!(args.first().map(String::as_str), Some("run")),
        "npx" | "bunx" => matches!(
            (
                args.first().map(String::as_str),
                args.get(1).map(String::as_str)
            ),
            (Some("vitest"), Some("run"))
        ),
        "pytest" => true,
        "python" | "python3" => matches!(
            (
                args.first().map(String::as_str),
                args.get(1).map(String::as_str)
            ),
            (Some("-m"), Some("pytest"))
        ),
        "go" | "dotnet" => matches!(args.first().map(String::as_str), Some("test")),
        // #2064: the runner-less-workspace witness — exactly one positional
        // `.sh` script, the authored artifact. `split_test_words` has
        // already rejected pipelines, redirects, substitution, and
        // backgrounding anywhere in the line, and the script path gets the
        // same escape checks as every other runner's arguments. `sh -c
        // '<inline>'` is deliberately NOT accepted, although the issue
        // sketched it: the inline program rides as one opaque argv word, so
        // the per-argument confinement checks cannot see a path inside it —
        // and an invocation that names no artifact could never be
        // tamper-excluded anyway. A bare `sh` is an interactive shell, not
        // a test.
        "sh" => match args {
            [script] => script.ends_with(".sh") && !script.starts_with('-'),
            _ => false,
        },
        _ => false,
    };
    if !allowed {
        return Err(TestInvocationError::Unsupported(command.to_string()));
    }
    validate_local_args(program, args)?;
    Ok(TestInvocation {
        program: program.clone(),
        args: args.to_vec(),
    })
}

fn split_test_words(command: &str) -> Result<Vec<String>, TestInvocationError> {
    if command.contains("$(")
        || command.contains('`')
        || command.chars().any(|ch| {
            matches!(
                ch,
                '&' | '|'
                    | ';'
                    | '<'
                    | '>'
                    | '\n'
                    | '\r'
                    | '\u{ff06}'
                    | '\u{ff5c}'
                    | '\u{ff1b}'
                    | '\u{ff1c}'
                    | '\u{ff1e}'
            ) || (ch.is_whitespace() && !matches!(ch, ' ' | '\t'))
                || ch.is_control()
        })
    {
        return Err(TestInvocationError::ShellSyntax);
    }
    let mut words = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut single = false;
    let mut double = false;
    let mut chars = command.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\'' if !double => {
                single = !single;
                started = true;
            }
            '"' if !single => {
                double = !double;
                started = true;
            }
            '\\' if !single => {
                let Some(escaped) = chars.next() else {
                    return Err(TestInvocationError::InvalidSyntax);
                };
                current.push(escaped);
                started = true;
            }
            '&' | '|' | ';' | '<' | '>' if !single && !double => {
                return Err(TestInvocationError::ShellSyntax);
            }
            c if c.is_whitespace() && !single && !double => {
                if started {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if single || double {
        return Err(TestInvocationError::InvalidSyntax);
    }
    if started {
        words.push(current);
    }
    Ok(words)
}

fn validate_local_args(program: &str, args: &[String]) -> Result<(), TestInvocationError> {
    let forbidden_flags: &[&str] = match program {
        "cargo" => &["--manifest-path", "--config", "-C", "--target-dir"],
        "pnpm" | "npm" | "yarn" | "bun" | "vitest" | "npx" | "bunx" => &[
            "--prefix",
            "--dir",
            "--cwd",
            "-C",
            "--userconfig",
            "--globalconfig",
            "--script-shell",
        ],
        "pytest" | "python" | "python3" => &["--rootdir", "--confcutdir", "-c", "--basetemp"],
        "go" => &["-C", "-exec", "-toolexec", "-overlay", "-modfile"],
        "dotnet" => &["--test-adapter-path", "--settings"],
        _ => &[],
    };
    for arg in args {
        let normalized = arg.replace('\\', "/");
        let windows_absolute = normalized.as_bytes().get(1) == Some(&b':')
            && normalized.as_bytes().get(2) == Some(&b'/');
        if std::path::Path::new(arg).is_absolute()
            || windows_absolute
            || normalized.split('/').any(|component| component == "..")
        {
            return Err(TestInvocationError::ShellSyntax);
        }
        let flag = arg.split_once('=').map_or(arg.as_str(), |(flag, _)| flag);
        if forbidden_flags.contains(&flag) {
            return Err(TestInvocationError::ShellSyntax);
        }
    }
    Ok(())
}

/// Whether the current no-follow filesystem observation is exactly the
/// regular, single-link artifact accepted at witness authoring time —
/// including the location the observation was made at, so a witness that was
/// renamed to a different path never matches even when an aliased lookup
/// still resolves the pinned path to its unchanged bytes.
pub fn witness_identity_matches(
    expected: &ArtifactIdentity,
    current: Option<&ArtifactIdentity>,
) -> bool {
    expected.is_regular_single_link() && current == Some(expected)
}

/// The handle a grant over the host's own tree carries.
///
/// Deliberately outside a candidate-handle registry's own namespace: it names
/// no entry in any live table, so every handle-addressed operation against it
/// answers "unknown handle" — which is the true answer. A tree the host is
/// already running in has nothing to seal, no baseline to adopt against, and
/// no workspace to remove; answering `Ok` to any of those would be the host
/// claiming an isolation it does not have.
pub const HOST_TREE_HANDLE: &str = "host-tree";

/// Mint the [`CandidateGrant`] for a tree the host is **already running in**.
///
/// The shared-work-tree case, and the reason it is a function here rather than
/// a second minting somewhere else: it resolves the root through the same
/// [`canonical_root`] a candidate-handle registry's own grant path does, so a
/// plugin's root and the fence in [`resolve_in_root`] cannot come to disagree
/// about which directory they mean. It fails closed the same two ways — a
/// root the filesystem will not resolve, and a root this host cannot spell as
/// UTF-8 — rather than handing out a path nothing can be judged against.
///
/// What it does **not** mint is isolation: the host's own tree is not a
/// workspace an isolation substrate created, so [`HOST_TREE_HANDLE`]
/// addresses nothing. The grant carries a real root and a real test plan —
/// everything a plugin needs to *read* the tree the turn ran in and run the
/// test there.
///
/// # Errors
///
/// [`CandidateDenial::RootUnavailable`] for a root that cannot be resolved on
/// this filesystem or is not valid UTF-8.
pub fn host_tree_grant(
    root: &str,
    test: Option<TestPlan>,
) -> Result<CandidateGrant, CandidateDenial> {
    let canonical = canonical_root(root)?
        .into_os_string()
        .into_string()
        .map_err(|_| CandidateDenial::RootUnavailable {
            reason: "the workspace root is not valid UTF-8".to_string(),
        })?;
    Ok(CandidateGrant {
        handle: CandidateHandle::new(HOST_TREE_HANDLE),
        root: canonical,
        test,
    })
}

/// Resolve one caller-supplied relative path inside `root`, or refuse it.
///
/// The same funnel a candidate-handle registry's own `resolve_path` runs, for
/// a caller that holds a root rather than a handle — a driver checking the
/// identity of a witness artifact inside the tree it granted, say. Both fence
/// layers apply: see [`fence`] for what the answer promises and what it
/// cannot.
///
/// # Errors
///
/// [`CandidateDenial::Path`] for a path that is absolute, traverses, is
/// malformed or lands outside `root` once symlinks are followed;
/// [`CandidateDenial::RootUnavailable`] for a root that will not canonicalise.
pub fn resolve_in_root(root: &str, relative: &str) -> Result<PathBuf, CandidateDenial> {
    fence(root, relative)
}

/// Build the wire [`TestPlan`] for one already-parsed invocation and what it
/// reported before the turn ran.
///
/// The baseline is read through [`CmdOutcome::assertion_result`] and never off
/// a raw exit code, which is the whole reason [`TestBaseline`] has a fourth
/// variant: a run that timed out or could not find its toolchain observed no
/// assertion, and reporting its non-zero exit as red would satisfy a flip's
/// precondition on infrastructure noise (#860). `None` is "the host did not run
/// it", which is a different claim again.
#[must_use]
pub fn test_plan(invocation: &TestInvocation, baseline: Option<&CmdOutcome>) -> TestPlan {
    TestPlan::new(invocation.program.clone(), invocation.args.clone()).with_baseline(match baseline
        .map(CmdOutcome::assertion_result)
    {
        None => TestBaseline::NotRun,
        Some(Some(true)) => TestBaseline::Passed,
        Some(Some(false)) => TestBaseline::Failed,
        Some(None) => TestBaseline::Unobserved,
    })
}

/// Both fence layers, in order: refuse what is malformed without touching the
/// filesystem, then judge containment after the filesystem has had its say.
///
/// # What this is not
///
/// The resolved path is true at the instant it is checked; a sufficiently
/// determined local attacker can swap a component afterwards (TOCTOU).
/// Closing that needs `openat`-style resolution held across the use, which is
/// a substrate change, not a caller change — #3483, declared rather than
/// pretended away.
pub fn fence(root: &str, path: &str) -> Result<PathBuf, CandidateDenial> {
    let relative = fence_lexical(path).map_err(|reason| CandidateDenial::Path {
        path: path.to_string(),
        reason,
    })?;
    fence_on_disk(root, &relative, path)
}

/// The pure layer: is this text even capable of naming something inside a
/// root?
///
/// Absolute paths, `..` components, NUL bytes and backslashes are refused
/// without touching the filesystem, as written and never normalised first —
/// normalising before checking is how a fence is talked past. Returns the
/// path rebuilt from its `Normal` components, which is what [`fence_on_disk`]
/// joins — rebuilt rather than reused so a `./` prefix cannot survive as a
/// component.
fn fence_lexical(path: &str) -> Result<PathBuf, PathDenial> {
    if path.is_empty() {
        return Err(PathDenial::Empty);
    }
    // A NUL cannot reach a syscall, and a backslash is a separator on one
    // host family and an ordinary filename byte on the other. Guessing which
    // one a plugin meant is how a fence produces different answers on
    // different machines, so both are refused.
    if path.contains('\0') || path.contains('\\') {
        return Err(PathDenial::Malformed);
    }
    let raw = Path::new(path);
    if raw.is_absolute() {
        return Err(PathDenial::Absolute);
    }
    let mut fenced = PathBuf::new();
    for component in raw.components() {
        match component {
            // `C:/x` is not absolute to a Unix host, so the drive prefix has
            // to be recognised by hand rather than left to `is_absolute`.
            Component::Normal(name) if fenced.as_os_str().is_empty() && is_drive_letter(name) => {
                return Err(PathDenial::Absolute);
            }
            Component::Normal(name) => fenced.push(name),
            Component::CurDir => {}
            Component::ParentDir => return Err(PathDenial::Traversal),
            Component::RootDir | Component::Prefix(_) => return Err(PathDenial::Absolute),
        }
    }
    if fenced.as_os_str().is_empty() {
        return Err(PathDenial::Empty);
    }
    Ok(fenced)
}

/// The one place a candidate root is turned into a path, for both directions:
/// the root a grant hands a plugin and the root the on-disk fence layer
/// judges containment against. One resolution, so the two can never disagree
/// about which directory the fence is around.
pub fn canonical_root(root: &str) -> Result<PathBuf, CandidateDenial> {
    Path::new(root)
        .canonicalize()
        .map_err(|reason| CandidateDenial::RootUnavailable {
            reason: reason.to_string(),
        })
}

fn is_drive_letter(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_str().map(str::as_bytes),
        Some([letter, b':']) if letter.is_ascii_alphabetic()
    )
}

/// The filesystem layer: does this well-formed relative path still land
/// inside the root once every symlink on the way has been followed?
///
/// Walks up from the joined path to the deepest component that exists,
/// canonicalises *that*, and requires the result to sit under the canonical
/// root — so a link in the middle of the path is judged, not just a link at
/// the end. The non-existent tail is re-appended afterwards, which is what
/// lets a caller resolve a path it is about to create.
///
/// Fails closed at every step: an unresolvable root, a broken link, or a walk
/// that runs off the top all refuse.
fn fence_on_disk(
    root: &str,
    relative: &Path,
    as_written: &str,
) -> Result<PathBuf, CandidateDenial> {
    let escapes = || CandidateDenial::Path {
        path: as_written.to_string(),
        reason: PathDenial::EscapesRoot,
    };
    let canonical_root_path = canonical_root(root)?;

    let mut existing = canonical_root_path.join(relative);
    let mut tail = Vec::new();
    // `symlink_metadata` rather than `exists`: a dangling symlink is an entry
    // that exists, and treating it as absent would append past it instead of
    // refusing it below.
    while std::fs::symlink_metadata(&existing).is_err() {
        let Some(name) = existing.file_name().map(std::ffi::OsStr::to_os_string) else {
            return Err(escapes());
        };
        tail.push(name);
        if !existing.pop() {
            return Err(escapes());
        }
    }

    let resolved = existing.canonicalize().map_err(|_| escapes())?;
    if !resolved.starts_with(&canonical_root_path) {
        return Err(escapes());
    }
    let mut full = resolved;
    for name in tail.iter().rev() {
        full.push(name);
    }
    Ok(full)
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
        assert!(
            !CmdOutcome {
                exit_code: 1,
                stdout_tail: String::new(),
                stderr_tail: String::new(),
                kind: CmdKind::Completed,
            }
            .passed()
        );
    }

    #[test]
    fn infra_outcomes_are_never_assertions() {
        let base = CmdOutcome {
            exit_code: -1,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            kind: CmdKind::TimedOut,
        };
        assert_eq!(base.assertion_result(), None);
        assert_eq!(base.infra_label(), Some("timed_out"));
    }

    #[test]
    fn an_out_of_memory_kill_is_its_own_outcome() {
        let oom = CmdOutcome {
            exit_code: -1,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            kind: CmdKind::OutOfMemory,
        };
        assert!(oom.is_out_of_memory());
        assert_eq!(oom.assertion_result(), None);
        assert_eq!(oom.infra_label(), Some("out_of_memory"));
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

    #[test]
    fn accepted_test_commands_parse() {
        assert!(parse_test_invocation("cargo test -p stella-plugin").is_ok());
        assert!(parse_test_invocation("pytest").is_ok());
        assert!(
            parse_test_invocation("./witness.sh").is_err(),
            "not a known runner"
        );
    }

    #[test]
    fn shell_syntax_is_refused() {
        assert_eq!(
            parse_test_invocation("cargo test; rm -rf /"),
            Err(TestInvocationError::ShellSyntax)
        );
    }

    #[test]
    fn escaping_test_arguments_are_refused() {
        assert_eq!(
            parse_test_invocation("cargo test --manifest-path ../../Cargo.toml"),
            Err(TestInvocationError::ShellSyntax)
        );
    }

    fn identity(path: &str, fingerprint: &str) -> ArtifactIdentity {
        ArtifactIdentity {
            path: path.to_string(),
            fingerprint: fingerprint.to_string(),
            kind: ArtifactKind::Regular,
            mode: 0o644,
            link_count: 1,
        }
    }

    #[test]
    fn identity_matches_only_the_exact_regular_single_link_observation() {
        let expected = identity("tests/witness_test.rs", "sha256:aa");
        assert!(witness_identity_matches(&expected, Some(&expected)));
        assert!(!witness_identity_matches(&expected, None), "no observation");
        let moved = identity("tests/renamed.rs", "sha256:aa");
        assert!(
            !witness_identity_matches(&expected, Some(&moved)),
            "same bytes at a different path is a rename, and a rename is tampering"
        );
        let symlinked = ArtifactIdentity {
            kind: ArtifactKind::Symlink,
            ..expected.clone()
        };
        assert!(!witness_identity_matches(&expected, Some(&symlinked)));
    }

    #[test]
    fn the_lexical_fence_refuses_what_cannot_name_an_inside_path() {
        assert_eq!(fence_lexical(""), Err(PathDenial::Empty));
        assert_eq!(fence_lexical("."), Err(PathDenial::Empty));
        assert_eq!(fence_lexical("./"), Err(PathDenial::Empty));
        assert_eq!(fence_lexical("/etc/passwd"), Err(PathDenial::Absolute));
        assert_eq!(fence_lexical("C:/Windows"), Err(PathDenial::Absolute));
        assert_eq!(fence_lexical("../x"), Err(PathDenial::Traversal));
        assert_eq!(fence_lexical("a/../../x"), Err(PathDenial::Traversal));
        assert_eq!(fence_lexical("a/..").unwrap_err(), PathDenial::Traversal);
        assert_eq!(fence_lexical("a\\b"), Err(PathDenial::Malformed));
        assert_eq!(fence_lexical("a\0b"), Err(PathDenial::Malformed));
        assert_eq!(
            fence_lexical("./tests/../tests/x.py").unwrap_err(),
            PathDenial::Traversal,
            "a `..` is refused as written, never normalised away first"
        );
        assert_eq!(
            fence_lexical("./tests/x.py").expect("in-root"),
            Path::new("tests/x.py")
        );
    }

    /// The free fence is the same funnel a handle-addressed one runs, so a
    /// caller holding a root gets exactly the containment a caller holding a
    /// handle does — the witness for "one fence, not two" across the crate
    /// boundary this module doc names.
    #[test]
    fn the_free_fence_refuses_what_the_lexical_layer_already_refused() {
        let dir = std::env::temp_dir().join(format!(
            "stella-plugin-free-fence-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(dir.join("tests")).expect("tests dir");
        std::fs::write(dir.join("tests/witness.sh"), "exit 1\n").expect("the artifact");
        let root = dir.canonicalize().expect("canonicalize");
        let root = root.to_str().expect("utf-8");

        assert!(resolve_in_root(root, "tests/witness.sh").is_ok());
        assert!(matches!(
            resolve_in_root(root, "../escape.sh"),
            Err(CandidateDenial::Path {
                reason: PathDenial::Traversal,
                ..
            })
        ));
        assert!(matches!(
            resolve_in_root(root, "/etc/passwd"),
            Err(CandidateDenial::Path {
                reason: PathDenial::Absolute,
                ..
            })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tree_the_host_already_runs_in_can_be_granted() {
        let dir = std::env::temp_dir().join(format!(
            "stella-plugin-candidate-grant-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let linked = dir.canonicalize().expect("canonicalize");
        let plan = TestPlan::new("pytest", vec![]);
        let grant = host_tree_grant(linked.to_str().expect("utf-8"), Some(plan.clone()))
            .expect("a real, resolvable directory grants");
        assert_eq!(grant.handle, CandidateHandle::new(HOST_TREE_HANDLE));
        assert_eq!(grant.test, Some(plan));
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn an_unresolvable_host_tree_mints_no_grant() {
        assert!(host_tree_grant("/definitely/not/here/at/all", None).is_err());
    }
}
