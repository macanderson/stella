//! Best-of-N candidate isolation tests — split out of `tests.rs` to keep it
//! small; a child module, so it reaches `run_isolated`,
//! `isolated_config`, and the other fakes above via `super::*`. The shared
//! infrastructure itself (`run_isolated`, `isolated_config`,
//! `FakeWorkspacePort`, `FakeWorkspace`) stays in `tests.rs` — `mcp_prefetch`
//! and `witness_isolation` also depend on it, so it must live in the common
//! ancestor.

use super::*;

/// The core best-of-N isolation contract: every candidate runs against
/// its own workspace surface (the session ports panic if touched), only
/// the winner is adopted, and every workspace is removed.
#[tokio::test]
async fn best_of_two_adopts_only_the_winner_and_removes_every_workspace() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("cand0 done"),
        text_result("cand1 done"),
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let port = FakeWorkspacePort::new(
        vec![
            // Candidate 0: baseline fail, still failing → Failed.
            Ok(FakeWorkspace::new(
                0,
                vec![false, false],
                Ok(vec![]),
                log.clone(),
            )),
            // Candidate 1: fail→pass flip → DeterministicPass (winner).
            Ok(FakeWorkspace::new(
                1,
                vec![false, true],
                Ok(vec![AdoptedChange {
                    path: "src/x.rs".into(),
                    kind: FileChangeKind::Modified,
                    added: 9,
                    removed: 4,
                    diff: Some("@@ -1 +1 @@\n-old\n+new\n".into()),
                }]),
                log.clone(),
            )),
        ],
        log.clone(),
    );

    let (outcome, events, messages) =
        run_isolated(&provider, &port, isolated_config(2), "Fix the failing test").await;
    let outcome = outcome.expect("run succeeds");
    assert_eq!(outcome.status, PipelineStatus::Completed);
    assert_eq!(outcome.candidates_run, 2);
    assert_eq!(outcome.final_text, "cand1 done");
    let verdict = outcome.verdict.expect("winner verified");
    assert!(verdict.passed && verdict.deterministic);
    // The winner's trajectory (not the loser's) was adopted.
    assert!(messages.iter().any(|m| m.content == "cand1 done"));

    let log = log.lock().unwrap().clone();
    assert_eq!(
        log.iter().filter(|e| *e == "create").count(),
        2,
        "one snapshot per candidate: {log:?}"
    );
    assert_eq!(
        log.iter()
            .filter(|e| e.starts_with("adopt"))
            .collect::<Vec<_>>(),
        vec!["adopt:1"],
        "only the winner is ever adopted: {log:?}"
    );
    assert!(
        log.contains(&"remove:0".to_string()) && log.contains(&"remove:1".to_string()),
        "every workspace is removed after the run: {log:?}"
    );
    // The adopted paths surface as FileChange events (the session's file
    // tracking never saw the winner's in-snapshot edits) — carrying the delta
    // the adoption measured, not a bare path. Adopted files used to arrive with
    // no counts and no diff, so the Files tab showed `+0 -0` for all of them.
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::FileChange {
                path,
                kind: FileChangeKind::Modified,
                added: 9,
                removed: 4,
                diff: Some(_),
            } if path == "src/x.rs"
        )),
        "winner adoption must emit FileChange carrying the adopted delta: {events:?}"
    );
}

/// The default single-shot path must never touch the workspace port —
/// zero snapshot machinery when `candidates` is unset.
#[tokio::test]
async fn single_shot_never_touches_the_candidate_workspace_port() {
    let provider = ScriptedProvider::new(vec![text_result("single"), text_result("done")]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![false, true], "@@ -1 +1 @@\n-a\n+b");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let port = FakeWorkspacePort::untouchable();
    let (tx, _rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            diagnostics: &runner,
            tests: &runner,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: Some(&port),
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig {
            test_command: Some("cargo test".into()),
            ..PipelineConfig::default()
        },
    );
    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("run succeeds");
    assert_eq!(outcome.status, PipelineStatus::Completed);
    assert_eq!(outcome.candidates_run, 1);
}

/// A candidate whose snapshot fails is scored as aborted — never run in
/// the shared tree instead — and the remaining candidates continue.
#[tokio::test]
async fn a_failed_snapshot_scores_an_aborted_candidate_and_the_run_continues() {
    // Only ONE worker turn is scripted: candidate 0 must never execute.
    let provider = ScriptedProvider::new(vec![text_result("single"), text_result("cand1 done")]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let port = FakeWorkspacePort::new(
        vec![
            Err(WorkspaceError::Snapshot {
                reason: "disk full".into(),
            }),
            Ok(FakeWorkspace::new(
                1,
                vec![false, true],
                Ok(vec![]),
                log.clone(),
            )),
        ],
        log.clone(),
    );

    let (outcome, events, _) =
        run_isolated(&provider, &port, isolated_config(2), "Fix the failing test").await;
    let outcome = outcome.expect("run succeeds");
    assert_eq!(outcome.status, PipelineStatus::Completed);
    assert_eq!(outcome.final_text, "cand1 done");
    assert_eq!(
        outcome.candidates_run, 1,
        "two candidates were configured but only one reached the worker — the \
         report is what ran, not what was asked for"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Error { message, retryable: true } if message.contains("skipped")
        )),
        "the skipped candidate is warned about, never silent"
    );
    let log = log.lock().unwrap().clone();
    assert_eq!(
        log,
        vec!["create", "create", "seal:1", "adopt:1", "remove:1"],
        "no adoption or removal for the never-created workspace"
    );
}

