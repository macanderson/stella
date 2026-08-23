use super::*;

// ---- read-only by default ---------------------------------------------

#[tokio::test]
async fn a_read_only_child_cannot_execute_a_mutating_tool_even_when_it_tries() {
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![
        Ok(tool_call_result("write_file", "c1", 0.001)),
        Ok(text_result("i tried", 0.001)),
    ]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    parent
        .run_sub_agent(
            SubAgentHost::new(&child_provider),
            &SubAgentSpec::read_only("reader", "look around"),
            &mut budget,
            &tx,
        )
        .await;

    assert_eq!(
        tools.writes.load(Ordering::SeqCst),
        0,
        "the restriction is enforced at execution time, not by prompt"
    );
}

#[tokio::test]
async fn write_access_is_opt_in_per_spawn() {
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![
        Ok(tool_call_result("write_file", "c1", 0.001)),
        Ok(text_result("done", 0.001)),
    ]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let spec = SubAgentSpec {
        write_access: true,
        ..SubAgentSpec::read_only("writer", "change something")
    };
    parent
        .run_sub_agent(SubAgentHost::new(&child_provider), &spec, &mut budget, &tx)
        .await;

    assert_eq!(tools.writes.load(Ordering::SeqCst), 1);
}

// ---- budget: carved, settled, and never a hole in the accounting ------

#[tokio::test]
async fn child_spend_settles_into_the_parent_exactly_once() {
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![
        Ok(tool_call_result("read_file", "c1", 0.02)),
        Ok(text_result("found it", 0.03)),
    ]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    budget.record_spend(0.10); // the parent's own prior work
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = parent
        .run_sub_agent(
            SubAgentHost::new(&child_provider),
            &SubAgentSpec::read_only("searcher", "search"),
            &mut budget,
            &tx,
        )
        .await;

    assert!(
        (outcome.cost_usd() - 0.05).abs() < 1e-9,
        "the child's own cost"
    );
    assert!(
        (budget.session_spent_usd() - 0.15).abs() < 1e-9,
        "0.10 parent + 0.05 child, counted once — got {}",
        budget.session_spent_usd()
    );
}

#[tokio::test]
async fn an_enforced_carve_stops_the_child_without_touching_the_parents_turn() {
    let parent_provider = ScriptedProvider::new(vec![]);
    // Each call costs more than the whole carve, so the child trips at the
    // first between-steps check.
    let child_provider = ScriptedProvider::new(vec![
        Ok(tool_call_result("read_file", "c1", 0.50)),
        Ok(text_result("never reached", 0.50)),
    ]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Enforced, None, Some(100.0));
    let (tx, _rx) = mpsc::unbounded_channel();

    let spec = SubAgentSpec {
        budget_usd: Some(0.10),
        ..SubAgentSpec::read_only("spendy", "burn money")
    };
    let outcome = parent
        .run_sub_agent(SubAgentHost::new(&child_provider), &spec, &mut budget, &tx)
        .await;

    assert!(
        matches!(outcome, SubAgentOutcome::Incomplete { .. }),
        "the carve is a wall under Enforced, got {outcome:?}"
    );
    assert!(
        (budget.session_spent_usd() - 0.50).abs() < 1e-9,
        "only the one call that landed is charged, and it IS charged"
    );
    assert_eq!(
        budget.evaluate(),
        BudgetOutcome::Continue,
        "the parent, far under its own cap, keeps going — a child hitting \
         its carve is not the parent's abort"
    );
}

#[tokio::test]
async fn a_child_can_never_be_carved_past_the_parents_remaining_headroom() {
    // The hard-ceiling property at the spawn boundary: the caller asks for
    // ten dollars, the parent has four cents left, and the child is bounded
    // by the four cents.
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![Ok(text_result("hi", 0.001))]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Enforced, None, Some(1.0));
    budget.record_spend(0.96);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let spec = SubAgentSpec {
        budget_usd: Some(10.0),
        ..SubAgentSpec::read_only("greedy", "spend it all")
    };
    parent
        .run_sub_agent(SubAgentHost::new(&child_provider), &spec, &mut budget, &tx)
        .await;

    let started = drain(&mut rx)
        .into_iter()
        .find_map(|event| match event {
            AgentEvent::SubAgent {
                phase: phase @ SubAgentPhase::Started { .. },
            } => Some(phase),
            _ => None,
        })
        .expect("Started is always emitted");
    match started {
        SubAgentPhase::Started { budget_usd, .. } => {
            let carved = budget_usd.expect("a bounded parent bounds its child");
            assert!(
                (carved - 0.04).abs() < 1e-9,
                "asked for 10.0, headroom was 0.04, carved {carved}"
            );
        }
        other => panic!("expected Started, got {other:?}"),
    }
}

#[tokio::test]
async fn an_enforced_parent_with_no_headroom_refuses_before_spending_anything() {
    let parent_provider = ScriptedProvider::new(vec![]);
    // Would answer happily if it were ever asked. It must not be.
    let child_provider = ScriptedProvider::new(vec![Ok(text_result("hello", 0.99))]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Enforced, None, Some(1.0));
    budget.record_spend(1.5);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = parent
        .run_sub_agent(
            SubAgentHost::new(&child_provider),
            &SubAgentSpec::read_only("doomed", "do something"),
            &mut budget,
            &tx,
        )
        .await;

    assert!(
        matches!(outcome, SubAgentOutcome::Refused { .. }),
        "got {outcome:?}"
    );
    assert_eq!(
        child_provider.calls.load(Ordering::SeqCst),
        0,
        "a refusal must cost exactly zero model calls — budget is checked \
         BETWEEN steps, so starting the child would always pay for one"
    );
    assert!((budget.session_spent_usd() - 1.5).abs() < 1e-9);
    // A refusal still brackets, so a fold over the stream never sees an
    // unclosed child.
    let events = drain(&mut rx);
    assert!(matches!(
        finished(&events),
        SubAgentPhase::Finished { status, .. } if status == SubAgentStatus::Refused
    ));
}
