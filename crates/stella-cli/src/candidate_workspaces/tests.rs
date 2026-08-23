// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! #3892's acceptance: the four substrate operations against **real git**, and
//! the fan-out plane driven end to end over them.
//!
//! Every test here creates an actual repository in a temporary directory and
//! lets `git` be the oracle. That is deliberate rather than lazy: the whole
//! defect this module exists to close was a port with no implementation, and a
//! fake `GitCli` would prove only that this code calls the commands its author
//! believed in. The witness has to be that a worktree really appears, that a
//! candidate's bytes really are invisible to its sibling, and that an adoption
//! really lands on the tree the user is looking at.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use stella_core::EngineConfig;
use stella_core::subagent::SubAgentOutcome;
use stella_protocol::candidate::CandidateHandle;
use stella_protocol::event::ModelCallRole;
use stella_protocol::{
    CompletionRequestRef, CompletionResult, CompletionUsage, Provider, ProviderError, ToolCall,
};
use stella_runtime::wrapper::{
    CandidateFanoutError, CandidateFanoutPlane, CandidateWork, CandidateWorkspaces,
};

use super::*;
use crate::config::Config;

/// A repository with one committed file, so `HEAD` exists and a worktree can
/// be cut from it.
///
/// `git worktree add` needs a commit; a freshly `init`ed repository has none,
/// so a substrate built over one would fail at `create` for a reason that has
/// nothing to do with what is under test.
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "candidate@example.invalid"],
        vec!["config", "user.name", "Candidate Test"],
    ] {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(&args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }
    std::fs::write(root.join("seed.txt"), "one\ntwo\nthree\n").unwrap();
    git(root, &["add", "seed.txt"]);
    git(root, &["commit", "-qm", "seed"]);
    dir
}

