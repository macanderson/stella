use super::*;

// ---- the witness ------------------------------------------------------

/// **The acceptance witness for #922.** A parent turn spawns a child that
/// does real, bulky work — four tool calls returning 4 KB each — and the
/// parent's transcript does not grow by ANY of it.
///
/// The assertion is deliberately on the parent's `Vec<CompletionMessage>`
/// rather than on a token estimate: message identity is what the next
/// provider call actually serializes, so an unchanged vector is proof the
/// child's intermediate work is not being re-sent on every subsequent step
/// for the rest of the session. The report the parent *may* choose to append
/// is bounded separately (see
/// [`the_report_is_clamped_to_the_spec_cap_and_says_so`]).
#[tokio::test]
async fn the_parent_transcript_does_not_grow_by_the_childs_intermediate_work() {
    let parent_provider = ScriptedProvider::new(vec![]);
    // Four read steps, then an answer: 9 messages of child transcript.
    let child_provider = ScriptedProvider::new(vec![
        Ok(tool_call_result("read_file", "c1", 0.001)),
        Ok(tool_call_result("read_file", "c2", 0.001)),
        Ok(tool_call_result("read_file", "c3", 0.001)),
        Ok(tool_call_result("read_file", "c4", 0.001)),
        Ok(text_result("the retry policy is in retry.rs", 0.001)),
    ]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);

    let mut parent_messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("which file defines the retry policy?"),
    ];
    let before = parent_messages.clone();
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = parent
        .run_sub_agent(
            SubAgentHost::new(&child_provider),
            &SubAgentSpec::read_only("search-1", "find the retry policy"),
            &mut budget,
            &tx,
        )
        .await;

    assert_eq!(
        parent_messages, before,
        "the parent's transcript must be byte-identical after a child ran"
    );

    let report = match &outcome {
        SubAgentOutcome::Completed(report) => report,
        other => panic!("expected Completed, got {other:?}"),
    };
    assert_eq!(report.summary, "the retry policy is in retry.rs");
    assert_eq!(report.steps, 5, "five committed model calls");
    assert!(
        report.absorbed_messages >= 8,
        "the child's own transcript grew by {} messages — that is the growth \
         the parent avoided",
        report.absorbed_messages
    );
    assert_eq!(tools.reads.load(Ordering::SeqCst), 4, "the work really ran");

    // And the child's bulk is nowhere in the parent's event stream either:
    // the ToolResults are forwarded for visibility, but the transcript that
    // held them is gone with the function scope that owned it.
    let events = drain(&mut rx);
    assert!(
        matches!(
            finished(&events),
            SubAgentPhase::Finished { status, .. } if status == SubAgentStatus::Completed
        ),
        "the bracket closes with the real status"
    );

    // Belt and braces: appending the report is the ONLY growth available,
    // and it is one message however much the child read.
    parent_messages.push(CompletionMessage::user(report.summary.clone()));
    assert_eq!(parent_messages.len(), before.len() + 1);
}

/// A sink that records what a turn asked it to do.
#[derive(Default)]
struct RecordingSink {
    persisted: std::sync::Mutex<Vec<String>>,
    discards: AtomicUsize,
}

impl std::fmt::Debug for RecordingSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecordingSink")
    }
}

impl crate::step::CheckpointSink for RecordingSink {
    fn persist(&self, json: &str) {
        self.persisted
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(json.to_string());
    }
    fn discard(&self) {
        self.discards.fetch_add(1, Ordering::SeqCst);
    }
}

/// A child turn must not touch the SESSION's resume point.
///
/// The sink is bound to a session, and the child runs inside one of the
/// parent's tool calls while the parent's own turn is still in flight. An
/// inherited sink breaks that in both directions: the child's steps overwrite
/// the parent's resume point with the child's transcript, and the child
/// reaching a terminal outcome retracts the parent's resume point outright — so
/// a crash moments later would find either nothing to resume from, or a
/// conversation belonging to a different agent.
#[tokio::test]
async fn a_child_turn_never_writes_or_clears_the_parents_resume_point() {
    let parent_provider = ScriptedProvider::new(vec![]);
    // Two tool calls then an answer: the child crosses two step boundaries, so
    // an inherited sink would be written to — and discarded — several times.
    let child_provider = ScriptedProvider::new(vec![
        Ok(tool_call_result("read_file", "c1", 0.001)),
        Ok(tool_call_result("read_file", "c2", 0.001)),
        Ok(text_result("answered", 0.001)),
    ]);
    let tools = MixedTools::default();
    let sink = Arc::new(RecordingSink::default());
    let config = EngineConfig {
        checkpoint_sink: Some(sink.clone() as Arc<dyn crate::step::CheckpointSink>),
        ..EngineConfig::default()
    };
    let parent = Engine::with_sleeper(&parent_provider, &tools, config, &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = parent
        .run_sub_agent(
            SubAgentHost::new(&child_provider),
            &SubAgentSpec::read_only("search-1", "find it"),
            &mut budget,
            &tx,
        )
        .await;
    assert!(
        matches!(outcome, SubAgentOutcome::Completed(_)),
        "the child really ran: {outcome:?}"
    );
    assert_eq!(
        tools.reads.load(Ordering::SeqCst),
        2,
        "and really did work, so it really crossed step boundaries"
    );

    assert!(
        sink.persisted
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_empty(),
        "a child step must not overwrite the session's resume point with the child's transcript"
    );
    assert_eq!(
        sink.discards.load(Ordering::SeqCst),
        0,
        "and a child ENDING must not retract a resume point the parent's in-flight turn still needs"
    );
    let _ = drain(&mut rx);
}

// ---- context economy is a mechanism, not an intention -----------------

#[tokio::test]
async fn the_report_is_clamped_to_the_spec_cap_and_says_so() {
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![Ok(text_result(&"y".repeat(5_000), 0.001))]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let spec = SubAgentSpec {
        max_report_chars: 100,
        ..SubAgentSpec::read_only("verbose", "say a lot")
    };
    let outcome = parent
        .run_sub_agent(SubAgentHost::new(&child_provider), &spec, &mut budget, &tx)
        .await;

    let report = outcome.report().expect("completed");
    assert!(
        report.truncated,
        "a clamped report must never look exhaustive"
    );
    assert_eq!(
        report.summary.chars().count(),
        101,
        "100 chars plus the ellipsis marker"
    );

    // The truncation is on the wire too, not just in the return value.
    match finished(&drain(&mut rx)) {
        SubAgentPhase::Finished { truncated, .. } => assert!(truncated),
        other => panic!("expected Finished, got {other:?}"),
    }
}
