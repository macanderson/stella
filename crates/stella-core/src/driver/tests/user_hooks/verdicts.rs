//! #3380 witnesses for the `Stop` gate: a denial that carries structured
//! evidence, and a hold allowance the host — not the engine — chooses.
//!
//! A sibling of `user_hooks.rs` rather than more lines inside it: that file
//! was 43 lines under the 1500-line ratchet when these arrived, and the rule
//! is that a file approaching the limit gets split rather than grown up to it.

use super::*;

/// **Witness for A6 (structured verdicts).** A `Stop` hook that denies with
/// evidence — witness, argv, tri-state flip, digest — reaches BOTH consumers
/// with those fields intact: the tail message the model reads renders each
/// one, and the `hook.stop.blocked` lifecycle payload carries the evidence
/// object structurally so a fold can branch without parsing prose.
///
/// Fails before #3380: `HookDecision::Deny` carried only a `String`, so serde
/// dropped the whole `evidence` object as an unknown key and neither consumer
/// could have seen a witness, a command, a flip or a digest.
#[tokio::test]
async fn a_structured_stop_denial_reaches_the_model_and_the_journal_intact() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(text_result("first answer")),
            Ok(text_result("second answer")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let runner = ScriptedHookRunner {
        stdouts: TokioMutex::new(vec![
            r#"{"action":"deny","reason":"the witness is still red",
                "evidence":{"witness":"tests/flip.rs",
                            "command":["cargo","test","-p","stella-core","--","flip"],
                            "flip":"not_achieved","digest":"sha256:c0ffee"}}"#
                .into(),
            r#"{"action":"allow"}"#.into(),
        ]),
        payloads: Arc::new(TokioMutex::new(Vec::new())),
    };
    let hooks: Hooks =
        serde_json::from_str(r#"{ "Stop": [ { "hooks": [{ "command": "verify" }] } ] }"#).unwrap();

    let bus = crate::bus::HookBus::new("denial-test");
    let blocked: Arc<std::sync::Mutex<Vec<serde_json::Value>>> = Arc::default();
    let sink = Arc::clone(&blocked);
    bus.on(crate::bus::names::HOOK_STOP_BLOCKED, move |event| {
        sink.lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(event.payload.clone());
        Ok(())
    })
    .detach();

    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper)
        .with_hooks(&hooks, &runner)
        .with_bus(&bus);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));

    // Consumer 1: the model's next observation renders every field.
    let feedback = messages
        .iter()
        .find(|m| {
            m.role == MessageRole::User
                && m.content
                    .starts_with(crate::driver::user_hooks::STOP_HOOK_MARKER_PREFIX)
        })
        .expect("the denial held the turn open with a marked tail message");
    for fragment in [
        "the witness is still red",
        "- witness: tests/flip.rs",
        r#"- command: ["cargo","test","-p","stella-core","--","flip"]"#,
        "- flip: not_achieved",
        "- digest: sha256:c0ffee",
    ] {
        assert!(
            feedback.content.contains(fragment),
            "the model must see `{fragment}` in: {}",
            feedback.content
        );
    }

    // Consumer 2: the journal carries the evidence as data, not as prose.
    let blocked = blocked.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(blocked.len(), 1, "one held-open round: {blocked:?}");
    let evidence = &blocked[0]["evidence"];
    assert_eq!(evidence["witness"], "tests/flip.rs");
    assert_eq!(evidence["flip"], "not_achieved");
    assert_eq!(evidence["digest"], "sha256:c0ffee");
    assert_eq!(
        evidence["command"],
        serde_json::json!(["cargo", "test", "-p", "stella-core", "--", "flip"])
    );
}

