use super::*;

// ---- failure is data --------------------------------------------------

#[tokio::test]
async fn an_aborted_child_salvages_the_last_answer_it_paid_for() {
    // The child answers, is told to keep going, then hits its step cap. Its
    // text is real work; throwing it away with the transcript would make an
    // abort strictly worse than never spawning.
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![
        Ok(CompletionResult {
            upstream_provider: None,
            text: "partial finding: it is in retry.rs".into(),
            tool_calls: vec![ToolCall {
                call_id: "c1".into(),
                name: "read_file".into(),
                input: json!({ "path": "c1" }),
            }],
            usage: CompletionUsage {
                reported: true,
                ..CompletionUsage::default()
            },
            model: "scripted".into(),
            cost_usd: 0.01,
            finish_reason: None,
        }),
        Ok(tool_call_result("read_file", "c2", 0.01)),
    ]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let spec = SubAgentSpec {
        max_steps: 2,
        ..SubAgentSpec::read_only("capped", "look")
    };
    let outcome = parent
        .run_sub_agent(SubAgentHost::new(&child_provider), &spec, &mut budget, &tx)
        .await;

    match outcome {
        SubAgentOutcome::Incomplete { report, reason } => {
            assert_eq!(report.summary, "partial finding: it is in retry.rs");
            assert!(!reason.is_empty(), "the abort always names itself");
            assert!(
                (report.cost_usd - 0.02).abs() < 1e-9,
                "the salvage does not hide what it cost"
            );
        }
        other => panic!("expected Incomplete, got {other:?}"),
    }
}

#[tokio::test]
async fn a_failed_child_never_becomes_an_error_the_parent_has_to_handle() {
    // The provider errors terminally on the first call. The parent still
    // gets a value back, with the reason in it.
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = parent
        .run_sub_agent(
            SubAgentHost::new(&child_provider),
            &SubAgentSpec::read_only("broken", "try"),
            &mut budget,
            &tx,
        )
        .await;

    match outcome {
        SubAgentOutcome::Incomplete { report, reason } => {
            assert!(report.summary.is_empty(), "nothing was produced to salvage");
            assert!(!reason.is_empty());
        }
        other => panic!("expected Incomplete, got {other:?}"),
    }
}

#[tokio::test]
async fn nesting_deeper_than_the_cap_is_refused_before_spending() {
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![Ok(text_result("hi", 0.01))]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let spec = SubAgentSpec {
        depth: MAX_SUB_AGENT_DEPTH + 1,
        ..SubAgentSpec::read_only("too-deep", "recurse")
    };
    let outcome = parent
        .run_sub_agent(SubAgentHost::new(&child_provider), &spec, &mut budget, &tx)
        .await;

    assert!(matches!(outcome, SubAgentOutcome::Refused { .. }));
    assert_eq!(child_provider.calls.load(Ordering::SeqCst), 0);
    // The boundary itself is allowed — the cap is a maximum, not an exclusive
    // bound.
    let ok = SubAgentSpec {
        depth: MAX_SUB_AGENT_DEPTH,
        ..SubAgentSpec::read_only("at-the-edge", "recurse")
    };
    let outcome = parent
        .run_sub_agent(SubAgentHost::new(&child_provider), &ok, &mut budget, &tx)
        .await;
    assert!(matches!(outcome, SubAgentOutcome::Completed(_)));
}

// ---- the event plane --------------------------------------------------

#[tokio::test]
async fn the_childs_stage_and_narration_never_reach_the_parents_stream() {
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![
        Ok(tool_call_result("read_file", "c1", 0.01)),
        Ok(text_result("the answer", 0.01)),
    ]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    parent
        .run_sub_agent(
            SubAgentHost::new(&child_provider),
            &SubAgentSpec::read_only("quiet", "work"),
            &mut budget,
            &tx,
        )
        .await;
    let events = drain(&mut rx);

    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::Stage { .. })),
        "a child's stage boundary read as the parent's is exactly the \
         confusion this filter exists to prevent"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::Text { .. })),
        "the child's narration is a draft; the report ships once, on Finished"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::TurnComplete { .. })),
        "the child does not terminate the parent's stream"
    );

    // ...but the report IS on the wire, exactly once.
    match finished(&events) {
        SubAgentPhase::Finished { summary, .. } => assert_eq!(summary, "the answer"),
        other => panic!("expected Finished, got {other:?}"),
    }
}

/// **#4383's witness.** A child's metering records reach the parent naming the
/// child. Without the stamp they are indistinguishable from the lead's own
/// calls, which is how execution 225 of session `ses-1787465453163-60967` came
/// to record ninety `worker` rows for a turn that fanned out five delegates.
///
/// The bracket cannot answer this, and that is the whole reason for the field:
/// independent delegates are dispatched concurrently, so `Started`/`Finished`
/// pairs interleave and enclose each other's calls.
#[tokio::test]
async fn a_childs_metering_records_name_the_child_that_spent_them() {
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![Ok(text_result("done", 0.02))]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    parent
        .run_sub_agent(
            SubAgentHost::new(&child_provider),
            &SubAgentSpec::read_only("researcher-7", "work"),
            &mut budget,
            &tx,
        )
        .await;
    let events = drain(&mut rx);

    let spenders: Vec<Option<String>> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::StepUsage { sub_agent_id, .. } => Some(sub_agent_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        spenders,
        vec![Some("researcher-7".to_string())],
        "every call the child made must name the child"
    );
    // And the bracket still says the same thing, so a consumer that reads
    // either one gets the same answer rather than two competing ones.
    match finished(&events) {
        SubAgentPhase::Finished { agent_id, .. } => assert_eq!(agent_id, "researcher-7"),
        other => panic!("expected Finished, got {other:?}"),
    }
}

