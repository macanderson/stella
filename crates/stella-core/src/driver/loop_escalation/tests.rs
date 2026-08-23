use super::{
    LoopEscalation, LoopIdentity, LoopSteerBudget, LoopVerdict, MAX_LOOP_STEERS, abort_reason,
    polling_tool, steer_text,
};
use crate::ports::ToolExecutor;
use proptest::prelude::*;
use stella_protocol::tool::{ToolOutput, ToolSchema};

fn tools(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| n.to_string()).collect()
}

/// A registry with one status-shaped (read-only) tool and one mutating
/// tool — the classification input, nothing more.
struct StatusAndBash;

#[async_trait::async_trait]
impl ToolExecutor for StatusAndBash {
    fn schemas(&self) -> Vec<ToolSchema> {
        let schema = |name: &str, read_only: bool| ToolSchema {
            name: name.into(),
            description: String::new(),
            input_schema: serde_json::json!({}),
            read_only,
            speculation_safe: read_only,
        };
        vec![schema("ci_status", true), schema("bash", false)]
    }
    async fn execute(&self, _name: &str, _input: &serde_json::Value) -> ToolOutput {
        unreachable!("steer classification never executes a tool")
    }
}

fn exact_repeat(tool: &str) -> LoopVerdict {
    LoopVerdict::ExactRepeat {
        tool: tool.to_string(),
        input: serde_json::json!({"wait": true}),
        count: 3,
    }
}

/// #1473's witness: a 3-repeat of a read-only status tool with
/// byte-identical output steers to ONE blocking wait — where the
/// generic text prescribed "vary the arguments", the exact move that
/// made the motivating session strictly worse (a 600s watch timeout
/// that voided the prompt cache, then blind sleeps).
#[test]
fn a_polling_loop_is_steered_to_one_blocking_wait_not_variation() {
    let verdict = exact_repeat("ci_status");
    let tool = polling_tool(&verdict, &StatusAndBash);
    assert_eq!(tool.as_deref(), Some("ci_status"));

    let text = steer_text("ci_status repeated 3×", tool.as_deref(), None, 1);
    assert!(text.contains("ONE blocking wait"), "{text}");
    assert!(
        !text.contains("vary the arguments, try a different tool"),
        "the generic prescription is precisely wrong for a poll: {text}"
    );
    assert!(
        text.contains("has not changed"),
        "the steer must say WHY re-checking is futile: {text}"
    );
}

/// A supplied duration prior is cited with a concrete number (#1472's
/// data, once a caller carries it).
#[test]
fn a_duration_prior_sizes_the_wait_concretely() {
    let text = steer_text("ci_status repeated 3×", Some("ci_status"), Some(720), 1);
    assert!(text.contains("~720s"), "{text}");
}

/// A mutating tool keeps the generic steer: repeating an ACTION is not
/// polling, and "vary the arguments" is real advice there.
#[test]
fn a_mutating_repeat_keeps_the_generic_steer() {
    let verdict = exact_repeat("bash");
    assert_eq!(polling_tool(&verdict, &StatusAndBash), None);
    let text = steer_text("bash repeated 3×", None, None, 1);
    assert!(text.contains("vary the arguments"), "{text}");
}

/// #2810's witness, non-final half: a warning with budget left behind it
/// keeps the legacy conditional consequence and must NOT claim finality.
#[test]
fn a_non_final_steer_keeps_the_conditional_consequence() {
    for tool in [None, Some("ci_status")] {
        let text = steer_text("ci_status repeated 3×", tool, None, 1);
        assert!(
            text.contains("If you keep"),
            "a warning with budget behind it states the conditional: {text}"
        );
        assert!(
            !text.to_lowercase().contains("last warning"),
            "and must not claim to be the last one: {text}"
        );
    }
}

/// #2810's witness, final half: the warning that spends the last of
/// [`MAX_LOOP_STEERS`] says so, and says the next loop of ANY kind ends
/// the turn — which is what `escalate` actually does once the budget is
/// gone, and what the shared conditional clause could not express.
#[test]
fn the_last_steer_says_it_is_the_last_one() {
    for tool in [None, Some("ci_status")] {
        let text = steer_text("ci_status repeated 3×", tool, None, 0);
        assert!(
            text.contains("LAST warning"),
            "the final warning must announce itself: {text}"
        );
        assert!(
            text.contains("any other"),
            "and must say a DIFFERENT loop also ends the turn, which is \
             the half the non-final wording gets right and this one \
             previously did not: {text}"
        );
        assert!(
            !text.contains("If you keep"),
            "the conditional it replaces must be gone, not appended to: {text}"
        );
    }
}