/// Run a git command and insist it worked — the arrange half of these tests,
/// where a silent failure would make an assertion below vacuous.
fn git(root: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A provider that writes one file through the real `write_file` tool and then
/// answers.
///
/// The point is the *tool call*: it proves the candidate's turn ran against a
/// registry rooted at the candidate's worktree, which no assertion about the
/// turn's text could.
///
/// Each **turn** writes different bytes (`candidate-0`, `candidate-1`, …) so
/// two candidates of one fan-out are distinguishable on disk. "Is this the
/// first call of a turn?" is read from the transcript — a request carrying no
/// tool result yet — rather than from a call counter, because a call counter
/// cannot tell the second call of one turn from the first call of the next,
/// and would hand two candidates the same bytes exactly when a test needs them
/// to differ.
struct WritesOneFile {
    file: String,
    turns: AtomicUsize,
}

impl WritesOneFile {
    fn new(file: &str) -> Self {
        Self {
            file: file.to_string(),
            turns: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl Provider for WritesOneFile {
    fn id(&self) -> &str {
        "writes-one-file"
    }

    async fn complete_ref(
        &self,
        request: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        let first = !request
            .messages
            .iter()
            .any(|message| !message.tool_results.is_empty());
        let tool_calls = if first {
            let ordinal = self.turns.fetch_add(1, Ordering::SeqCst);
            vec![ToolCall {
                call_id: "c1".into(),
                name: "write_file".into(),
                input: serde_json::json!({
                    "path": self.file,
                    "content": format!("candidate-{ordinal}\n"),
                    "reason": "the candidate's answer",
                }),
            }]
        } else {
            Vec::new()
        };
        Ok(CompletionResult {
            upstream_provider: None,
            text: if first {
                String::new()
            } else {
                "wrote it".into()
            },
            tool_calls,
            usage: CompletionUsage {
                reported: true,
                input_tokens: 1,
                ..CompletionUsage::default()
            },
            model: "writes-one-file".into(),
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

/// The substrate under test, over `root`, backed by `provider`.
fn substrate(root: &Path, provider: Arc<dyn Provider>) -> SessionCandidateWorkspaces {
    let mut cfg = Config::for_tests(crate::config::PROVIDERS[0].clone(), "m".to_string());
    cfg.workspace_root = root.to_path_buf();
    // The session's own registry: a candidate never runs against it (that is
    // the whole point), but `SessionSubAgents` holds a `Weak` to one and
    // refuses if it is gone, so it has to exist and be kept alive.
    let session_registry = Arc::new(stella_tools::ToolRegistry::new(root.to_path_buf()));
    let sub_agents = Arc::new(
        crate::subagent::SessionSubAgents::new(
            provider,
            &session_registry,
            EngineConfig::default(),
            stella_protocol::BudgetMode::Observed,
        )
        .with_pool_limit(None),
    );
    // Leaked deliberately: the `Weak` above must stay upgradable for the
    // substrate's whole life, and a test that dropped it would be measuring
    // "the registry is gone" rather than anything about candidates.
    std::mem::forget(session_registry);
    SessionCandidateWorkspaces::new(&cfg, "candidates-plugin", sub_agents)
}

fn work(role: &str, instruction: &str) -> CandidateWork {
    CandidateWork {
        agent_id: "plugin:candidates-plugin/worker#0".into(),
        instruction: instruction.into(),
        role: role.into(),
        seat: ModelCallRole::Worker,
        budget_usd: None,
        turn_instance: 2,
    }
}

/// **The #3892 witness, end to end through the shipped substrate.**
///
/// Two candidates, two real worktrees, two writing turns that each land bytes
/// only in their own tree, one adoption — and the loser gone from disk with
/// nothing of its work on the real tree. Fails on `main` by not compiling
/// (`SessionCandidateWorkspaces` does not exist); would fail on any
/// implementation that shared a root between candidates, adopted the wrong
/// tree, or left the loser behind.
///
/// **One** substrate mints both, which is the production shape: a session
/// installs one plane per plugin and the plane owns one substrate, so a
/// handle only has to be unique within it.
#[tokio::test]
async fn two_candidates_write_in_isolation_and_exactly_one_is_adopted() {
    let dir = repo();
    let root = dir.path();
    let subject = substrate(root, Arc::new(WritesOneFile::new("answer.txt")));

    let a = subject
        .create("plugin:candidates-plugin/worker#0")
        .await
        .unwrap();
    let b = subject
        .create("plugin:candidates-plugin/worker#1")
        .await
        .unwrap();
    assert_ne!(a.handle, b.handle, "two candidates must not share a handle");
    assert_ne!(a.root, b.root, "two candidates must not share a tree");

    subject
        .work(&a, work("worker", "write the answer"))
        .await
        .unwrap();
    subject
        .work(&b, work("worker", "write the answer"))
        .await
        .unwrap();

    // Isolation, stated as a fact about the filesystem rather than about the
    // code that made it: each candidate holds only its own bytes, the two
    // differ, and the real tree holds neither.
    let wrote_a = std::fs::read_to_string(PathBuf::from(&a.root).join("answer.txt")).unwrap();
    let wrote_b = std::fs::read_to_string(PathBuf::from(&b.root).join("answer.txt")).unwrap();
    assert_ne!(
        wrote_a, wrote_b,
        "each candidate's turn must have written into its own tree"
    );
    assert!(
        !root.join("answer.txt").exists(),
        "a candidate's write must not reach the real tree before adoption"
    );

    subject.adopt(&a).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("answer.txt")).unwrap(),
        wrote_a,
        "the adopted candidate's bytes — and not its sibling's — must be on the \
         real tree"
    );

    subject.remove(&b).await.unwrap();
    assert!(
        !PathBuf::from(&b.root).exists(),
        "the discarded candidate's workspace must be off disk"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("answer.txt")).unwrap(),
        wrote_a,
        "discarding the loser must not disturb what was adopted"
    );
}

/// A candidate's turn measures what it changed from the **tree**, not from
/// what the turn said it did — including a file it created, which is the case
/// a plain `git diff` misses.
#[tokio::test]
async fn a_created_file_counts_as_a_change_and_survives_adoption() {
    let dir = repo();
    let root = dir.path();
    let subject = substrate(root, Arc::new(WritesOneFile::new("new.txt")));

    let candidate = subject.create("plugin:p/worker#0").await.unwrap();
    let report = subject
        .work(&candidate, work("worker", "create a file"))
        .await
        .unwrap();

    assert_eq!(
        report.files_changed, 1,
        "a created file is a changed file: {report:?}"
    );
    assert_eq!(report.lines_changed, 1, "one added line: {report:?}");

    let wrote = std::fs::read_to_string(PathBuf::from(&candidate.root).join("new.txt")).unwrap();
    subject.adopt(&candidate).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("new.txt")).unwrap(),
        wrote,
        "a created file must survive adoption — the `--intent-to-add` stage is \
         what puts it in the patch at all"
    );
}

/// An adoption that cannot apply refuses, names why, and leaves the real tree
/// exactly as it found it.
///
/// The conflict is the real tree growing the same *new* file the candidate
/// created — `git apply` refuses "already exists" and writes nothing. Chosen
/// over "both edited the same lines" because `write_file` will not overwrite a
/// file this session has never read, so a candidate rewriting `seed.txt`
/// produces an empty patch and an adoption that vacuously succeeds — which
/// would have made this test pass while proving nothing.
#[tokio::test]
async fn a_conflicting_adoption_refuses_and_writes_nothing() {
    let dir = repo();
    let root = dir.path();
    let subject = substrate(root, Arc::new(WritesOneFile::new("answer.txt")));

    let candidate = subject.create("plugin:p/worker#0").await.unwrap();
    subject
        .work(&candidate, work("worker", "write the answer"))
        .await
        .unwrap();

    // The real tree moves under the candidate: the same path, other bytes.
    std::fs::write(root.join("answer.txt"), "the user's own file\n").unwrap();

    let error = subject.adopt(&candidate).await.unwrap_err();
    match &error {
        CandidateFanoutError::NotAdopted { handle, reason } => {
            assert_eq!(handle, &candidate.handle);
            assert!(
                reason.contains("git apply"),
                "the refusal must name what failed: {reason}"
            );
        }
        other => panic!("expected NotAdopted, got {other:?}"),
    }
    assert_eq!(
        std::fs::read_to_string(root.join("answer.txt")).unwrap(),
        "the user's own file\n",
        "a refused adoption must not have half-applied"
    );
}

/// A candidate that changed nothing adopts successfully and changes nothing —
/// the "the code was already right" answer, which must not be an error.
#[tokio::test]
async fn a_candidate_that_changed_nothing_adopts_as_a_no_op() {
    let dir = repo();
    let root = dir.path();
    let subject = substrate(root, Arc::new(NoTools));

    let candidate = subject.create("plugin:p/worker#0").await.unwrap();
    let report = subject
        .work(&candidate, work("worker", "decide nothing is needed"))
        .await
        .unwrap();
    assert_eq!(report.files_changed, 0);

    subject.adopt(&candidate).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("seed.txt")).unwrap(),
        "one\ntwo\nthree\n",
        "an empty adoption must leave the tree alone"
    );
}

/// A provider that answers in text and calls nothing.
struct NoTools;

#[async_trait]
impl Provider for NoTools {
    fn id(&self) -> &str {
        "no-tools"
    }
    async fn complete_ref(
        &self,
        _request: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        Ok(CompletionResult {
            upstream_provider: None,
            text: "nothing to change".into(),
            tool_calls: Vec::new(),
            usage: CompletionUsage {
                reported: true,
                input_tokens: 1,
                ..CompletionUsage::default()
            },
            model: "no-tools".into(),
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

/// `HOST_TREE_HANDLE` must name no entry in this substrate's table, which is
/// what keeps the shared work tree un-adoptable for free.
///
/// Asserted against a handle this substrate really minted rather than against
/// the constant alone: the property that matters is that the two namespaces
/// cannot meet, and a test that only read the constant would still pass if
/// minting changed to produce it.
#[tokio::test]
async fn a_minted_handle_is_never_the_host_tree_handle() {
    let dir = repo();
    let subject = substrate(dir.path(), Arc::new(NoTools));

    for _ in 0..4 {
        let candidate = subject.create("plugin:p/worker#0").await.unwrap();
        assert_ne!(
            candidate.handle,
            CandidateHandle::new(stella_plugin::HOST_TREE_HANDLE),
            "a minted handle must never collide with the shared work tree's"
        );
    }
}

/// Removing a workspace the substrate no longer holds is success, not an
/// error.
///
/// The end-of-run sweep removes everything, and adoption has already removed
/// the winner — so a substrate that reported the second removal as a failure
/// would print a leak warning on every successful best-of-N run.
#[tokio::test]
async fn removing_an_already_removed_workspace_is_not_a_failure() {
    let dir = repo();
    let subject = substrate(dir.path(), Arc::new(NoTools));

    let candidate = subject.create("plugin:p/worker#0").await.unwrap();
    subject.remove(&candidate).await.unwrap();
    subject
        .remove(&candidate)
        .await
        .expect("a second removal must be a no-op, not a leak report");
}

/// Candidate checkouts and branches stay out of the fleet's namespace, so
/// `stella fleet gc` can never reclaim a live candidate.
#[tokio::test]
async fn candidates_live_outside_the_fleet_namespace() {
    let dir = repo();
    let root = dir.path();
    let subject = substrate(root, Arc::new(NoTools));

    let candidate = subject.create("plugin:p/worker#0").await.unwrap();

    assert!(
        PathBuf::from(&candidate.root).starts_with(
            root.canonicalize()
                .unwrap_or_else(|_| root.to_path_buf())
                .join(CANDIDATES_DIR)
        ),
        "a candidate belongs under {CANDIDATES_DIR}, not the fleet's worktree \
         root: {}",
        candidate.root
    );
    let branches = git(root, &["branch", "--list", "--format=%(refname:short)"]);
    let candidate_branches: Vec<&str> = branches
        .lines()
        .filter(|branch| branch.starts_with(CANDIDATE_BRANCH_PREFIX))
        .collect();
    assert_eq!(
        candidate_branches.len(),
        1,
        "exactly one candidate branch, none of them fleet's: {branches}"
    );
    assert!(
        !branches.lines().any(|branch| branch.starts_with("fleet/")),
        "a candidate branch spelled `fleet/` is one `fleet gc` believes it \
         owns: {branches}"
    );
}

/// The plane over the real substrate: a fan-out really creates N worktrees,
/// and adopting through the plane lands one and takes the rest off disk.
///
/// This is the issue's own definition of done, minus the plugin process — the
/// wire half is witnessed in `stella-runtime`'s
/// `wrapper_candidate_fanout.rs`, and repeating a `/bin/sh` plugin here would
/// prove the socket twice and the substrate not at all.
#[tokio::test]
async fn the_plane_over_this_substrate_fans_out_and_lands_a_winner() {
    let dir = repo();
    let root = dir.path();
    let manifest = stella_plugin::PluginManifest::from_toml_str(
        r#"
name = "candidates"
description = "best of n"

[loop]
participation = "steering"
points = ["before_turn"]
calls = ["candidate_fanout", "adopt_candidate"]
max_fanout_width = 2

[subloop]
stages = ["research"]

[roles.worker]
tier = "worker"

[wrapper]
id = "candidates-v1"

[[wrapper.stages]]
name = "research"
"#,
    )
    .expect("the fixture manifest must parse");

    let plane = crate::wrapper_plugin::candidate_fanout_plane(
        &manifest,
        substrate(root, Arc::new(WritesOneFile::new("answer.txt"))),
    );

    let result = plane
        .fanout(stella_plugin::CandidateFanoutArgs {
            role: "worker".into(),
            instruction: "write the answer".into(),
            width: 2,
        })
        .await
        .expect("the fan-out must run");
    assert_eq!(result.requested, 2);
    assert_eq!(result.candidates.len(), 2, "two real worktrees: {result:?}");
    for candidate in &result.candidates {
        assert!(
            PathBuf::from(&candidate.root).join("answer.txt").exists(),
            "every candidate's turn must have written in its own tree: {candidate:?}"
        );
    }

    // Read before the adoption, because adopting takes the winner's scratch
    // copy with it — and read from the *winner* rather than a fixed index, so
    // the assertion holds whichever candidate the concurrent turns wrote
    // first.
    let winner = result.candidates[1].candidate.clone();
    let winning_bytes =
        std::fs::read_to_string(PathBuf::from(&result.candidates[1].root).join("answer.txt"))
            .unwrap();
    let loser = PathBuf::from(&result.candidates[0].root);
    let adopted = plane
        .adopt(stella_plugin::AdoptCandidateArgs {
            candidate: winner.clone(),
        })
        .await
        .expect("the adoption must land");

    assert_eq!(adopted.adopted, winner);
    assert_eq!(adopted.discarded.len(), 1, "one sibling: {adopted:?}");
    assert_eq!(
        std::fs::read_to_string(root.join("answer.txt")).unwrap(),
        winning_bytes,
        "the tree must hold the winner's bytes, not its sibling's"
    );
    assert!(!loser.exists(), "the losing workspace must be off disk");
    assert!(
        plane.live().is_empty(),
        "an adoption empties the table, so a second one is refused"
    );
}

/// A turn that never reached its first model call is the one outcome reported
/// as an error; a turn that ran is a candidate the plugin scores, however it
/// ended.
///
/// The distinction is the whole reason `work` matches on
/// [`SubAgentOutcome::Refused`] rather than on "did it complete": a naive
/// `if !completed { Err }` would turn every candidate that hit its step cap —
/// an ordinary, scoreable outcome — into a fan-out the plugin is told failed.
#[tokio::test]
async fn a_turn_that_never_ran_is_not_run_and_one_that_did_is_a_candidate() {
    let dir = repo();
    let root = dir.path();
    let subject = substrate(root, Arc::new(NoTools));
    let candidate = subject.create("plugin:p/worker#0").await.unwrap();

    assert!(
        subject
            .work(&candidate, work("worker", "answer"))
            .await
            .is_ok(),
        "a turn that ran and answered is a candidate, not a NotRun"
    );

    // A depth past `MAX_SUB_AGENT_DEPTH` is refused by the engine before any
    // model call — the engine's own refusal rather than a stubbed one, which
    // is what makes this the shape `work` maps to `NotRun`.
    let refused = subject
        .sub_agents
        .dispatch_in_workspace(
            stella_core::subagent::SubAgentSpec {
                depth: u8::MAX,
                ..stella_core::subagent::SubAgentSpec::read_only("deep", "x")
            },
            Arc::new(stella_tools::ToolRegistry::new(root.to_path_buf())),
            crate::agent::session_tool_policy(&subject.cfg),
            stella_core::ports::Principal::Plugin("p".into()),
        )
        .await;
    assert!(
        matches!(refused, SubAgentOutcome::Refused { .. }),
        "the engine must refuse an over-deep child before spending: {refused:?}"
    );
}

/// **A session started in a subdirectory still adopts, and adopts for real.**
///
/// The bug this exists for was silent: `Config::workspace_root` is the
/// process's working directory, a candidate worktree is always a *full*
/// checkout, and `git apply` run from a subdirectory filters the patch to
/// paths under the current directory — finding none in a top-relative patch,
/// applying nothing, and **exiting 0**. An adoption reported success while the
/// user's tree was untouched.
///
/// Two properties, and the second is the one that was broken:
///
/// - The candidate is rooted at *its* copy of the subdirectory, so its turn
///   sees the tree shape the session sees rather than a whole repository the
///   session never had.
/// - The adopted bytes are on disk in the real subdirectory afterwards.
#[tokio::test]
async fn a_session_inside_a_subdirectory_adopts_into_that_subdirectory() {
    let dir = repo();
    let top = dir.path();
    let nested = top.join("crates/inner");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("keep.txt"), "kept\n").unwrap();
    git(top, &["add", "-A"]);
    git(top, &["commit", "-qm", "nested"]);

    // The session runs *here*, not at the repository top.
    let subject = substrate(&nested, Arc::new(WritesOneFile::new("answer.txt")));
    let candidate = subject.create("plugin:p/worker#0").await.unwrap();

    assert!(
        PathBuf::from(&candidate.root).ends_with("crates/inner"),
        "a candidate must be shaped like the workspace that asked for it, not \
         like the whole repository: {}",
        candidate.root
    );

    subject
        .work(&candidate, work("worker", "write the answer"))
        .await
        .unwrap();
    let wrote = std::fs::read_to_string(PathBuf::from(&candidate.root).join("answer.txt")).unwrap();

    subject.adopt(&candidate).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(nested.join("answer.txt")).unwrap(),
        wrote,
        "`git apply` must run at the repository top, or it exits 0 having \
         written nothing and the adoption is a silent lie"
    );
}

#[test]
fn numstat_counts_a_binary_file_as_a_change_with_no_lines() {
    let (files, lines) = numstat_totals("3\t1\tsrc/a.rs\n-\t-\tlogo.png\n");
    assert_eq!(files, 2, "a binary file is still a changed file");
    assert_eq!(lines, 4, "and contributes no line count of its own");
}

#[test]
fn numstat_ignores_blank_and_pathless_rows() {
    assert_eq!(numstat_totals(""), (0, 0));
    assert_eq!(numstat_totals("\n\n"), (0, 0));
    // Two columns and no path is not a numstat row; counting it would inflate
    // the one number a plugin scores on.
    assert_eq!(numstat_totals("1\t2\n"), (0, 0));
}

/// **#4390's ref half.** A candidate worktree shares the repository's `.git`,
/// so `refs/heads/*` is one object the user's own checkout reads. A candidate
/// that commits and then force-moves a shared branch onto its work has
/// corrupted the tree it is asking to be adopted into — and until this, the
/// adoption went through and said nothing.
#[tokio::test]
async fn a_candidate_that_force_moves_a_shared_branch_is_refused_adoption() {
    let dir = repo();
    let root = dir.path();
    git(root, &["branch", "other"]);
    let other_before = git(root, &["rev-parse", "other"]);
    let subject = substrate(root, Arc::new(NoTools));

    let candidate = subject.create("plugin:p/worker#0").await.unwrap();
    let checkout = PathBuf::from(&candidate.root);
    // The candidate does its work, commits it onto its own branch — which is
    // its own business — and then puts it on the user's `other`, which is not.
    std::fs::write(checkout.join("answer.txt"), "the candidate's answer\n").unwrap();
    git(&checkout, &["add", "-A"]);
    git(&checkout, &["commit", "-qm", "candidate work"]);
    git(&checkout, &["branch", "-f", "other", "HEAD"]);

    let error = subject.adopt(&candidate).await.unwrap_err();
    match &error {
        CandidateFanoutError::NotAdopted { handle, reason } => {
            assert_eq!(handle, &candidate.handle);
            assert!(
                reason.contains("refs/heads/other"),
                "the refusal must name the ref: {reason}"
            );
        }
        other => panic!("expected NotAdopted, got {other:?}"),
    }
    assert!(
        !root.join("answer.txt").exists(),
        "a refused adoption writes nothing"
    );
    assert_ne!(
        git(root, &["rev-parse", "other"]),
        other_before,
        "the refusal is detection, not repair: `other` is still where the \
         candidate put it, and the reflog the refusal points at is what puts \
         it back"
    );
}

/// The control ported from the deleted `candidate_ws/ref_escape.rs`: a shared
/// ref that simply *moved* proves nothing. A user committing — or here,
/// rewinding — in their own checkout while a fan-out is in flight must not
/// cost a good candidate its adoption.
#[tokio::test]
async fn a_mid_run_user_rewind_is_not_a_ref_escape() {
    let dir = repo();
    let root = dir.path();
    std::fs::write(root.join("second.txt"), "second\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "second"]);
    let subject = substrate(root, Arc::new(NoTools));

    let candidate = subject.create("plugin:p/worker#0").await.unwrap();
    std::fs::write(
        PathBuf::from(&candidate.root).join("answer.txt"),
        "the candidate's answer\n",
    )
    .unwrap();

    // The user, in their own checkout, throws away their last commit. That
    // moves `refs/heads/main` backwards into history the tree already had.
    git(root, &["reset", "--hard", "-q", "HEAD~1"]);

    subject.adopt(&candidate).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("answer.txt")).unwrap(),
        "the candidate's answer\n",
        "a rewind into already-reachable history is the user's own action"
    );
}

/// **#2651's witness.** A run that ends before anything scores its candidates
/// keeps their work: the patch is on disk, and the sweep that follows takes
/// the checkout and leaves the patch standing.
///
/// The measured shape was a solved task scoring zero — the turn budget ended
/// the run, the plugin never adopted, and the end-of-run sweep deleted the
/// checkout and its branch with nothing written out first.
#[tokio::test]
async fn work_nothing_scored_is_written_out_before_the_sweep_takes_it() {
    let dir = repo();
    let root = dir.path();
    let subject = substrate(root, Arc::new(WritesOneFile::new("answer.txt")));

    let candidate = subject.create("plugin:p/worker#0").await.unwrap();
    subject
        .work(&candidate, work("worker", "write the answer"))
        .await
        .unwrap();
    let wrote = std::fs::read_to_string(PathBuf::from(&candidate.root).join("answer.txt")).unwrap();

    // The run ends abnormally: nothing adopts, and the sweep is about to take
    // everything.
    let kept = subject.preserve_unscored().await;
    assert_eq!(kept.len(), 1, "one unscored candidate: {kept:?}");

    let patch = root.join(CANDIDATES_DIR).join(format!(
        "{}.patch",
        PathBuf::from(&candidate.root)
            .file_name()
            .unwrap()
            .to_string_lossy()
    ));
    assert!(
        kept[0].contains(&patch.display().to_string()),
        "the report must name the file: {}",
        kept[0]
    );

    subject.remove(&candidate).await.unwrap();
    assert!(
        !PathBuf::from(&candidate.root).exists(),
        "premise: the sweep really took the checkout"
    );
    let rescued = std::fs::read_to_string(&patch).expect("the patch outlives the workspace");
    assert!(
        rescued.contains("answer.txt") && rescued.contains(wrote.trim()),
        "and carries what the candidate wrote: {rescued}"
    );
    assert!(
        !root.join("answer.txt").exists(),
        "keeping is not adopting — which candidate deserved to land is a \
         judgement nobody made"
    );
}

/// A candidate that changed nothing writes no patch: a path in a report is
/// worth nothing to open if the file behind it is empty.
#[tokio::test]
async fn a_candidate_that_changed_nothing_is_kept_as_nothing() {
    let dir = repo();
    let subject = substrate(dir.path(), Arc::new(NoTools));
    let candidate = subject.create("plugin:p/worker#0").await.unwrap();
    subject
        .work(&candidate, work("worker", "decide nothing is needed"))
        .await
        .unwrap();

    assert!(subject.preserve_unscored().await.is_empty());
}

/// **#2813's witness.** A candidate that is created and never removed leaves
/// a record on disk, and a later run names it — while a record whose owner is
/// still running is left alone.
///
/// The kill is modelled by dropping the substrate without calling `remove`,
/// which is exactly what a SIGKILL leaves behind: the checkout, the branch,
/// and no in-process table that ever knew about either.
#[tokio::test]
async fn a_candidate_that_outlives_its_run_is_named_by_the_next_one() {
    let dir = repo();
    let root = dir.path();
    let records = root.join(CANDIDATES_DIR);

    let killed = {
        let subject = substrate(root, Arc::new(NoTools));
        let candidate = subject.create("plugin:p/worker#0").await.unwrap();
        // Nothing calls `remove`, and the substrate goes out of scope with the
        // handle still in its table.
        PathBuf::from(&candidate.root)
    };
    assert!(killed.exists(), "premise: the checkout is still on disk");
    assert_eq!(
        record::orphans(&records, &|_| true).len(),
        0,
        "an owner that is still alive is not residue"
    );

    // The record this process wrote names this process, which is alive — so
    // the sweep of a *later* run is modelled by re-pointing the record at a
    // pid that certainly is not.
    let mut mine = record::orphans(&records, &|_| false).remove(0);
    assert_eq!(
        mine.checkout.canonicalize().unwrap(),
        killed,
        "the record names the checkout"
    );
    assert!(mine.branch.starts_with(CANDIDATE_BRANCH_PREFIX));
    mine.pid = u32::MAX - 1;
    mine.write().unwrap();

    let named = substrate(root, Arc::new(NoTools)).orphaned_candidates();
    assert_eq!(named.len(), 1, "the residue is named: {named:?}");
    assert!(
        named[0].contains(&mine.checkout.display().to_string()) && named[0].contains(&mine.branch),
        "and named with both halves of a reclaim: {}",
        named[0]
    );
    assert!(
        killed.exists(),
        "the sweep reports; deleting a checkout it did not create is the \
         user's call, not this host's"
    );
}

/// The other side of #2813: a candidate removed cleanly leaves no record, so
/// the next run's sweep has nothing to say about it.
#[tokio::test]
async fn a_clean_removal_takes_the_record_with_it() {
    let dir = repo();
    let root = dir.path();
    let subject = substrate(root, Arc::new(NoTools));

    let candidate = subject.create("plugin:p/worker#0").await.unwrap();
    assert_eq!(
        record::orphans(&root.join(CANDIDATES_DIR), &|_| false).len(),
        1,
        "premise: creation wrote a record"
    );

    subject.remove(&candidate).await.unwrap();
    assert!(
        record::orphans(&root.join(CANDIDATES_DIR), &|_| false).is_empty(),
        "a candidate that ended is not residue"
    );
}

/// **#4478's stash half.** A candidate's `git stash` pushes onto the one
/// stack the user pops from, and the reachability probe cannot see it: a stash
/// commit is built *on* the candidate's tip, so it is a descendant rather than
/// an ancestor. Attributed by the branch git itself recorded in the stash
/// commit instead.
#[tokio::test]
async fn a_candidate_that_stashes_is_refused_adoption() {
    let dir = repo();
    let root = dir.path();
    let subject = substrate(root, Arc::new(NoTools));

    let candidate = subject.create("plugin:p/worker#0").await.unwrap();
    let checkout = PathBuf::from(&candidate.root);
    std::fs::write(checkout.join("seed.txt"), "one\ntwo\nthree\nfour\n").unwrap();
    git(&checkout, &["stash", "-q"]);

    let error = subject.adopt(&candidate).await.unwrap_err();
    match &error {
        CandidateFanoutError::NotAdopted { reason, .. } => assert!(
            reason.contains("refs/stash"),
            "the refusal must name the stack the candidate pushed onto: {reason}"
        ),
        other => panic!("expected NotAdopted, got {other:?}"),
    }
}

/// The control for the test above: the **user** stashing in their own checkout
/// while a fan-out is in flight names their own branch, so it costs no
/// candidate its adoption.
#[tokio::test]
async fn a_mid_run_user_stash_is_not_a_ref_escape() {
    let dir = repo();
    let root = dir.path();
    let subject = substrate(root, Arc::new(NoTools));

    let candidate = subject.create("plugin:p/worker#0").await.unwrap();
    std::fs::write(
        PathBuf::from(&candidate.root).join("answer.txt"),
        "the candidate's answer\n",
    )
    .unwrap();

    // The user, in their own checkout, parks their work.
    std::fs::write(root.join("seed.txt"), "one\ntwo\nthree\nmine\n").unwrap();
    git(root, &["stash", "-q"]);

    subject.adopt(&candidate).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("answer.txt")).unwrap(),
        "the candidate's answer\n",
        "the user's own stash is the user's own action"
    );
}

