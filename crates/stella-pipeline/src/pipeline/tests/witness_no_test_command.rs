//! #2908: a witness author that returns no `TEST_COMMAND` line must not end
//! the run — it may only downgrade the claim.

use super::*;

/// #2908's witness, in the exact shape the issue describes: a witness author
/// that returns no `TEST_COMMAND` line leaves `effective_cmd` at `None`, and
/// on `main@554bb4f` that ended the run right there — `Unverified`, verdict
/// emitted, done — one revision short of where the worker itself would have
/// stopped. The worker here is scripted to have MORE to do after that: a
/// second tool call the old code never reached because the run never asked.
///
/// A full "same bytes on disk as bare" comparison needs a real filesystem —
/// `OneWritingTool` intentionally touches nothing real (its own docs say so)
/// — so the observable proxy here is the pipeline's own dispatch record,
/// `mutating_actions`, which is exactly what invariant #9's `ChangeSignals`
/// and the ladder itself already treat as the authoritative "the workspace
/// moved" signal. Two dispatched writes reaching the final ladder snapshot is
/// the same claim as two files landing on disk, read off the channel this
/// codebase already trusts for it.
#[tokio::test]
async fn a_witness_author_with_no_test_command_still_lets_the_worker_finish_its_second_round() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        writing_tool_result("about to write the first half"),
        text_result("first pass done"),
        text_result("I looked around but there is nothing here to write a test against"),
        // #2908: without the fix these two script entries are never reached
        // — the run ends at the `Unverified` verdict the line above
        // produces. With the fix, this is the worker's one continue round.
        writing_tool_result("about to write the second half"),
        text_result("second pass done"),
        // The second round made progress (a new dispatched write), so the
        // loop rightly re-evaluates rather than assuming it is done —
        // exactly bare's behavior of continuing until the model stops
        // requesting tools on its own. This third round is where that
        // happens.
        text_result("nothing more to do"),
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let candidate = FakeWorkspace::new(0, vec![true], Ok(vec![]), log.clone());
    let baseline = FakeWorkspace::new(1, vec![], Ok(vec![]), log.clone())
        .with_available_runners(vec!["cargo"]);
    let port = FakeWorkspacePort::new(vec![Ok(candidate), Ok(baseline)], log.clone());
    let (outcome, events, _) = run_isolated(
        &provider,
        &port,
        PipelineConfig::default(),
        "Fix the retry bug",
    )
    .await;
    let outcome = outcome.expect("run succeeds");

    assert!(
        !matches!(outcome.status, PipelineStatus::Aborted { .. }),
        "an unauthorable witness degrades the claim, never the run: {outcome:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Error { message, .. }
                if message.contains("witness author produced no TEST_COMMAND line")
        )),
        "the degradation states why no witness exists: {events:?}"
    );
    assert_eq!(
        outcome.revisions, 2,
        "two continue rounds: the first one made progress (a new dispatched write), so the \
         loop re-evaluates rather than assuming the worker is done, exactly as bare would \
         keep going while the model keeps calling tools; the second made none and ends the \
         run — the run must not stop a revision short of where the worker itself would \
         have stopped, even though nothing could confirm the change (#2908)"
    );
    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(
        matches!(outcome.status, PipelineStatus::Unverified { .. }) && verdict.passed,
        "the claim stays honestly downgraded — this is not a pass being smuggled in: {:?}",
        outcome.status
    );
    assert_eq!(
        verdict
            .ladder
            .as_deref()
            .expect("the verdict carries its snapshot")
            .mutating_actions,
        2,
        "both dispatched writes reach the final ladder snapshot — the second round's work \
         was not dropped, only the claim about it stayed unproven"
    );
}
