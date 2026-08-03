//! The interactive scope-review seam: a run that is NOT headless and carries a
//! real approval gate must raise the card and, on approve, execute — the
//! posture the command deck adopted once it grew a native approval surface.
//! Its headless twin (`ScopeReviewRequiredHeadless`) lives in the parent
//! module; this is the branch that had no coverage.

use super::*;

/// A gate that answers differently each time it is consulted — needed for the
/// revision loop, where the whole point is that the reviewer's second answer
/// differs from the first. Records the proposals it saw so a test can assert
/// the re-planned card actually changed. Runs out by aborting (fail-closed),
/// so a loop bug shows up as an abort instead of hanging the test.
struct ScriptedGate {
    answers: std::sync::Mutex<std::collections::VecDeque<ScopeDecision>>,
    seen: std::sync::Mutex<Vec<ScopeProposal>>,
}

impl ScriptedGate {
    fn new(answers: Vec<ScopeDecision>) -> Self {
        Self {
            answers: std::sync::Mutex::new(answers.into()),
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn cards_raised(&self) -> Vec<ScopeProposal> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl ApprovalGate for ScriptedGate {
    async fn review(&self, proposal: &ScopeProposal) -> ScopeDecision {
        self.seen.lock().unwrap().push(proposal.clone());
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(ScopeDecision::Abort)
    }
}

/// The deck's configuration: NOT headless, with a real approval gate. The
/// same 6-step plan that returns `ScopeReviewRequiredHeadless` for a headless
/// run must instead raise a `ScopeReview` card and, on approve, carry on into
/// execution. This is the seam the command deck relies on — before it was
/// wired, an interactive session dead-ended on any plan over five steps and
/// pointed the user at `headless_scope_bypass`, which that path never reads.
#[tokio::test]
async fn interactive_scope_review_approve_proceeds_past_the_gate() {
    let provider = ScriptedProvider::new(vec![
        text_result("multi"),
        text_result(r#"["s1","s2","s3","s4","s5","s6"]"#),
        text_result("working"),
        text_result("done"),
    ]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![], "");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = FixedGate(ScopeDecision::Approve);
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
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        // `headless: false` is the default — the deck's exact posture.
        PipelineConfig::default(),
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let result = pipeline
        .run(
            "Refactor across the codebase and then update all callers",
            &mut messages,
            &mut budget,
        )
        .await;

    // Whatever else the scripted run does downstream, the scope gate must not
    // be what stopped it.
    if let Err(err) = &result {
        assert_ne!(
            err.cause,
            PipelineError::ScopeReviewRequiredHeadless,
            "an interactive run has someone to ask: {err:?}"
        );
    }
    let events = drain(&mut rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ScopeReview { .. })),
        "the approval card must be raised so the deck can render it"
    );
    assert!(
        stages(&events).contains(&StageKind::Execute),
        "an approved plan proceeds into execution"
    );
}

/// The reviewer's third answer. A note at the card must send the *goal* back to
/// the planner carrying that note, raise a fresh card for the new plan, and —
/// once approved — execute the revised plan. Before this, a typed line at the
/// card could not reach the gate at all: on the deck it was read as a new
/// mid-turn request and spawned a sidecar sub-session while the review sat
/// parked, so the only reachable key was Esc, which aborts.
#[tokio::test]
async fn a_revision_note_re_plans_and_gates_again() {
    let provider = ScriptedProvider::new(vec![
        text_result("multi"),
        // First plan: 6 steps, over the 5-step threshold, so it gates.
        text_result(r#"["s1","s2","s3","s4","s5","s6"]"#),
        // Re-plan after the note: still over the threshold, so it gates again
        // (a revision does not skip review — the new scope is also unreviewed).
        text_result(r#"["only the dialog","wire the key","test it","doc it","tidy","ship"]"#),
        text_result("working"),
        text_result("done"),
    ]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![], "");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = ScriptedGate::new(vec![
        ScopeDecision::Revise {
            note: "only the ctrl+O dialog, skip the rest".to_string(),
        },
        ScopeDecision::Approve,
    ]);
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
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig::default(),
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let _ = pipeline
        .run("add issue keybindings", &mut messages, &mut budget)
        .await;

    let events = drain(&mut rx);
    let cards = approvals.cards_raised();
    assert_eq!(
        cards.len(),
        2,
        "the revised plan must be reviewed too — a note is not an approval"
    );
    assert!(
        cards[1].steps.iter().any(|s| s.contains("only the dialog")),
        "the second card must show the RE-PLANNED steps, not the rejected ones: {:?}",
        cards[1].steps
    );
    // The planner ran twice: the loop went back through PLAN rather than
    // patching the plan locally.
    assert_eq!(
        stages(&events)
            .iter()
            .filter(|s| **s == StageKind::Plan)
            .count(),
        2,
        "a revision re-runs the planner"
    );
    assert!(
        stages(&events).contains(&StageKind::Execute),
        "the approved revision executes"
    );
    // The reviewer's own words reached the planner.
    assert!(
        provider
            .prompts()
            .iter()
            .any(|p| p.contains("only the ctrl+O dialog, skip the rest")),
        "the note must be folded into the re-plan prompt"
    );
}

/// The loop is bounded. A reviewer who keeps typing notes cannot spend a turn's
/// budget on planner calls forever — and the refusal must say what still works,
/// because the alternative is a run that ends on an unexplained abort.
#[tokio::test]
async fn endless_revision_notes_stop_at_the_cap_and_say_why() {
    // One more note than the cap allows, all of them answered with a note.
    let answers: Vec<ScopeDecision> = (0..MAX_SCOPE_REVISIONS + 1)
        .map(|i| ScopeDecision::Revise {
            note: format!("try again {i}"),
        })
        .collect();
    // A plan for every gated round: the first plan plus one per allowed
    // revision. Each stays over the threshold so every round raises a card.
    let mut script = vec![text_result("multi")];
    for _ in 0..=MAX_SCOPE_REVISIONS {
        script.push(text_result(r#"["s1","s2","s3","s4","s5","s6"]"#));
    }
    let provider = ScriptedProvider::new(script);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![], "");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = ScriptedGate::new(answers);
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
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig::default(),
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run("do a large thing", &mut messages, &mut budget)
        .await;

    // The cap is on re-planning, so the reviewer still got a card for every
    // plan that was made — one first plan plus MAX_SCOPE_REVISIONS re-plans.
    assert_eq!(approvals.cards_raised().len(), MAX_SCOPE_REVISIONS + 1);
    let events = drain(&mut rx);
    assert!(
        !stages(&events).contains(&StageKind::Execute),
        "nothing executes — no scope was ever accepted"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Error { message, .. }
                if message.contains("approve, trim, or abort")
        )),
        "the refusal must name the keys that still work: {events:?}"
    );
    match outcome {
        Ok(o) => assert!(
            matches!(&o.status, PipelineStatus::Aborted { reason } if reason.contains("revision limit")),
            "the outcome must name the cap as the cause: {:?}",
            o.status
        ),
        Err(e) => panic!("a hit cap is an abort, not a pipeline error: {e:?}"),
    }
}

/// #1264: a plan far *below* every threshold — two steps, well under the
/// 5/8/$1.00 defaults — must still raise the card when plan mode is on.
/// Without this the flag is decorative: the estimator waves the plan through
/// before the gate is ever consulted, and the user who asked to see the plan
/// silently does not.
#[tokio::test]
async fn plan_mode_raises_the_card_for_a_plan_no_threshold_would_catch() {
    let provider = ScriptedProvider::new(vec![
        text_result("multi"),
        text_result(r#"["s1","s2"]"#),
        text_result("working"),
        text_result("done"),
    ]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![], "");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = ScriptedGate::new(vec![ScopeDecision::Approve]);
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
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig {
            plan_mode: true,
            ..PipelineConfig::default()
        },
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let _ = pipeline
        .run("small tidy-up", &mut messages, &mut budget)
        .await;

    assert_eq!(
        approvals.cards_raised().len(),
        1,
        "plan mode must consult the gate even for a two-step plan"
    );
    let _ = drain(&mut rx);
}

/// The control: the SAME two-step plan with plan mode off must sail straight
/// through untouched. Without this pair, a bug that raised the card for every
/// run would still pass the test above.
#[tokio::test]
async fn the_same_small_plan_is_not_gated_when_plan_mode_is_off() {
    let provider = ScriptedProvider::new(vec![
        text_result("multi"),
        text_result(r#"["s1","s2"]"#),
        text_result("working"),
        text_result("done"),
    ]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![], "");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = ScriptedGate::new(vec![ScopeDecision::Approve]);
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
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig::default(),
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let _ = pipeline
        .run("small tidy-up", &mut messages, &mut budget)
        .await;

    assert!(
        approvals.cards_raised().is_empty(),
        "a two-step plan is under every threshold and must not be gated"
    );
    let _ = drain(&mut rx);
}

/// Plan mode is an explicit request to be asked. Honouring the headless
/// bypass here would deliver the exact opposite of the ask, silently — worse
/// than refusing, because the run would proceed to touch the tree.
#[tokio::test]
async fn plan_mode_refuses_a_headless_run_rather_than_bypassing_the_gate() {
    let provider = ScriptedProvider::new(vec![
        text_result("multi"),
        text_result(r#"["s1","s2"]"#),
        text_result("working"),
        text_result("done"),
    ]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![], "");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = ScriptedGate::new(vec![ScopeDecision::Approve]);
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
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig {
            plan_mode: true,
            headless: true,
            // Even with the bypass explicitly ON, plan mode must not proceed.
            headless_bypass_scope_review: true,
            ..PipelineConfig::default()
        },
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let _ = pipeline
        .run("small tidy-up", &mut messages, &mut budget)
        .await;

    assert!(
        approvals.cards_raised().is_empty(),
        "there is nobody to ask in a headless run"
    );
    let events = drain(&mut rx);
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Error { message, .. } if message.contains("--plan")
        )),
        "the refusal must say why, naming the flag: {events:?}"
    );
}
