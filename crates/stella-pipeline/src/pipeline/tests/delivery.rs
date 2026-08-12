//! Delivery witnesses (#2927): the verdict is a claim about the work, so a
//! run that does not reach a passing one still delivers what the worker built.
//!
//! The mirror-image contract — an *integrity* refusal still withholds — is
//! pinned next door in `witness_isolation` (escaped candidate, tampered
//! witness artifact, post-verification drift), and the rung-by-rung argument
//! for the split is in `super::super::delivery`'s module doc.

use super::*;

/// A modified file for the winner's adoption to report, so the test can assert
/// the bytes reached the stream rather than only that `adopt` was called.
fn delivered_change() -> AdoptedChange {
    AdoptedChange {
        path: "src/solution.rs".into(),
        kind: FileChangeKind::Modified,
        added: 12,
        removed: 3,
        diff: Some("@@ -1 +1 @@\n-old\n+new\n".into()),
    }
}

/// **The witness for #2927.** Both candidates end red, so the run's verdict is
/// `passed: false` and its status is `VerificationFailed` — and the winner's
/// work is delivered into the working tree anyway.
///
/// On `main` this asserted the opposite (`..._never_adopts_and_removes_all_
/// candidates`, renamed and inverted here): adoption required
/// `verdict.passed`, so a red verdict deleted the candidate workspace with the
/// work still in it. Measured over ArenaBench `w10p`, that gate accounted for
/// eight of fifteen pipeline losses, including a trial whose final event was
/// `18 passed in 1.44s`.
#[tokio::test]
async fn failed_final_verification_still_delivers_the_winners_work() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("candidate zero"),
        text_result("candidate one"),
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let port = FakeWorkspacePort::new(
        vec![
            Ok(FakeWorkspace::new(
                0,
                vec![false, false],
                Ok(vec![delivered_change()]),
                log.clone(),
            )),
            Ok(FakeWorkspace::new(
                1,
                vec![false, false],
                Ok(vec![]),
                log.clone(),
            )),
        ],
        log.clone(),
    );

    let (outcome, events, _) =
        run_isolated(&provider, &port, isolated_config(2), "Fix the failing test").await;
    let outcome = outcome.expect("red verification is a terminal outcome");
    // Honesty first: delivering the work changes nothing about the claim.
    assert!(
        matches!(outcome.status, PipelineStatus::VerificationFailed { .. }),
        "a delivered run must still report the rung it came to rest on: {:?}",
        outcome.status
    );
    assert!(
        !outcome
            .verdict
            .expect("the ladder reached a verdict")
            .passed,
        "delivery must never flip the verdict to a pass"
    );

    let log = log.lock().unwrap().clone();
    assert_eq!(
        log.iter()
            .filter(|entry| entry.starts_with("adopt"))
            .collect::<Vec<_>>(),
        vec!["adopt:0"],
        "the winner's work is delivered and the loser's is not: {log:?}"
    );
    assert!(
        log.contains(&"remove:0".to_string()) && log.contains(&"remove:1".to_string()),
        "every workspace is still cleaned up: {log:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::FileChange { path, added: 12, removed: 3, .. }
                if path == "src/solution.rs"
        )),
        "the delivered bytes reach the stream as FileChange: {events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::Error { message, retryable: true } if message.contains("UNPROVEN")
        )),
        "an unproven delivery says so on the rail: {events:?}"
    );
}

/// **The witness for #2941.** Candidate delivery used to happen exactly once,
/// at the very end of the run, after a verdict — so a process killed before
/// that point (the harness's wall-clock cap, a SIGKILL, a crash) discarded
/// the whole candidate, work already finished included, because nothing this
/// process had not yet executed could ever run. A unit test cannot simulate
/// the kill itself (the test process would die with it); what it CAN show is
/// the fact that makes the kill survivable — that a finished step's work
/// reaches the real tree at the step boundary, not at the end of the run. If
/// step one's `deliver:0` is already in the log before step two's turn even
/// starts, then a kill at any point after step one — including one this
/// process never gets to react to — has already lost nothing.
///
/// Fails on `main`: nothing but the final winner adoption ever writes to the
/// real tree, so `deliver:0` never appears and only `adopt:0` — at the very
/// end — would be in this log.
#[tokio::test]
async fn a_finished_steps_work_is_delivered_before_the_next_steps_turn_starts() {
    let provider = ScriptedProvider::new(vec![
        text_result("multi"),
        text_result(r#"["step one","step two"]"#),
        text_result("did step one"),
        text_result("did step two"),
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let port = FakeWorkspacePort::new(
        vec![Ok(FakeWorkspace::new(
            0,
            vec![false, true],
            Ok(vec![delivered_change()]),
            log.clone(),
        ))],
        log.clone(),
    );

    // A lone candidate is not isolated by default (#1719) — `create_worktrees`
    // is the deliberate trigger here; an authored witness is the other, and
    // either reaches the same isolated path this fix lives on.
    let config = PipelineConfig {
        create_worktrees: crate::ports::WorktreePolicy::Always,
        ..isolated_config(1)
    };
    let (outcome, _events, _) =
        run_isolated(&provider, &port, config, "Fix the failing test").await;
    let outcome = outcome.expect("run succeeds");
    assert_eq!(outcome.status, PipelineStatus::Completed);

    let log = log.lock().unwrap().clone();
    let first_delivery = log.iter().position(|entry| entry == "deliver:0");
    let final_adoption = log.iter().position(|entry| entry == "adopt:0");
    assert!(
        first_delivery.is_some(),
        "step one's finished work must be delivered at its own step boundary: {log:?}"
    );
    assert!(
        first_delivery < final_adoption,
        "the checkpoint delivery must land strictly before the run's final \
         winner adoption, so a kill any time after it has already lost \
         nothing: {log:?}"
    );
}

/// The rail line is a warning, not a failure, and it is absent when the run
/// genuinely proved itself — a passing delivery must not be narrated as
/// unproven.
#[tokio::test]
async fn a_proven_delivery_carries_no_unproven_notice() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("candidate zero"),
        text_result("candidate one"),
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
            // Fail → pass: a genuine flip, so this candidate wins on proof.
            Ok(FakeWorkspace::new(
                1,
                vec![false, true],
                Ok(vec![delivered_change()]),
                log.clone(),
            )),
        ],
        log.clone(),
    );

    let (outcome, events, _) =
        run_isolated(&provider, &port, isolated_config(2), "Fix the failing test").await;
    let outcome = outcome.expect("run succeeds");
    assert_eq!(outcome.status, PipelineStatus::Completed);
    assert!(
        !events.iter().any(|event| matches!(
            event,
            AgentEvent::Error { message, .. } if message.contains("UNPROVEN")
        )),
        "a proven run must not be narrated as unproven: {events:?}"
    );
}