/// A cycle involves several tools; no single wait replaces it.
#[test]
fn a_cycle_keeps_the_generic_steer_even_over_read_only_tools() {
    let verdict = LoopVerdict::ShortCycle {
        pattern: Vec::new(),
        repeats: 3,
    };
    assert_eq!(polling_tool(&verdict, &StatusAndBash), None);
}

/// An exact-repeat loop: one tool, one set of arguments.
fn repeat(tool: &str, input: &str) -> LoopIdentity {
    LoopIdentity {
        tools: tools(&[tool]),
        inputs: Some(vec![input.to_string()]),
    }
}

/// A `bash` loop on `command`, the shape of nearly every real one.
fn bash(command: &str) -> LoopIdentity {
    repeat("bash", &format!("{{\"command\":\"{command}\"}}"))
}

#[test]
fn re_detecting_the_warned_pattern_claims_persistence() {
    let reason = abort_reason(&bash("cargo test"), &bash("cargo test"), "bash repeated 4×");
    assert_eq!(
        reason, "stuck-loop detected (persisted after a steering warning): bash repeated 4×",
        "the byte-exact legacy phrasing must survive for the one case it was true of"
    );
}

/// #1524 witness: on `prove-plus-comm` the model was warned about a
/// `write_file` loop, stopped repeating `write_file`, and was then
/// aborted over a fresh `edit_file` loop by a message claiming the
/// warned loop had "persisted". The abort stays; the false claim goes.
#[test]
fn a_different_pattern_does_not_claim_the_warned_loop_persisted() {
    let reason = abort_reason(
        &repeat("write_file", r#"{"path":"a.rs"}"#),
        &repeat("edit_file", r#"{"path":"a.rs"}"#),
        "edit_file repeated 3×",
    );
    assert!(
        !reason.contains("persisted"),
        "a fresh loop must not be blamed on the warned one: {reason}"
    );
    assert!(
        reason.contains("write_file"),
        "the abort should name what the warning was actually about: {reason}"
    );
    assert!(
        reason.contains("edit_file repeated 3×"),
        "the detector's evidence for the loop that DID abort stays: {reason}"
    );
}

#[test]
fn a_cycle_pattern_only_persists_if_the_whole_cycle_matches() {
    let cycle = LoopIdentity {
        tools: tools(&["read_file", "bash"]),
        inputs: Some(vec![
            r#"{"path":"a.rs"}"#.to_string(),
            r#"{"command":"cargo test"}"#.to_string(),
        ]),
    };
    let reason = abort_reason(&cycle, &bash("cargo test"), "bash repeated 3×");
    assert!(!reason.contains("persisted"), "{reason}");
}

#[test]
fn an_unknown_warned_pattern_makes_no_claim_either_way() {
    let unknown = LoopIdentity {
        tools: Vec::new(),
        inputs: None,
    };
    let reason = abort_reason(&unknown, &bash("cargo test"), "bash repeated 3×");
    assert!(
        !reason.contains("persisted") && !reason.contains("new loop"),
        "a checkpoint-resumed turn with no recorded pattern cannot support \
         either claim: {reason}"
    );
    assert!(reason.contains("steering warning"), "{reason}");
}

/// A turn resumed from a checkpoint that recorded the warned *tools* but
/// not the arguments (#1524's format) knows the tool and not the loop.
/// Tool-name equality is exactly the inference this change removed, so
/// it must not sneak back in through the resume path.
#[test]
fn a_resumed_turn_that_recorded_only_tools_still_makes_no_claim() {
    let partial = LoopIdentity {
        tools: tools(&["bash"]),
        inputs: None,
    };
    let reason = abort_reason(&partial, &bash("cargo test"), "bash repeated 3×");
    assert!(
        !reason.contains("persisted") && !reason.contains("new loop"),
        "knowing only the tool is not knowing the loop: {reason}"
    );
}

/// The witness, taken verbatim from the run that exposed this
/// (`ses-1785988025124`, worker `moonshotai/kimi-k3`): warned about one
/// `grep`, the model changed the command as instructed, looped on a
/// different `grep` over a different file — and the abort told the user
/// the warned loop had "persisted". Both loops are `bash`, so tool-name
/// identity called them one loop and #1524's fix could not fire.
#[test]
fn two_different_bash_loops_are_not_one_loop() {
    let warned = bash(r#"grep -n \"fn open\\b\" -B 8 crates/stella-graph/src/store.rs"#);
    let detected = bash(r#"grep -n \"fn index_all\" -B 4 crates/stella-graph/src/graph.rs"#);
    assert_eq!(
        warned.same_loop_as(&detected),
        Some(false),
        "same tool, different arguments — the detector keyed on the \
         arguments, so identity must too"
    );

    let reason = abort_reason(
        &warned,
        &detected,
        "the same `bash` call … repeated 3 times",
    );
    assert!(
        !reason.contains("persisted"),
        "the model obeyed the steer and broke the warned loop; the abort \
         must not say otherwise: {reason}"
    );
    assert!(
        reason.contains("fn open"),
        "and it should name the loop the warning was actually about, or \
         a reader cannot tell the two `bash` loops apart: {reason}"
    );
}

/// The other half: arguments are what identity is keyed on, so the same
/// `bash` command re-detected still reports persistence. Without this,
/// "compare the arguments too" could be satisfied by never claiming
/// persistence at all.
#[test]
fn the_same_bash_command_twice_still_persists() {
    let loop_ = bash("cargo test --workspace");
    assert_eq!(loop_.same_loop_as(&loop_), Some(true));
    assert!(
        abort_reason(&loop_, &loop_, "bash repeated 3×").contains("persisted"),
        "the byte-exact legacy phrasing must survive for the case it is true of"
    );
}

/// Stagnation's loop spans *differing* arguments by definition, so its
/// identity is the tool alone and two stagnation verdicts on one tool
/// are the same loop. Recorded as an empty input list, which must not be
/// confused with the `None` of an unrecorded one.
#[test]
fn stagnation_identity_is_the_tool_alone() {
    let stagnant = |tool: &str| LoopIdentity {
        tools: tools(&[tool]),
        inputs: Some(Vec::new()),
    };
    assert_eq!(stagnant("bash").same_loop_as(&stagnant("bash")), Some(true));
    assert_eq!(
        stagnant("bash").same_loop_as(&stagnant("read_file")),
        Some(false)
    );
    assert_eq!(
        stagnant("bash").same_loop_as(&bash("cargo test")),
        Some(false),
        "an empty input list is a real identity, not an absent one"
    );
}

// ── The steering budget (#1743) ──────────────────────────────────────

/// Whether this escalation is the turn's end, without naming the loop.
fn aborted(escalation: &LoopEscalation) -> bool {
    matches!(escalation, LoopEscalation::Abort { .. })
}

#[test]
fn the_first_detection_of_a_turn_is_always_a_steer() {
    let mut budget = LoopSteerBudget::default();
    let first = bash("cargo test");
    assert!(!aborted(&budget.escalate(&first)));
    assert_eq!(
        budget.warned(),
        Some(&first),
        "and it records what it warned"
    );
}

/// #1743's unit witness. The model was told about loop A, obeyed, and
/// formed loop B — so it has never been told about B, and killing the
/// turn here is both a resolve-rate loss and unfair to a compliant
/// model.
#[test]
fn a_provably_different_second_loop_earns_its_own_steer() {
    let mut budget = LoopSteerBudget::default();
    let first = bash(r#"grep -n \"fn open\" crates/stella-graph/src/store.rs"#);
    let second = bash(r#"grep -n \"fn index_all\" crates/stella-graph/src/graph.rs"#);
    assert!(!aborted(&budget.escalate(&first)));
    assert!(
        !aborted(&budget.escalate(&second)),
        "same tool, different arguments — a loop the model was never warned about"
    );
    assert_eq!(
        budget.warned(),
        Some(&second),
        "the warning the abort will be phrased against is the most recent one"
    );
}

/// The half that keeps the ladder a ladder: obeying does not reset it.
/// Re-detecting the loop the model WAS told about ends the turn on the
/// first re-detection, budget remaining or not.
#[test]
fn the_warned_loop_re_detected_aborts_on_sight() {
    let mut budget = LoopSteerBudget::default();
    let warned = bash("cargo test");
    assert!(!aborted(&budget.escalate(&warned)));
    assert_eq!(
        budget.escalate(&warned),
        LoopEscalation::Abort {
            warned: warned.clone()
        },
        "one steer spent of two, and it still dies: the model was told"
    );
}

/// The cap. A model that keeps forming genuinely new loops would earn a
/// warning every time without one, which is `max_steps` of waste — the
/// outcome loop detection exists to prevent.
#[test]
fn a_third_distinct_loop_is_not_steered() {
    let mut budget = LoopSteerBudget::default();
    let (a, b, c) = (bash("one"), bash("two"), bash("three"));
    assert!(!aborted(&budget.escalate(&a)));
    assert!(!aborted(&budget.escalate(&b)));
    assert_eq!(
        budget.escalate(&c),
        LoopEscalation::Abort { warned: b },
        "the budget is spent, and the abort is phrased against the loop \
         the last warning actually named"
    );
    assert_eq!(
        MAX_LOOP_STEERS, 2,
        "the ladder above assumes the cap it documents"
    );
}

/// A rotated cycle is the same cycle one call later, not a new loop —
/// otherwise a period-3 grind would earn a fresh warning every step.
#[test]
fn a_rotated_cycle_buys_no_second_steer() {
    let cycle = |shift: usize| {
        let names = ["read_file", "edit_file", "bash"];
        let args = [r#"{"path":"a.rs"}"#, r#"{"old":"x"}"#, r#"{"cmd":"test"}"#];
        LoopIdentity {
            tools: (0..3).map(|i| names[(i + shift) % 3].to_string()).collect(),
            inputs: Some((0..3).map(|i| args[(i + shift) % 3].to_string()).collect()),
        }
    };
    let mut budget = LoopSteerBudget::default();
    assert!(!aborted(&budget.escalate(&cycle(0))));
    assert!(
        aborted(&budget.escalate(&cycle(1))),
        "the same read → edit → test grind, reported from a window that \
         moved on by one call"
    );
}

/// "I cannot tell" is not proof of a different loop. A turn resumed from
/// a checkpoint written before identities carried arguments knows it was
/// warned and not about what, so it spends the budget rather than
/// risking a second warning for the loop it was already told about.
#[test]
fn an_unidentifiable_loop_never_earns_the_second_steer() {
    let unrecorded = LoopIdentity {
        tools: tools(&["bash"]),
        inputs: None,
    };
    let mut budget = LoopSteerBudget::resumed(Some(unrecorded.clone()), 1);
    assert_eq!(
        budget.escalate(&bash("cargo test")),
        LoopEscalation::Abort { warned: unrecorded },
    );
}

/// The checkpoint reconciliation on a checkpoint that CARRIES the count
/// (#2809): a turn that spent its whole budget resumes with it spent, so
/// the next detection ends the turn whichever loop it is.
///
/// This is the half that was lossy until the count reached the wire — the
/// resumed turn used to restore `spent = 1` from the legacy bool and buy
/// one more warning for a different loop, which is exactly what the
/// second assertion here now refuses.
#[test]
fn a_resumed_turn_restores_the_recorded_steer_count() {
    let warned = bash("cargo test");
    let mut resumed = LoopSteerBudget::resumed(Some(warned.clone()), MAX_LOOP_STEERS);
    assert_eq!(resumed.remaining(), 0, "a spent budget resumes spent");
    assert!(
        aborted(&resumed.escalate(&bash("cargo build"))),
        "a turn that had already spent every warning must not re-open one \
         for a different loop merely by being resumed"
    );
}

/// The same reconciliation on a checkpoint written BEFORE the count field
/// existed: `#[serde(default)]` decodes it as `0`, which read alone would
/// refund the warning the turn already paid for. The legacy `loop_steered`
/// flag is what stops that, and this pins it — the compatibility half of
/// #2809, and the reason `CHECKPOINT_VERSION` did not need a bump.
#[test]
fn a_legacy_checkpoint_still_restores_one_spent_steer() {
    let warned = bash("cargo test");
    let mut resumed = LoopSteerBudget::resumed(Some(warned.clone()), 0);
    assert_eq!(
        resumed.spent(),
        1,
        "never a refund, whatever the count says"
    );
    assert!(
        aborted(&resumed.escalate(&warned)),
        "the warning is not refunded by the round trip"
    );
    let mut fresh = LoopSteerBudget::resumed(Some(warned), 0);
    assert!(
        !aborted(&fresh.escalate(&bash("cargo build"))),
        "and one steer remains for a provably different loop, exactly as \
         a pre-#2809 checkpoint always behaved"
    );
    assert_eq!(
        LoopSteerBudget::resumed(None, 0),
        LoopSteerBudget::default(),
        "a turn that never steered resumes with its whole budget"
    );
    assert_eq!(
        LoopSteerBudget::resumed(None, MAX_LOOP_STEERS),
        LoopSteerBudget::default(),
        "and a count with no warned loop beside it charges nothing: no \
         abort could name the loop it claims to have warned about"
    );
}

/// The round trip the two tests above bracket: whatever `escalate` spends,
/// `spent()` reports it and `resumed()` reads it back unchanged, so a
/// checkpoint taken at any point in a turn restores the same budget.
#[test]
fn the_spent_count_survives_a_checkpoint_round_trip() {
    let mut live = LoopSteerBudget::default();
    for detected in [bash("cargo test"), bash("cargo build")] {
        live.escalate(&detected);
        let restored = LoopSteerBudget::resumed(live.warned().cloned(), live.spent());
        assert_eq!(restored, live, "the budget must survive its own snapshot");
    }
    assert_eq!(live.spent(), MAX_LOOP_STEERS);
}

/// A loop identity over a deliberately tiny alphabet, so an arbitrary
/// sequence collides with itself often enough to exercise the
/// same-loop rung rather than wandering through distinct loops.
fn arb_identity() -> impl Strategy<Value = LoopIdentity> {
    proptest::collection::vec(
        (
            proptest::sample::select(vec!["bash", "read_file", "edit_file"]),
            proptest::sample::select(vec!["a", "b", "c"]),
        ),
        0..4usize,
    )
    .prop_flat_map(|pairs| {
        let tools: Vec<String> = pairs.iter().map(|(t, _)| (*t).to_string()).collect();
        let inputs: Vec<String> = pairs.iter().map(|(_, i)| (*i).to_string()).collect();
        // `None` inputs are the unrecorded resume shape, which must be
        // covered by the same properties as a fully recorded loop.
        prop_oneof![
            Just(LoopIdentity {
                tools: tools.clone(),
                inputs: Some(inputs)
            }),
            Just(LoopIdentity {
                tools,
                inputs: None
            }),
        ]
    })
}

proptest! {
    /// The bound that makes the whole change safe: whatever the model
    /// does, the turn cannot buy more than [`MAX_LOOP_STEERS`] warnings.
    /// Driven past its own aborts on purpose — the engine stops at the
    /// first one, so this is the strictly harder claim.
    #[test]
    fn no_detection_sequence_outspends_the_cap(
        detections in proptest::collection::vec(arb_identity(), 0..24),
    ) {
        let mut budget = LoopSteerBudget::default();
        let steers = detections
            .iter()
            .filter(|identity| !aborted(&budget.escalate(identity)))
            .count();
        prop_assert!(
            steers <= MAX_LOOP_STEERS as usize,
            "{steers} warnings spent of a {MAX_LOOP_STEERS} cap"
        );
    }

    /// A warning is never issued twice for one loop: whatever a
    /// detection just earned, re-detecting the SAME loop immediately
    /// afterwards ends the turn. This is what stops the per-loop budget
    /// becoming a licence to loop.
    #[test]
    fn re_detecting_what_was_just_warned_always_aborts(
        detections in proptest::collection::vec(arb_identity(), 1..12),
    ) {
        let mut budget = LoopSteerBudget::default();
        for identity in &detections {
            if aborted(&budget.escalate(identity)) {
                continue;
            }
            let mut echo = budget.clone();
            prop_assert!(
                aborted(&echo.escalate(identity)),
                "a loop just warned about earned a second warning: {identity:?}"
            );
        }
    }

    /// Every escalation leaves a loop recorded, so the abort message can
    /// always name what the last warning was about — the `Option` that
    /// `LoopEscalation::Abort` exists to remove from the caller.
    #[test]
    fn a_charged_budget_always_knows_what_it_warned_about(
        detections in proptest::collection::vec(arb_identity(), 1..12),
    ) {
        let mut budget = LoopSteerBudget::default();
        for identity in &detections {
            if let LoopEscalation::Abort { warned } = budget.escalate(identity) {
                prop_assert_eq!(Some(&warned), budget.warned());
            }
            prop_assert!(budget.warned().is_some());
        }
    }
}

mod stall {
    use super::StatusAndBash;
    use crate::driver::config::EngineConfig;
    use crate::driver::loop_escalation::{
        LOOP_STEER_PREFIX, LoopSteerBudget, STALL_STEER_PREFIX, STALL_STEER_THRESHOLD_SECS,
        check_loop_detection, steer_stalled_turn, turn_stall_seconds,
    };
    use crate::driver::loop_evidence::ResultIdentities;
    use crate::event_sender::EventSender;
    use crate::loop_detect::CallRecord;
    use proptest::prelude::*;
    use std::borrow::Cow;
    use stella_protocol::tool::{ToolCall, ToolOutput, ToolResult};
    use stella_protocol::{AgentEvent, CompletionMessage, MessageRole, SteerCause};

    /// One resolved `bash` call and the bytes it produced, as the pair of
    /// transcript messages the driver would have accumulated.
    fn call(id: &str, command: &str, output: &str) -> [CompletionMessage; 2] {
        let mut assistant = CompletionMessage::assistant("");
        assistant.tool_calls = vec![ToolCall {
            call_id: id.to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({ "command": command }),
        }];
        let mut result = CompletionMessage {
            role: MessageRole::Tool,
            content: String::new(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            attachments: Vec::new(),
        };
        result.tool_results = vec![ToolResult {
            call_id: id.to_string(),
            output: ToolOutput::ok(output),
        }];
        [assistant, result]
    }

    /// A window of `bash` calls, as [`CallRecord`]s — the shape
    /// [`turn_stall_seconds`] reads, without a transcript around it.
    fn records(commands: &[&str]) -> Vec<CallRecord<'static>> {
        commands
            .iter()
            .enumerate()
            .map(|(i, command)| CallRecord {
                call: Cow::Owned(ToolCall {
                    call_id: format!("c{i}"),
                    name: "bash".to_string(),
                    input: serde_json::json!({ "command": command }),
                }),
                output: Some(Cow::Owned(ToolOutput::ok("done"))),
                identity: None,
            })
            .collect()
    }

    /// #3624's decision, pinned: a sleep the shell never ran to term
    /// still contributes its full request, because the bound is what the
    /// model asked for and that is what the steer names.
    #[test]
    fn a_sleep_the_shell_never_ran_still_counts_what_it_asked_for() {
        let mut window = records(&["sleep 300; echo done"; 3]);
        for record in &mut window {
            record.output = Some(Cow::Owned(ToolOutput::error(
                "command timed out after 120s",
            )));
        }
        assert_eq!(turn_stall_seconds(&window), 900);
        assert!(turn_stall_seconds(&window) >= STALL_STEER_THRESHOLD_SECS);
    }

    /// #2022's witness, in the shape the trace recorded: three
    /// `sleep 300; echo done` calls with a poll between each, 900s of a
    /// 900s allowance spent waiting.
    ///
    /// Every loop rung is silent on it *correctly*, which is why nothing
    /// caught it for months — exact repeat needs the sleeps adjacent and
    /// they are not; the interleaved rung counts all three but is
    /// suppressed because the polls answer differently, so the window
    /// reads as progressing; stagnation needs one tool's output to stop
    /// changing and the polls' does not; and the budget guard is
    /// spend-based, so none of this costs a cent. The turn is steered
    /// anyway.
    #[test]
    fn a_turn_that_sleeps_away_its_budget_is_steered_though_no_loop_fires() {
        let mut messages = vec![
            CompletionMessage::system("sys"),
            CompletionMessage::user("optimize the portfolio"),
        ];
        messages.extend(call("c1", "sleep 300; echo done", "done"));
        messages.extend(call("c2", "tail -n 5 build.log", "line A"));
        messages.extend(call("c3", "sleep 300; echo done", "done"));
        messages.extend(call("c4", "tail -n 5 build.log", "line B"));
        messages.extend(call("c5", "sleep 300; echo done", "done"));

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let events = EventSender::new(tx);
        let outcome = check_loop_detection(
            &EngineConfig::default(),
            &StatusAndBash,
            &mut messages,
            &ResultIdentities::default(),
            &mut LoopSteerBudget::default(),
            0.0,
            &events,
        );

        assert!(outcome.is_none(), "a stalled turn is steered, never killed");
        let drained: Vec<AgentEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            !drained
                .iter()
                .any(|e| matches!(e, AgentEvent::LoopDetected { .. })),
            "no loop rung fires on this window — that is the whole point: {drained:?}"
        );

        let steer = messages
            .last()
            .expect("the transcript is not empty")
            .clone();
        assert_eq!(steer.role, MessageRole::User);
        assert!(
            steer.content.starts_with(STALL_STEER_PREFIX),
            "the turn must carry the stalled-turn steer: {:?}",
            steer.content
        );
        assert!(
            steer.content.contains("900s"),
            "the steer names what the turn asked for: {:?}",
            steer.content
        );
        assert!(
            drained.iter().any(|e| matches!(
                e,
                AgentEvent::Steered { text, cause }
                    if text == &steer.content && *cause == SteerCause::Stall
            )),
            "the steer is on the transcript AND on the wire, named as the \
             stall rung rather than left for a prose match (#3622): {drained:?}"
        );

        // Warn once: a second pass over the same (still stalling) window
        // adds nothing, because the marker it would add is already there.
        let before = messages.len();
        let outcome = check_loop_detection(
            &EngineConfig::default(),
            &StatusAndBash,
            &mut messages,
            &ResultIdentities::default(),
            &mut LoopSteerBudget::default(),
            0.0,
            &events,
        );
        assert!(outcome.is_none());
        assert_eq!(messages.len(), before, "the stalled turn is steered once");
    }

    /// Warn-once means once per **turn**, and the transcript it is read
    /// off is session-scoped: `run_turn` is handed the same `Vec` every
    /// turn, with each new prompt appended and nothing trimmed.
    ///
    /// So an unbounded marker scan silently downgrades this rung to once
    /// per *session* — turn 1 is steered, and every later turn can sleep
    /// its whole allowance away in silence because turn 1's marker is
    /// still sitting upstream. That is the exact shape #2022 is about,
    /// arrived at from the other end. The seconds were always turn-bounded
    /// (`recent_call_records`); only the marker scan was not.
    #[test]
    fn a_later_turn_that_sleeps_away_its_budget_is_steered_on_its_own_account() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let events = EventSender::new(tx);
        let steer_once = |messages: &mut Vec<CompletionMessage>| {
            check_loop_detection(
                &EngineConfig::default(),
                &StatusAndBash,
                messages,
                &ResultIdentities::default(),
                &mut LoopSteerBudget::default(),
                0.0,
                &events,
            )
        };

        let mut messages = vec![
            CompletionMessage::system("sys"),
            CompletionMessage::user("optimize the portfolio"),
        ];
        messages.extend(call("a1", "sleep 300; echo done", "done"));
        messages.extend(call("a2", "tail -n 5 build.log", "line A"));
        assert!(steer_once(&mut messages).is_none());
        assert!(
            messages
                .last()
                .is_some_and(|m| m.content.starts_with(STALL_STEER_PREFIX)),
            "turn 1 stalls and is steered: {:?}",
            messages.last()
        );

        // The user answers; turn 2 opens on the same transcript and
        // stalls just as badly.
        messages.push(CompletionMessage::user("now try the hedged variant"));
        messages.extend(call("b1", "sleep 300; echo done", "done"));
        messages.extend(call("b2", "tail -n 5 build.log", "line B"));
        let before = messages.len();
        assert!(steer_once(&mut messages).is_none());

        let steer = messages.last().expect("the transcript is not empty");
        assert_eq!(
            messages.len(),
            before + 1,
            "turn 2 stalled on its own account and must hear about it"
        );
        assert_eq!(steer.role, MessageRole::User);
        assert!(
            steer.content.starts_with(STALL_STEER_PREFIX),
            "{:?}",
            steer.content
        );
        assert!(
            steer.content.contains("300s"),
            "the steer names what THIS turn asked for, not the session total: {:?}",
            steer.content
        );

        // Still once within turn 2: a second pass adds nothing.
        let before = messages.len();
        assert!(steer_once(&mut messages).is_none());
        assert_eq!(messages.len(), before, "and still exactly once per turn");

        let steers = std::iter::from_fn(|| rx.try_recv().ok())
            .filter(|e| matches!(e, AgentEvent::Steered { .. }))
            .count();
        assert_eq!(steers, 2, "one steer per stalled turn, on the wire too");
    }

    /// The steer rides `LOOP_STEER_PREFIX` rather than minting a marker of
    /// its own, which is what lets every existing consumer — the receipt
    /// classifier, the prompt's injection-defense contract, and the turn
    /// boundary scan that must not read an engine injection as a user turn
    /// — handle it unchanged. If these ever come apart, a stalled turn's
    /// steer starts reading as the user speaking.
    #[test]
    fn stall_steer_prefix_extends_the_loop_steer_marker() {
        assert!(
            STALL_STEER_PREFIX.starts_with(LOOP_STEER_PREFIX),
            "{STALL_STEER_PREFIX:?} must extend {LOOP_STEER_PREFIX:?}"
        );
        assert!(crate::engine_markers::ENGINE_MARKERS.contains(&LOOP_STEER_PREFIX));
    }

    /// The remedy has to be one the agent can actually perform. The
    /// sibling `bash` advisory spent months naming `read_output` /
    /// `wait_for` / `start_process`, deleted in #3244 (#3555) — and the
    /// parked wait, which this shape really is a worse version of, is
    /// deposited by a tool and cannot be asked for by the model at all.
    #[test]
    fn the_stall_steer_names_no_instrument_the_model_cannot_reach() {
        let text = super::super::stall_steer_text(900);
        assert!(text.contains("poll"), "{text}");
        for gone in [
            "read_output",
            "wait_for",
            "start_process",
            "parked wait",
            "vary the arguments",
        ] {
            assert!(
                !text.contains(gone),
                "the steer names `{gone}`, which the model cannot reach: {text}"
            );
        }
    }

    /// The threshold is a turn-level escalation, not a second copy of
    /// `bash`'s 30s per-call advisory.
    #[test]
    fn a_turn_under_the_threshold_is_left_alone() {
        let quiet = records(&["sleep 60", "cargo test"]);
        let secs = turn_stall_seconds(&quiet);
        assert_eq!(secs, 60, "only the bare sleep counts, not the test run");
        assert!(secs < STALL_STEER_THRESHOLD_SECS);

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let events = EventSender::new(tx);
        let mut messages = vec![CompletionMessage::user("build it")];
        steer_stalled_turn(&mut messages, secs, &events);
        assert_eq!(
            messages.len(),
            1,
            "60s is `bash`'s per-call advisory territory, not the turn rung's"
        );
        assert!(
            rx.try_recv().is_err(),
            "and nothing goes on the wire either"
        );
    }

    /// Model-authored seconds are runtime data: an absurd request
    /// saturates rather than overflowing (invariant 5).
    #[test]
    fn an_absurd_sleep_request_saturates() {
        let absurd = records(&["sleep 99999999999999999999", "sleep 99999999999999999999"]);
        assert_eq!(turn_stall_seconds(&absurd), u64::MAX);
    }

    proptest! {
        /// The expensive direction. A sleep beside real work is an
        /// ordinary retry backoff, and however long the backoff or however
        /// many of them a turn makes, it can never earn a stall steer —
        /// clamping or scolding a legitimate retry loop breaks real tasks,
        /// which is why the classifier is biased toward silence.
        #[test]
        fn a_sleep_beside_real_work_never_stalls_a_turn(
            secs in proptest::collection::vec(1u64..600, 1..12),
            work in proptest::collection::vec(
                proptest::sample::select(vec![
                    "curl -sf http://localhost:8080/health",
                    "cargo test -p stella-core",
                    "tail -n 20 build.log",
                    "git status --porcelain",
                ]),
                1..12,
            ),
        ) {
            let commands: Vec<String> = secs
                .iter()
                .zip(work.iter().cycle())
                .enumerate()
                .map(|(i, (s, cmd))| {
                    if i % 2 == 0 {
                        format!("sleep {s} && {cmd}")
                    } else {
                        format!("{cmd}; sleep {s}")
                    }
                })
                .collect();
            let borrowed: Vec<&str> = commands.iter().map(String::as_str).collect();
            prop_assert_eq!(turn_stall_seconds(&records(&borrowed)), 0);
        }
    }
}