/// A winner whose adoption conflicts (the user edited mid-run) aborts the
/// run loudly — naming the conflicting paths — and preserves the winner's
/// workspace while still removing the losers'.
#[tokio::test]
async fn an_adoption_conflict_aborts_loudly_and_preserves_the_winner_workspace() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("cand0 done"),
        text_result("cand1 done"),
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let port = FakeWorkspacePort::new(
        vec![
            Ok(FakeWorkspace::new(
                0,
                vec![false, false],
                Ok(vec![]),
                log.clone(),
            )),
            Ok(FakeWorkspace::new(
                1,
                vec![false, true],
                Err(WorkspaceError::Adopt {
                    reason: "`git apply` rejected the patch".into(),
                    paths: vec!["src/conflict.rs".into()],
                    workspace: "/tmp/stella_candidate_ws1".into(),
                }),
                log.clone(),
            )),
        ],
        log.clone(),
    );

    let (outcome, _, _) =
        run_isolated(&provider, &port, isolated_config(2), "Fix the failing test").await;
    let outcome = outcome.expect("an adoption conflict is a loud abort, not a panic");
    match &outcome.status {
        PipelineStatus::Aborted { reason } => {
            assert!(
                reason.contains("src/conflict.rs"),
                "the abort names the conflicting paths: {reason}"
            );
            assert!(
                reason.contains("/tmp/stella_candidate_ws1"),
                "the abort names the preserved workspace: {reason}"
            );
        }
        other => panic!("expected an aborted run, got {other:?}"),
    }
    let log = log.lock().unwrap().clone();
    assert!(
        log.contains(&"remove:0".to_string()),
        "losers are removed: {log:?}"
    );
    assert!(
        !log.contains(&"remove:1".to_string()),
        "the winner's workspace is preserved for recovery: {log:?}"
    );
}

/// Without a workspace port, best-of-N degrades to the historical
/// shared-tree behavior — and says so out loud.
#[tokio::test]
async fn best_of_n_without_a_port_degrades_to_the_shared_tree_with_a_warning() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("cand0 done"),
        text_result("cand1 done"),
    ]);
    let resolver = OneProvider(&provider);
    // One shared runner serves both candidates back-to-back.
    let runner = ScriptedRunner::new(vec![false, false, false, true], "@@ -1 +1 @@\n-a\n+b");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            diagnostics: &runner,
            tests: &runner,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        isolated_config(2),
    );
    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("run succeeds");
    assert_eq!(outcome.status, PipelineStatus::Completed);
    assert_eq!(outcome.candidates_run, 2);
    assert_eq!(outcome.final_text, "cand1 done");
    assert!(
        drain(&mut rx).iter().any(|e| matches!(
            e,
            AgentEvent::Error { message, retryable: true }
                if message.contains("shared working tree")
        )),
        "shared-tree degradation must be loud"
    );
}

/// **Every candidate must see the steer.** A single `TurnSteering` tap is
/// destructive by contract, so handing it to N candidate engines used to mean
/// the first one to reach a step boundary consumed the user's words and the
/// rest ran as if nothing had been typed — and since candidates run
/// sequentially, that was always candidate 1. If the judge then picked another
/// candidate, the steer was absent from the winning transcript entirely.
///
/// Counted via `Steered` events rather than the returned trajectory on purpose:
/// `messages` only carries the WINNER's turn, so asserting there would pass
/// even with the old racing behavior whenever candidate 1 happened to win.
#[tokio::test]
async fn a_mid_turn_steer_reaches_every_candidate_not_just_the_first() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("cand0 done"),
        text_result("cand1 done"),
        text_result("cand2 done"),
    ]);
    let resolver = OneProvider(&provider);
    // Three candidates back-to-back in the shared tree: baseline+result each.
    let runner = ScriptedRunner::new(
        vec![false, false, false, true, false, false],
        "@@ -1 +1 @@\n-a\n+b",
    );
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let steering = SteeringOnce {
        queued: std::sync::Mutex::new(vec!["also update the changelog".into()]),
        drains: std::sync::atomic::AtomicU32::new(0),
    };
    let (tx, mut rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            diagnostics: &runner,
            tests: &runner,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: Some(&steering),
        },
        tx,
        isolated_config(3),
    );
    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    let events = drain(&mut rx);
    let steered = events
        .iter()
        .filter(
            |e| matches!(e, AgentEvent::Steered { text } if text == "also update the changelog"),
        )
        .count();
    assert_eq!(
        steered, 3,
        "all three candidates must observe the steer — one Steered event each; \
         got {steered}: {events:?}"
    );
}

