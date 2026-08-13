use super::*;
use stella_protocol::ToolCall;

fn checkpoint_fixture() -> Checkpoint {
    let mut budget = BudgetGuard::new(BudgetMode::Enforced, Some(2.5), Some(10.0));
    let _ = budget.record_spend(0.375);
    budget.reseed_session_spend(1.125);
    let messages = vec![
        CompletionMessage::system("you are stella"),
        CompletionMessage::user("read the file"),
        CompletionMessage {
            role: MessageRole::Assistant,
            content: "on it".into(),
            tool_calls: vec![ToolCall {
                call_id: "c1".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "a.rs"}),
            }],
            tool_results: Vec::new(),
            attachments: Vec::new(),
        },
        CompletionMessage {
            role: MessageRole::Tool,
            content: String::new(),
            tool_calls: Vec::new(),
            tool_results: vec![ToolResult {
                call_id: "c1".into(),
                output: ToolOutput::Ok {
                    content: "fn main() {}".into(),
                },
            }],
            attachments: Vec::new(),
        },
    ];
    let state = TurnState::new(messages, budget, &EngineConfig::default());
    let mut state = state;
    state.total_cost_usd = 0.375;
    state.calibration_model = Some("glm-5.2".into());
    // Two spent steers, not one: the count is a wire field as of #2809, and a
    // fixture carrying its serde default could not tell "round-tripped" from
    // "silently defaulted".
    state.loop_steer = LoopSteerBudget::resumed(
        Some(LoopIdentity {
            tools: vec!["bash".into()],
            inputs: Some(vec![r#"{"command":"cargo test"}"#.into()]),
        }),
        2,
    );
    state.step = 3;
    state.mark_transcript_rewritten();
    state.mark_transcript_rewritten();
    state.to_checkpoint()
}

#[test]
fn checkpoint_round_trips_byte_identically() {
    // serialize → deserialize → serialize must produce the same bytes.
    // The repo has no `preserve_order` on serde_json, so this is only true
    // because `Checkpoint` is a struct: a `json!`-built object would come
    // back key-sorted and fail here.
    let checkpoint = checkpoint_fixture();
    let first = checkpoint.to_json().expect("encode");
    let decoded = Checkpoint::from_json(&first).expect("decode");
    let second = decoded.to_json().expect("re-encode");
    assert_eq!(
        first, second,
        "checkpoint JSON must be stable across a round trip"
    );
    assert_eq!(checkpoint, decoded, "and the value must survive it");
}

#[test]
fn a_restored_turn_state_carries_the_whole_checkpoint() {
    let checkpoint = checkpoint_fixture();
    let state = TurnState::from_checkpoint(checkpoint.clone(), &EngineConfig::default());

    assert_eq!(state.step(), checkpoint.step);
    assert_eq!(state.messages(), checkpoint.messages.as_slice());
    assert!((state.total_cost_usd() - checkpoint.total_cost_usd).abs() < 1e-12);
    assert_eq!(state.calibration_model.as_deref(), Some("glm-5.2"));
    assert_eq!(
        state.loop_steer.warned(),
        Some(&LoopIdentity {
            tools: vec!["bash".to_string()],
            inputs: Some(vec![r#"{"command":"cargo test"}"#.to_string()]),
        }),
        "the spent loop steer must not be re-earned, and the warned loop \
         must survive the resume WITH its arguments — tools alone cannot \
         tell one `bash` loop from another"
    );
    assert_eq!(state.transcript_rewrites, checkpoint.transcript_rewrites);
    // And re-snapshotting the restored turn reproduces the checkpoint, so
    // resume → checkpoint → resume is a fixed point.
    assert_eq!(state.to_checkpoint(), checkpoint);
}

/// #2809's witness, end to end through the wire shape a host actually stores:
/// a checkpoint that recorded a fully spent steering budget resumes with it
/// spent, so the next detection of ANY loop ends the turn.
///
/// Fails before #2809, where the count was not on the wire at all: the
/// resumed turn reconciled the legacy `loop_steered` bool as one spent steer
/// and could buy a second warning it had already paid for.
#[test]
fn a_resumed_turn_cannot_re_open_a_steer_the_checkpoint_says_it_spent() {
    let json = checkpoint_fixture().to_json().expect("encode");
    let decoded = Checkpoint::from_json(&json).expect("decode");
    assert_eq!(
        decoded.loop_steers_spent, 2,
        "the count must survive the JSON, not just the struct"
    );

    let state = TurnState::from_checkpoint(decoded, &EngineConfig::default());
    assert_eq!(
        state.loop_steer.remaining(),
        0,
        "a spent budget must resume spent — this is the whole of #2809"
    );

    // And a checkpoint written before the field decodes it as 0, which must
    // still restore one spent steer rather than refunding the warning.
    let mut legacy: serde_json::Value = serde_json::from_str(&json).expect("parse");
    legacy
        .as_object_mut()
        .expect("object")
        .remove("loop_steers_spent");
    let legacy = Checkpoint::from_json(&legacy.to_string()).expect("a v1 checkpoint still decodes");
    assert_eq!(legacy.loop_steers_spent, 0);
    let restored = TurnState::from_checkpoint(legacy, &EngineConfig::default());
    assert_eq!(
        restored.loop_steer.spent(),
        1,
        "the legacy flag is what keeps the serde default from reading as a \
         refund"
    );
}

#[test]
fn a_restored_budget_meters_from_exactly_where_it_stopped() {
    let checkpoint = checkpoint_fixture();
    let restored = checkpoint.budget.restore();
    assert_eq!(restored.mode(), BudgetMode::Enforced);
    assert_eq!(restored.turn_limit_usd(), Some(2.5));
    assert_eq!(restored.session_limit_usd(), Some(10.0));
    assert!((restored.spent_usd() - 0.375).abs() < 1e-12, "turn axis");
    assert!(
        (restored.session_spent_usd() - 1.125).abs() < 1e-12,
        "session axis"
    );
}

#[test]
fn a_checkpoint_from_another_build_is_refused_not_half_understood() {
    let mut checkpoint = checkpoint_fixture();
    checkpoint.version = CHECKPOINT_VERSION + 7;
    let json = checkpoint.to_json().expect("encode");
    match Checkpoint::from_json(&json) {
        Err(CheckpointError::Version { found, expected }) => {
            assert_eq!(found, CHECKPOINT_VERSION + 7);
            assert_eq!(expected, CHECKPOINT_VERSION);
        }
        other => panic!("expected a version refusal, got {other:?}"),
    }
    assert!(matches!(
        Checkpoint::from_json("{ not json"),
        Err(CheckpointError::Decode(_))
    ));
}

#[test]
fn a_cancel_closes_every_open_tool_use_so_the_history_stays_reusable() {
    // The shape a provider rejects outright: an assistant `tool_use` with
    // no answering `tool_result`.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let events = EventSender::new(tx);
    let mut state = TurnState::new(
        vec![
            CompletionMessage::user("go"),
            CompletionMessage {
                role: MessageRole::Assistant,
                content: String::new(),
                tool_calls: vec![
                    ToolCall {
                        call_id: "c1".into(),
                        name: "bash".into(),
                        input: serde_json::json!({}),
                    },
                    ToolCall {
                        call_id: "c2".into(),
                        name: "bash".into(),
                        input: serde_json::json!({}),
                    },
                ],
                tool_results: Vec::new(),
                attachments: Vec::new(),
            },
        ],
        BudgetGuard::new(BudgetMode::Off, None, None),
        &EngineConfig::default(),
    );
    assert!(
        state.cancel_outcome(&events).is_none(),
        "an un-cancelled token must not touch the transcript"
    );

    state.cancel_token().cancel();
    let outcome = state.cancel_outcome(&events).expect("cancelled");
    assert!(matches!(
        outcome,
        StepOutcome::Aborted { ref reason, .. } if reason == CANCELLED_REASON
    ));

    let closing = state.messages().last().expect("a closing Tool message");
    assert_eq!(closing.role, MessageRole::Tool);
    let ids: Vec<&str> = closing
        .tool_results
        .iter()
        .map(|r| r.call_id.as_str())
        .collect();
    assert_eq!(ids, ["c1", "c2"], "every open call is closed, in order");
    assert!(closing.tool_results.iter().all(
        |r| matches!(&r.output, ToolOutput::Error { message, .. } if message == CANCELLED_TOOL_RESULT)
    ));

    // Mirrored onto the event stream, so an events-only reconstruction of
    // the transcript resolves the same calls.
    let mut seen = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let AgentEvent::ToolResult { call_id, .. } = event {
            seen.push(call_id);
        }
    }
    assert_eq!(seen, ["c1", "c2"]);
}

#[test]
fn closing_open_calls_is_a_no_op_on_a_well_paired_transcript() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let events = EventSender::new(tx);
    let checkpoint = checkpoint_fixture();
    let mut state = TurnState::from_checkpoint(checkpoint, &EngineConfig::default());
    let before = state.messages().len();
    state.cancel_token().cancel();
    let _ = state.cancel_outcome(&events);
    assert_eq!(
        state.messages().len(),
        before,
        "nothing to close means nothing appended"
    );
}

#[test]
fn a_cloned_token_cancels_the_turn_it_came_from() {
    let state = TurnState::new(
        Vec::new(),
        BudgetGuard::new(BudgetMode::Off, None, None),
        &EngineConfig::default(),
    );
    let handed_off = state.cancel_token();
    assert!(!state.cancel.is_cancelled());
    handed_off.cancel();
    assert!(state.cancel.is_cancelled(), "a clone shares the flag");
}

/// **Witness (#1841).** The compaction budget stops moving between steps
/// once a corrected value lands.
///
/// The drift factor is re-recorded on every committed step, so the budget
/// was recomputed — and moved — on every one. A conversation parked just
/// under compaction's hysteresis low-watermark could be pushed back over
/// it by a factor update alone, re-triggering the oldest-first passes and
/// rewriting the earliest tool result: the worst position to mutate for
/// the prompt cache, and exactly what that hysteresis exists to prevent.
///
/// The fresh values here move the way a warmed calibration's do.
#[test]
fn the_compaction_budget_is_latched_once_the_calibration_corrects() {
    let mut state = TurnState::new(
        Vec::new(),
        BudgetGuard::new(BudgetMode::Observed, None, None),
        &EngineConfig::default(),
    );
    state.calibration_model = Some("anthropic/claude-fable-5".to_string());

    let first = state.latch_effective_budget((120_000, 1.25));
    assert_eq!(first, (120_000, 1.25));

    for moving in [(98_000, 1.53), (75_000, 2.0), (150_000, 1.0)] {
        assert_eq!(
            state.latch_effective_budget(moving),
            first,
            "a factor update alone must not move the budget mid-turn"
        );
    }
}

/// **Witness (#2133).** A warming calibration's identity answer is served
/// live and never captured — the first CORRECTED value is what settles.
///
/// A fresh session's first latched read lands one sample into the
/// three-sample warm-up, so the latch captured `(budget, 1.0)` and served
/// it for the rest of the turn: 40+ steps of a bench trial reported
/// `calibration_factor: 1.0` while the map underneath had long since
/// converged on 2×+ observed drift, and every compaction decision ran on
/// a fraction of the real context.
#[test]
fn a_warming_calibrations_identity_is_served_live_never_captured() {
    let mut state = TurnState::new(
        Vec::new(),
        BudgetGuard::new(BudgetMode::Observed, None, None),
        &EngineConfig::default(),
    );
    state.calibration_model = Some("claude-fable-5".to_string());

    // Below the sample floor the estimator answers the identity.
    assert_eq!(state.latch_effective_budget((150_000, 1.0)), (150_000, 1.0));
    assert_eq!(state.latch_effective_budget((150_000, 1.0)), (150_000, 1.0));
    assert!(
        state.effective_budget.is_none(),
        "the identity must not latch"
    );

    // Warm-up crossed: the first corrected value reaches the decision…
    assert_eq!(
        state.latch_effective_budget((100_000, 1.5)),
        (100_000, 1.5),
        "the first corrected value must be adopted, not the frozen identity"
    );
    // …and THAT is what settles: later drift no longer moves the budget.
    assert_eq!(state.latch_effective_budget((75_000, 2.0)), (100_000, 1.5));
}

/// …and NOT before, because a value latched then is keyed on nothing.
///
/// `calibration_model` is unset until the first result lands.
/// `CalibrationMap::effective_budget(None, …)` resolves through the
/// single-entry fallback on a one-model session but returns the
/// UNCORRECTED budget on a multi-model one — so latching at step 0 would
/// silently disable drift correction for exactly the sessions that route
/// triage, verifier and worker to different models, while leaving every
/// single-model test green.
#[test]
fn nothing_is_latched_while_the_model_is_still_unknown() {
    let mut state = TurnState::new(
        Vec::new(),
        BudgetGuard::new(BudgetMode::Observed, None, None),
        &EngineConfig::default(),
    );
    assert!(state.calibration_model.is_none());

    assert_eq!(state.latch_effective_budget((150_000, 1.0)), (150_000, 1.0));
    assert_eq!(
        state.latch_effective_budget((120_000, 1.25)),
        (120_000, 1.25),
        "step 0 tracks the live value — there is no model to latch against"
    );
    assert!(state.effective_budget.is_none(), "and nothing was captured");

    // The moment the first result lands, the next CORRECTED value sticks.
    state.calibration_model = Some("zai/glm-5.2".to_string());
    let latched = state.latch_effective_budget((118_000, 1.27));
    assert_eq!(state.latch_effective_budget((75_000, 2.0)), latched);
}

/// The usage anchor's turn-state lifecycle (#2681): a reported envelope
/// anchors, an empty one keeps the previous anchor (the transcript has only
/// been appended to, so the report still describes a live prefix), and any
/// in-place rewrite drops it — the report names bytes that no longer exist.
#[test]
fn the_usage_anchor_survives_appends_and_dies_with_a_rewrite() {
    let mut state = TurnState::new(
        vec![CompletionMessage::system("sys")],
        BudgetGuard::new(BudgetMode::Observed, None, None),
        &EngineConfig::default(),
    );
    let latched = (150_000, 1.0);
    assert_eq!(
        state.anchored_budget(latched, 150_000),
        latched,
        "no report yet — the latched pair passes through untouched"
    );

    let mut usage = CompletionUsage::reported_zero();
    usage.input_tokens = 200_000; // over the whole configured budget
    state.anchor_usage(&usage, 1_000);
    let (anchored, factor) = state.anchored_budget(latched, 150_000);
    assert!(
        anchored < latched.0,
        "the report exceeds the budget, so the handed budget must tighten"
    );
    assert_eq!(factor, latched.1, "the factor rides along for the manifest");

    state.anchor_usage(&CompletionUsage::reported_zero(), 1_000);
    assert_eq!(
        state.anchored_budget(latched, 150_000),
        (anchored, factor),
        "a signal-less envelope keeps the previous anchor"
    );

    state.mark_transcript_rewritten();
    assert_eq!(
        state.anchored_budget(latched, 150_000),
        latched,
        "a rewrite invalidates the report's prefix — back to the estimator"
    );
}

fn stub_completion_result() -> CompletionResult {
    CompletionResult {
        text: "ok".into(),
        tool_calls: Vec::new(),
        usage: CompletionUsage::reported_zero(),
        model: "test".into(),
        cost_usd: 0.0,
        finish_reason: None,
        upstream_provider: None,
    }
}

/// #2021's witness: a call with no idle timeout at all — the shape a
/// well-behaved streaming provider has, since it is always producing — is
/// still cut off once the task's own remaining wall clock runs out. Before
/// `deadline_bounded_generation` nothing in this module could express that:
/// `bounded_generation`'s idle bound never fires on a call with `idle_limit:
/// None`, no matter how long it runs.
#[tokio::test]
async fn a_call_with_no_idle_bound_is_still_cut_by_the_task_deadline() {
    let progress = StreamProgress::default();
    let result = deadline_bounded_generation(
        None,
        Some(std::time::Instant::now() + Duration::from_millis(30)),
        &progress,
        std::future::pending(),
    )
    .await;
    match result {
        Err(ProviderError::Terminal(message)) => {
            assert!(
                message.contains("deadline") || message.contains("wall clock"),
                "the trip should name the task deadline, got {message:?}"
            );
        }
        other => panic!("a call must not outlive the task deadline, got {other:?}"),
    }
}

/// The fix-git shape itself: a call that keeps streaming fragments fast
/// enough to reset a generous idle bound on every check — so the idle bound
/// alone would wait the full 10s — is still cut at the task's much shorter
/// remaining deadline. This is the failure #2021 measured: a provider
/// trickling output, not a silent one, outrunning the deadline while the
/// idle clock never once saw a gap worth tripping on.
#[tokio::test]
async fn a_trickling_generation_under_a_generous_idle_bound_is_still_cut_by_the_task_deadline() {
    let progress = StreamProgress::default();
    let trickle = async {
        loop {
            progress.record();
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        #[allow(unreachable_code)]
        Ok(stub_completion_result())
    };
    let result = deadline_bounded_generation(
        Some(Duration::from_secs(10)),
        Some(std::time::Instant::now() + Duration::from_millis(30)),
        &progress,
        trickle,
    )
    .await;
    match result {
        Err(ProviderError::Terminal(message)) => {
            assert!(
                message.contains("deadline") || message.contains("wall clock"),
                "the trip should name the task deadline, not the idle bound, got {message:?}"
            );
        }
        other => panic!("a trickling call must still be cut by the task deadline, got {other:?}"),
    }
}

/// The other side: with no task deadline armed (`deadline_remaining: None`)
/// — an interactive CLI session with no bench harness — behavior is
/// unchanged from `bounded_generation` alone. This pins the claim in
/// `deadline_bounded_generation`'s doc comment that a call with no deadline
/// armed is never touched by this bound.
#[tokio::test]
async fn no_armed_deadline_leaves_the_idle_bound_as_the_only_cut() {
    let progress = StreamProgress::default();
    let result = deadline_bounded_generation(
        Some(Duration::from_millis(20)),
        None,
        &progress,
        std::future::pending(),
    )
    .await;
    match result {
        Err(ProviderError::Terminal(message)) => {
            assert!(
                message.contains("stalled") || message.contains("idle"),
                "with no deadline armed the trip must be the idle bound's own message, \
                 got {message:?}"
            );
        }
        other => {
            panic!("an unarmed deadline must still leave the idle bound active, got {other:?}")
        }
    }
}