/// **#4478's deletion half — the false refusal it removes.** A deletion
/// carries no value to attribute, and until this a candidate lost its adoption
/// because the user tidied up a branch while the fan-out ran.
#[tokio::test]
async fn a_mid_run_user_branch_deletion_is_not_a_ref_escape() {
    let dir = repo();
    let root = dir.path();
    git(root, &["branch", "doomed"]);
    let subject = substrate(root, Arc::new(NoTools));

    let candidate = subject.create("plugin:p/worker#0").await.unwrap();
    std::fs::write(
        PathBuf::from(&candidate.root).join("answer.txt"),
        "the candidate's answer\n",
    )
    .unwrap();

    // The user deletes one of their own branches. The candidate never touched
    // it, and nothing in the candidate's own worktree state says otherwise.
    git(root, &["branch", "-D", "doomed"]);

    subject.adopt(&candidate).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("answer.txt")).unwrap(),
        "the candidate's answer\n",
        "a branch the candidate never sat on is not the candidate's to answer for"
    );
}

/// The other side of the same probe: a candidate that checked a shared branch
/// out — the observed escape — and then deleted it is still refused, because
/// its own per-worktree HEAD reflog names the ref.
#[tokio::test]
async fn a_candidate_that_deletes_a_branch_it_checked_out_is_refused() {
    let dir = repo();
    let root = dir.path();
    git(root, &["branch", "other"]);
    let subject = substrate(root, Arc::new(NoTools));

    let candidate = subject.create("plugin:p/worker#0").await.unwrap();
    let checkout = PathBuf::from(&candidate.root);
    let own_branch = git(&checkout, &["rev-parse", "--abbrev-ref", "HEAD"]);
    git(
        &checkout,
        &["checkout", "-q", "--ignore-other-worktrees", "other"],
    );
    git(
        &checkout,
        &[
            "checkout",
            "-q",
            "--ignore-other-worktrees",
            own_branch.trim(),
        ],
    );
    git(&checkout, &["branch", "-D", "other"]);

    let error = subject.adopt(&candidate).await.unwrap_err();
    match &error {
        CandidateFanoutError::NotAdopted { reason, .. } => assert!(
            reason.contains("refs/heads/other"),
            "the refusal must name the ref: {reason}"
        ),
        other => panic!("expected NotAdopted, got {other:?}"),
    }
}