/// A prose-only denial — every pre-#3380 hook — renders exactly as it always
/// did, and its journal payload states that this hook does not verify rather
/// than inventing an empty evidence object.
#[tokio::test]
async fn a_prose_only_stop_denial_grows_no_evidence_section() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(text_result("first answer")),
            Ok(text_result("second answer")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let runner = ScriptedHookRunner {
        stdouts: TokioMutex::new(vec![
            r#"{"action":"deny","reason":"the checklist is not done"}"#.into(),
            r#"{"action":"allow"}"#.into(),
        ]),
        payloads: Arc::new(TokioMutex::new(Vec::new())),
    };
    let hooks: Hooks =
        serde_json::from_str(r#"{ "Stop": [ { "hooks": [{ "command": "verify" }] } ] }"#).unwrap();
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper)
        .with_hooks(&hooks, &runner);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    engine.run_turn(&mut messages, &mut budget, &tx).await;

    let feedback = messages
        .iter()
        .find(|m| {
            m.role == MessageRole::User
                && m.content
                    .starts_with(crate::driver::user_hooks::STOP_HOOK_MARKER_PREFIX)
        })
        .expect("the denial held the turn open");
    assert!(feedback.content.contains("the checklist is not done"));
    assert!(
        !feedback.content.contains("verification evidence"),
        "a hook that measured nothing must not be rendered as if it had: {}",
        feedback.content
    );
}

/// **Witness for A7 (`max_holds` becomes real), half one.** A host-supplied
/// allowance ABOVE the old hardcoded three actually buys those rounds: five
/// held-open consultations, then the sixth completion stands.
///
/// Fails before #3380: nothing read a bound, so the turn completed on the
/// fourth answer whatever the host asked for.
#[tokio::test]
async fn a_host_supplied_hold_allowance_buys_more_rounds_than_the_default() {
    let held = run_always_denying_stop_gate(Some(5), 6).await;
    assert_eq!(
        held, 5,
        "an allowance of five must buy five held-open rounds"
    );
}

/// **Witness for A7, half two.** The declared number is an ASK, never an
/// authority: an allowance far above the host ceiling is clamped to
/// [`STOP_HOLD_CEILING`](crate::driver::STOP_HOLD_CEILING) rather than
/// honoured, so no manifest can buy an unbounded deny → revise → re-check
/// loop.
#[tokio::test]
async fn a_hold_allowance_above_the_ceiling_is_clamped_not_honoured() {
    let ceiling = crate::driver::STOP_HOLD_CEILING;
    let held = run_always_denying_stop_gate(Some(1_000), ceiling as usize + 1).await;
    assert_eq!(
        held, ceiling as usize,
        "a thousand-round ask buys the ceiling and no more"
    );
}

/// Run one turn against an always-denying `Stop` hook and report how many
/// held-open rounds it bought. `answers` scripts that many model replies —
/// one per held round plus the completion that finally stands.
async fn run_always_denying_stop_gate(allowance: Option<u32>, answers: usize) -> usize {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(
            (0..answers)
                .map(|i| Ok(text_result(&format!("answer {i}"))))
                .collect(),
        ),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let runner = RecordingHookRunner {
        exit_code: 0,
        stdout: r#"{"action":"deny","reason":"not yet"}"#.into(),
        stderr: String::new(),
        payloads: Arc::new(TokioMutex::new(Vec::new())),
    };
    let hooks: Hooks =
        serde_json::from_str(r#"{ "Stop": [ { "hooks": [{ "command": "verify" }] } ] }"#).unwrap();
    let config = EngineConfig {
        stop_hold_allowance: allowance,
        ..EngineConfig::default()
    };
    let engine =
        Engine::with_sleeper(&provider, &tools, config, &sleeper).with_hooks(&hooks, &runner);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "the allowance must be spent and the turn must stand, not abort: {outcome:?}"
    );
    let feedback: Vec<&CompletionMessage> = messages
        .iter()
        .filter(|m| {
            m.role == MessageRole::User
                && m.content
                    .starts_with(crate::driver::user_hooks::STOP_HOOK_MARKER_PREFIX)
        })
        .collect();
    // The last permitted round still announces itself, whatever the bound:
    // a spent allowance is reported, never silently dropped.
    if let Some(last) = feedback.last() {
        assert!(
            last.content
                .contains(crate::driver::user_hooks::STOP_FINAL_ROUND_NOTE.trim_start()),
            "the final round must say it is final: {}",
            last.content
        );
    }
    feedback.len()
}