/// A steer must be injected once per candidate, not once per step boundary.
/// The fan-out replays from a per-candidate cursor, so a bug that forgot to
/// advance it would re-inject the user's words on every step of every
/// candidate — quietly multiplying the instruction.
#[tokio::test]
async fn the_steer_is_injected_once_per_candidate_not_once_per_step() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("cand0 done"),
        text_result("cand1 done"),
    ]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![false, false, false, true], "@@ -1 +1 @@\n-a\n+b");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let steering = SteeringOnce {
        queued: std::sync::Mutex::new(vec!["keep it minimal".into()]),
        drains: std::sync::atomic::AtomicU32::new(0),
    };
    let (tx, mut rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            diagnostics: &runner,
            tests: &runner,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: Some(&steering),
        },
        tx,
        isolated_config(2),
    );
    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    let events = drain(&mut rx);
    let steered = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::Steered { text } if text == "keep it minimal"))
        .count();
    assert_eq!(
        steered, 2,
        "exactly one injection per candidate — never a re-injection per step: \
         {events:?}"
    );
    // And the winner's own trajectory carries it exactly once.
    let in_winner = messages
        .iter()
        .filter(|m| m.role == MessageRole::User && m.content == "keep it minimal")
        .count();
    assert_eq!(
        in_winner, 1,
        "the adopted trajectory must carry the steer once, not N times"
    );
}

/// Best-of-N used to be invisible: the same `Execute` stage repeated N times
/// with nothing naming the fan-out, the candidate, or the winner. The run must
/// narrate itself so the TUI shows what the tool is doing while it does it.
#[tokio::test]
async fn a_fan_out_narrates_each_candidate_and_names_the_winner() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("cand0 done"),
        text_result("cand1 done"),
    ]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![false, false, false, true], "@@ -1 +1 @@\n-a\n+b");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            diagnostics: &runner,
            tests: &runner,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        isolated_config(2),
    );
    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    let events = drain(&mut rx);
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Text { delta } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        text.contains("candidate 1/2 starting"),
        "each candidate must announce itself: {text:?}"
    );
    assert!(
        text.contains("candidate 2/2 starting"),
        "including the last one: {text:?}"
    );
    assert!(
        text.contains("the shared working tree"),
        "a shared-tree fan-out must say so — isolation changes what a loser \
         leaves behind: {text:?}"
    );
    assert!(
        text.contains("won"),
        "the winner must be named, or a losing verdict looks like the run \
         ignoring the user: {text:?}"
    );
}

/// A single-candidate run must stay silent: `run_best_of_n` also serves `n == 1`
/// when a witness is authored, and "candidate 1/1" there is noise, not signal.
#[tokio::test]
async fn a_single_candidate_run_emits_no_fan_out_narration() {
    let provider = ScriptedProvider::new(vec![text_result("single"), text_result("only done")]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![false, true], "@@ -1 +1 @@\n-a\n+b");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            diagnostics: &runner,
            tests: &runner,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        isolated_config(1),
    );
    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    let events = drain(&mut rx);
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Text { delta } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        !text.contains("best-of-"),
        "a one-candidate run must not narrate a fan-out: {text:?}"
    );
}

/// The same fan-out contract on the ISOLATED path. There are two candidate
/// loops — shared-tree and per-workspace — and each attaches steering to its
/// own engine, so fixing one and not the other would leave the real default
/// (isolation is on wherever a git workspace exists) still racing.
#[tokio::test]
async fn every_isolated_candidate_also_sees_the_steer() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("cand0 done"),
        text_result("cand1 done"),
    ]);
    let resolver = OneProvider(&provider);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let port = FakeWorkspacePort::new(
        vec![
            // Candidate 0 stays red; candidate 1 flips and wins.
            Ok(FakeWorkspace::new(
                0,
                vec![false, false],
                Ok(vec![]),
                log.clone(),
            )),
            Ok(FakeWorkspace::new(
                1,
                vec![false, true],
                Ok(vec![]),
                log.clone(),
            )),
        ],
        log.clone(),
    );
    // Session ports must never be touched on the isolated path.
    let diagnostics = NeverRunner;
    let repo_status = NeverRepoStatus;
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let steering = SteeringOnce {
        queued: std::sync::Mutex::new(vec!["prefer the smaller diff".into()]),
        drains: std::sync::atomic::AtomicU32::new(0),
    };
    let (tx, mut rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            diagnostics: &diagnostics,
            tests: &diagnostics,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: Some(&port),
            mcp_prefetch: None,
            steering: Some(&steering),
        },
        tx,
        isolated_config(2),
    );
    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    let events = drain(&mut rx);
    let steered = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::Steered { text } if text == "prefer the smaller diff"))
        .count();
    assert_eq!(
        steered, 2,
        "both isolated candidates must observe the steer: {events:?}"
    );
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Text { delta } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        text.contains("its own isolated workspace"),
        "an isolated fan-out must say the candidates are isolated: {text:?}"
    );
}