/// **#4390's mode half, tightening (#2935).** Git records exactly `100644` and
/// `100755`, so a candidate that generates a private key and `chmod 600`s it
/// had that half of its work dropped by `git apply`.
#[cfg(unix)]
#[tokio::test]
async fn adoption_delivers_a_mode_the_patch_could_not_carry() {
    use std::os::unix::fs::PermissionsExt;

    let dir = repo();
    let root = dir.path();
    let subject = substrate(root, Arc::new(NoTools));

    let candidate = subject.create("plugin:p/worker#0").await.unwrap();
    let checkout = PathBuf::from(&candidate.root);
    std::fs::write(checkout.join("server.key"), "-----BEGIN-----\n").unwrap();
    std::fs::set_permissions(
        checkout.join("server.key"),
        PermissionsExt::from_mode(0o600),
    )
    .unwrap();

    subject.adopt(&candidate).await.unwrap();
    assert_eq!(
        std::fs::metadata(root.join("server.key"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o600,
        "the mode is half of what the candidate did, and a patch cannot say it"
    );
}

/// **#4390's mode half, relaxing (#2988).** The direction the pre-#3865
/// substrate could not deliver at all: `git worktree add` checks a tracked
/// file out at git's own normalization, so a candidate cut from a `0600` tree
/// saw `0644` and could not observe — let alone relax — the mode the user set.
///
/// Two assertions, and the first is what makes the second possible: the
/// checkout is stamped faithful at creation, and the change the candidate then
/// makes to it is delivered.
#[cfg(unix)]
#[tokio::test]
async fn a_candidate_sees_the_users_mode_and_can_relax_it() {
    use std::os::unix::fs::PermissionsExt;

    let dir = repo();
    let root = dir.path();
    std::fs::write(root.join("secret.txt"), "shh\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "secret"]);
    std::fs::set_permissions(root.join("secret.txt"), PermissionsExt::from_mode(0o600)).unwrap();
    let subject = substrate(root, Arc::new(NoTools));

    let candidate = subject.create("plugin:p/worker#0").await.unwrap();
    let mine = PathBuf::from(&candidate.root).join("secret.txt");
    assert_eq!(
        std::fs::metadata(&mine).unwrap().permissions().mode() & 0o7777,
        0o600,
        "a candidate must see the tree the user has, or it cannot decide \
         anything about a mode git does not record"
    );

    std::fs::set_permissions(&mine, PermissionsExt::from_mode(0o644)).unwrap();
    subject.adopt(&candidate).await.unwrap();
    assert_eq!(
        std::fs::metadata(root.join("secret.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o644,
        "the candidate relaxed it, and a pure chmod is not a diff — an empty \
         patch still has a mode to deliver"
    );
}