/// A call the lead made itself is the lead's, and must not acquire an id from
/// a child that ran beside it. `None` is a fact — "the lead spent this" — so a
/// reader summing by spender gets the turn's real shape.
#[tokio::test]
async fn the_leads_own_calls_name_no_sub_agent() {
    let provider = ScriptedProvider::new(vec![Ok(text_result("answered", 0.05))]);
    let tools = MixedTools::default();
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut messages = vec![CompletionMessage::user("do it")];

    let _ = engine.run_turn(&mut messages, &mut budget, &tx).await;
    let events = drain(&mut rx);

    let spenders: Vec<Option<String>> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::StepUsage { sub_agent_id, .. } => Some(sub_agent_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(spenders, vec![None], "{spenders:?}");
}

#[tokio::test]
async fn step_usage_and_tool_activity_reach_the_parent_so_cost_rolls_up() {
    // Dropping StepUsage is precisely how child spend would vanish from
    // `stella stats` and quietly falsify the $/resolved-task number.
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![
        Ok(tool_call_result("read_file", "c1", 0.01)),
        Ok(text_result("done", 0.02)),
    ]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    parent
        .run_sub_agent(
            SubAgentHost::new(&child_provider),
            &SubAgentSpec::read_only("metered", "work"),
            &mut budget,
            &tx,
        )
        .await;
    let events = drain(&mut rx);

    let metered: f64 = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::StepUsage { cost_usd, .. } => Some(*cost_usd),
            _ => None,
        })
        .sum();
    assert!(
        (metered - 0.03).abs() < 1e-9,
        "every child call must appear in the parent's metering stream, got {metered}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolStart { .. })),
        "a child's activity stays visible even though its narration does not"
    );
    // And the parent's own post-settlement numbers are re-ticked, since the
    // child's carve-scoped ticks were dropped.
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::BudgetTick { session_spent_usd: Some(spent), .. } if (*spent - 0.03).abs() < 1e-9
        )),
        "the HUD must not sit stale across a child run"
    );
}

/// **The witness for #4624.** A child's tool traffic names the child, on both
/// halves of the pair.
///
/// Fails before this change: `ToolStart`/`ToolResult` carried no
/// `sub_agent_id` at all, so every delegate's call landed in `tool_calls`
/// under the parent execution id with nothing naming the child — the bracket
/// cannot stand in for it, because independent delegates are dispatched
/// concurrently and no `Started`/`Finished` pair encloses a particular call.
#[tokio::test]
async fn a_childs_tool_calls_name_the_child_that_ran_them() {
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![
        Ok(tool_call_result("read_file", "c1", 0.01)),
        Ok(text_result("done", 0.02)),
    ]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    parent
        .run_sub_agent(
            SubAgentHost::new(&child_provider),
            &SubAgentSpec::read_only("researcher-7", "work"),
            &mut budget,
            &tx,
        )
        .await;
    let events = drain(&mut rx);

    let runners: Vec<Option<String>> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolStart { sub_agent_id, .. }
            | AgentEvent::ToolResult { sub_agent_id, .. } => Some(sub_agent_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        runners,
        vec![
            Some("researcher-7".to_string()),
            Some("researcher-7".to_string())
        ],
        "the announcement and the result both name the child: {runners:?}"
    );
}

/// The other half of the same fact: a call the lead made itself is the lead's,
/// and `None` is that answer rather than the absence of one.
#[tokio::test]
async fn the_leads_own_tool_calls_name_no_sub_agent() {
    let provider = ScriptedProvider::new(vec![
        Ok(tool_call_result("read_file", "c1", 0.01)),
        Ok(text_result("answered", 0.02)),
    ]);
    let tools = MixedTools::default();
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut messages = vec![CompletionMessage::user("do it")];

    let _ = engine.run_turn(&mut messages, &mut budget, &tx).await;
    let events = drain(&mut rx);

    let runners: Vec<Option<String>> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolStart { sub_agent_id, .. }
            | AgentEvent::ToolResult { sub_agent_id, .. } => Some(sub_agent_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(runners, vec![None, None], "{runners:?}");
}

#[test]
fn the_forward_filter_fails_toward_visible() {
    // A new AgentEvent variant must default to *forwarded*: a redundant HUD
    // line is a cosmetic bug, a dropped metering row is a falsified invoice.
    assert!(forwards_to_parent(&AgentEvent::Error {
        message: "boom".into(),
        retryable: false,
    }));
    assert!(forwards_to_parent(&AgentEvent::TaskUpdate {
        tasks: vec![]
    }));
    assert!(!forwards_to_parent(&AgentEvent::Stage {
        name: stella_protocol::StageKind::Execute.into(),
        scope: stella_protocol::StageScope::Turn
    }));
}
