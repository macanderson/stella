//! The reflection prompt must not undercut its own instruction.
//!
//! This prompt has been wrong twice, in opposite directions (#768, then #944),
//! and the second fix landed in two halves — the framing question in #944, the
//! body it contradicted only afterwards. In between, the prompt asked for facts
//! that inspection could not reveal and then offered a one-grep fact as its
//! model of a good lesson.
//!
//! That is a specific and recurring failure, not a typo: the instruction was
//! correct in the abstract each time, and what the prompt *showed* pulled the
//! other way. An example outranks a rule for the model reading it, so the
//! examples are the part worth pinning.
//!
//! These assertions are deliberately coarse. They do not score prompt quality —
//! nothing here can. They pin the two things that were actually wrong: that the
//! discard test is present at all, and that the construct which carried the
//! anti-example both times has not come back.
//!
//! The witnesses at the bottom of the file pin something else: what the prompt
//! is allowed to *see*. Both fail on the tail-window digest #2460 replaced, each
//! on a different one of its two defects.

use std::sync::Mutex;

use async_trait::async_trait;
use stella_protocol::{
    CompletionMessage, CompletionRequestRef, CompletionResult, CompletionUsage, MessageRole,
    Provider, ProviderError, ReasoningEffort, ToolCall, ToolOutput, ToolResult,
};

/// Records the prompt it is asked to complete — and the request's dispatch
/// shape (effort, output cap) — then answers with a well-formed empty result
/// so the caller's parsing path stays on its happy road: the request is what
/// is under test, not the response handling.
#[derive(Default)]
struct CapturingProvider {
    prompt: Mutex<String>,
    shape: Mutex<(Option<ReasoningEffort>, Option<u32>)>,
    reasoning: Mutex<Option<bool>>,
}

#[async_trait]
impl Provider for CapturingProvider {
    fn id(&self) -> &str {
        "capturing"
    }
    async fn complete_ref(
        &self,
        req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        if let Some(user) = req
            .messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::User)
        {
            *self.prompt.lock().expect("prompt lock") = user.content.clone();
        }
        *self.shape.lock().expect("shape lock") = (req.effort, req.max_output_tokens);
        *self.reasoning.lock().expect("reasoning lock") = req.reasoning;
        Ok(CompletionResult {
            text: r#"{"lessons": []}"#.into(),
            tool_calls: vec![],
            usage: CompletionUsage {
                reported: true,
                input_tokens: 1,
                ..CompletionUsage::default()
            },
            model: "capturing".into(),
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

/// Drive the real prompt builder and hand back exactly what it sent.
async fn prompt_for(succeeded: bool) -> String {
    let transcript = vec![
        CompletionMessage::user("fix the leak"),
        CompletionMessage::assistant("swapped db() for withTenantDb"),
    ];
    prompt_for_evidence(super::TurnEvidence::from_transcript(&transcript, succeeded)).await
}

/// The same, for a caller that has already assembled the turn's evidence — the
/// selection tests below build transcripts and friction ledgers of their own.
async fn prompt_for_evidence(evidence: super::TurnEvidence<'_>) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".stella")).expect("workspace");
    let provider = CapturingProvider::default();
    super::reflect_on_turn(
        &provider,
        "capturing",
        dir.path(),
        evidence,
        &["testing".to_string()],
        None,
        super::ReflectionPosture::default(),
    )
    .await
    .expect("the stub provider cannot fail");
    provider.prompt.lock().expect("prompt lock").clone()
}

/// The construct that carried the anti-example through two rounds of fixes.
///
/// Both times, the prompt held up "amounts are stored as integer minor units;
/// use money.parse_amount" as what a good lesson looks like — a fact one grep
/// away, which is exactly the class the prompt now tells the model to discard.
/// The phrase is the fingerprint: if it returns, so has the contradiction.
const THE_ANTI_PATTERN: &str = "A good domain lesson reads like";

#[tokio::test]
async fn the_prompt_states_the_rediscovery_test_on_both_outcomes() {
    for succeeded in [true, false] {
        let prompt = prompt_for(succeeded).await;
        assert!(
            prompt.contains("could a competent engineer find this in under a minute"),
            "succeeded={succeeded}: the prompt no longer applies the rediscovery \
             test, which is the whole mechanism that keeps greppable facts out \
             of the store"
        );
        assert!(
            prompt.contains("DISCARD IT"),
            "succeeded={succeeded}: the test is stated but not actionable — the \
             model is told what to weigh and not what to do about it"
        );
    }
}

#[tokio::test]
async fn the_prompt_does_not_offer_a_greppable_fact_as_its_model_lesson() {
    for succeeded in [true, false] {
        let prompt = prompt_for(succeeded).await;
        assert!(
            !prompt.contains(THE_ANTI_PATTERN),
            "succeeded={succeeded}: {THE_ANTI_PATTERN:?} is back. That sentence \
             introduced a one-grep fact as the ideal lesson in both previous \
             versions of this prompt, contradicting the instruction above it. \
             If a worked example is wanted, name a fact that would actually \
             cost something to rediscover — see the Good/Bad pair in the prompt."
        );
        assert!(
            prompt.contains("Bad: \"commands are registered in registry.py\""),
            "succeeded={succeeded}: the named counter-example is gone. It is the \
             measured one — that fact was held seven separate times in a live \
             store — and naming it is what makes the rule concrete"
        );
    }
}

/// The framing question from #944, which is the half that did land. Pinned here
/// so a future edit cannot quietly restore the "where things live" framing that
/// #768's over-correction introduced.
#[tokio::test]
async fn a_successful_turn_is_asked_what_surprised_it() {
    let prompt = prompt_for(true).await;
    assert!(
        prompt.contains("What SURPRISED you?"),
        "the surprise framing is the operational form of the principle: if \
         inspection would have told you, it will tell you again for free"
    );
    assert!(
        !prompt.contains("where things live"),
        "the prompt is asking for file locations again, which is the \
         over-correction #944 was fixing"
    );
}

/// #1847's request-shape half, and #2174's: reflection is a bounded
/// management call that must also leave a reasoning model room to think.
///
/// `effort` was previously unset, which leaves the provider's own default
/// reasoning allowance in force — unbounded thinking spent deciding, most
/// turns, to return an empty list. Pinned low it matches the shape every
/// bounded management call already has (the pipeline's `management_bounds`
/// pins triage the same way; so does the engine's overflow summarizer).
///
/// The cap is asserted alongside because the two bounds are one dispatch
/// contract — and because the cap sent for a year was the written contract
/// with NO headroom. `max_output_tokens` is one number on the wire and a
/// reasoning model bills its thinking against it, so that cap came back spent
/// entirely on reasoning: `finish_reason: length`, empty text, zero lessons,
/// and a learning plane frozen for nine days with every surface reporting
/// health (#2174).
#[tokio::test]
async fn reflection_dispatches_low_effort_with_a_cap_that_leaves_room_to_think() {
    let (shape, reasoning) = dispatch_shape(super::ReflectionPosture::default()).await;
    assert_eq!(
        shape,
        (
            Some(ReasoningEffort::Low),
            Some(stella_core::starvation::with_reasoning_headroom(2048)),
        ),
        "reflection must dispatch as a bounded, pinned-low management call \
         whose cap covers its output contract PLUS thinking room"
    );
    assert!(
        shape.1.expect("a cap is sent") > 2048,
        "sending the written contract alone is the #2174 defect itself"
    );
    assert_eq!(
        reasoning, None,
        "with no triage posture configured, the provider default stands — \
         exactly as before the posture was threaded"
    );
}

/// #2174 witness: the triage agent's configured posture reaches the wire.
///
/// Reflection dispatches on the model the triage pin selected, and built its
/// request with `reasoning: None` regardless — so `agents.triage.reasoning:
/// off` selected the model for a call it could not reach. The effort half is
/// asserted in the same breath: reflection's own low pin is a default, and an
/// operator's explicit setting is the more specific statement about the call.
#[tokio::test]
async fn the_triage_posture_reaches_the_reflection_wire() {
    let (shape, reasoning) = dispatch_shape(super::ReflectionPosture {
        reasoning: Some(false),
        effort: Some(ReasoningEffort::High),
    })
    .await;
    assert_eq!(
        reasoning,
        Some(false),
        "an explicit off must reach the wire"
    );
    assert_eq!(
        shape.0,
        Some(ReasoningEffort::High),
        "an explicitly configured triage effort outranks reflection's own \
         low default"
    );
}

/// Drive the real dispatch and hand back `((effort, cap), reasoning)`.
async fn dispatch_shape(
    posture: super::ReflectionPosture,
) -> ((Option<ReasoningEffort>, Option<u32>), Option<bool>) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".stella")).expect("workspace");
    let provider = CapturingProvider::default();
    let transcript = vec![CompletionMessage::user("fix the leak")];
    super::reflect_on_turn(
        &provider,
        "capturing",
        dir.path(),
        super::TurnEvidence::from_transcript(&transcript, true),
        &["testing".to_string()],
        None,
        posture,
    )
    .await
    .expect("the stub provider cannot fail");
    let shape = *provider.shape.lock().expect("shape lock");
    let reasoning = *provider.reasoning.lock().expect("reasoning lock");
    (shape, reasoning)
}

/// One assistant message that calls one tool, and the `Tool` message answering
/// it. Built as the engine builds them — `content` empty, payload in
/// `tool_results` — because that construction is the whole of defect two.
fn tool_exchange(call_id: &str, name: &str, output: ToolOutput) -> Vec<CompletionMessage> {
    vec![
        CompletionMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            tool_calls: vec![ToolCall {
                call_id: call_id.to_string(),
                name: name.to_string(),
                input: serde_json::json!({"cmd": "cargo test -p stella-core"}),
            }],
            tool_results: Vec::new(),
            attachments: Vec::new(),
        },
        CompletionMessage {
            role: MessageRole::Tool,
            content: String::new(),
            tool_calls: Vec::new(),
            tool_results: vec![ToolResult {
                call_id: call_id.to_string(),
                output,
            }],
            attachments: Vec::new(),
        },
    ]
}

/// WITNESS (#2460, defect one — the window is in the wrong place).
///
/// A thirty-message turn whose only learnable fact sits at index 8. The old
/// digest read the last twelve messages, so index 8 of 30 was outside the window
/// by construction and the fact could not reach the prompt at any truncation
/// length. Selection keeps it because the tool call it provoked errored.
#[tokio::test]
async fn a_fact_in_the_middle_of_a_long_turn_reaches_the_reflection_prompt() {
    const THE_FACT: &str = "withTenantDb must be opened before the migration lock or the \
                            leak reappears silently";
    let mut transcript = vec![CompletionMessage::user("fix the tenancy leak")];
    for step in 0..14 {
        if step == 4 {
            // The fact, and the failure that taught it, in the middle.
            transcript.push(CompletionMessage::assistant(THE_FACT));
            transcript.extend(tool_exchange(
                "call_middle",
                "bash",
                ToolOutput::Error {
                    message: "migration lock held by another connection".into(),
                },
            ));
            continue;
        }
        transcript.extend(tool_exchange(
            &format!("call_{step}"),
            "read_file",
            ToolOutput::Ok {
                content: format!("routine step {step}, nothing to learn here"),
            },
        ));
    }
    transcript.push(CompletionMessage::assistant("done — tests pass"));
    assert!(
        transcript.len() > 24,
        "the fact must sit well outside a twelve-message tail, or this witness \
         proves nothing"
    );

    let prompt = prompt_for_evidence(super::TurnEvidence::from_transcript(&transcript, true)).await;
    assert!(
        prompt.contains(THE_FACT),
        "reflection is being asked what it learned by a witness that cannot see \
         where the turn went wrong: the fact at index 5 of {} never reached the \
         prompt.\n\nprompt was:\n{prompt}",
        transcript.len()
    );
}

/// WITNESS (#2460, defect two — a `Tool` message's payload is not in `content`).
///
/// Four messages, so every one of them is inside even the old twelve-message
/// tail: the window is not what this pins. The old digest rendered
/// `message.content`, which the engine leaves EMPTY on a `Tool` message, so
/// every tool result Stella has ever produced reached reflection as the six
/// characters `"tool: "` — the 300-character cut never even applied.
#[tokio::test]
async fn a_failed_tool_result_reaches_the_reflection_prompt() {
    const THE_ERROR: &str = "assertion failed: expected 1_15 minor units, got 1_14";
    let mut transcript = vec![CompletionMessage::user("make the money test pass")];
    transcript.extend(tool_exchange(
        "call_1",
        "bash",
        ToolOutput::Error {
            message: THE_ERROR.into(),
        },
    ));
    transcript.push(CompletionMessage::assistant("gave up"));

    let prompt =
        prompt_for_evidence(super::TurnEvidence::from_transcript(&transcript, false)).await;
    assert!(
        prompt.contains(THE_ERROR),
        "the tool result that failed is the highest-value evidence in the turn, \
         and it is not in the prompt.\n\nprompt was:\n{prompt}"
    );
    assert!(
        prompt.contains("bash"),
        "a failed result must name the tool that produced it — the name lives on \
         the call, not the result, so it has to be joined by call id"
    );
}

/// #2460's definition of done, item 2: what reflection costs per turn, reported
/// rather than asserted into invisibility.
///
/// The digest half of the comparison lives in `digest::tests` (which can measure
/// the old tail rendering directly). This one measures the whole billed prompt —
/// instructions plus digest — because that is the number an operator pays, and
/// the instruction block is the larger of the two on a short turn.
///
/// Run it with `--nocapture` to read the numbers. The assertion is only a
/// ceiling: pinning an exact size would fail on every prompt edit, for reasons
/// that are not about cost.
#[tokio::test]
async fn the_billed_prompt_size_is_reported_and_bounded() {
    let bare = vec![CompletionMessage::user("fix the leak")];
    let instructions = prompt_for_evidence(super::TurnEvidence::from_transcript(&bare, true))
        .await
        .chars()
        .count();

    let mut heavy = vec![CompletionMessage::user("make the suite green")];
    for step in 0..40 {
        let id = format!("call_{step}");
        heavy.extend(tool_exchange(
            &id,
            "bash",
            ToolOutput::Ok {
                content: format!("step {step}: {}", "running 412 tests ... ok ".repeat(30)),
            },
        ));
    }
    heavy.push(CompletionMessage::assistant("done"));
    let loaded = prompt_for_evidence(super::TurnEvidence::from_transcript(&heavy, true))
        .await
        .chars()
        .count();

    println!(
        "#2460 reflection prompt: instructions alone = {instructions} chars \
         (~{} tokens); with a full {}-message turn selected = {loaded} chars \
         (~{} tokens)",
        instructions / 4,
        heavy.len(),
        loaded / 4
    );
    assert!(
        loaded <= instructions + super::digest::DIGEST_BUDGET_CHARS + 4_000,
        "the billed prompt is {loaded} chars, past the instruction block plus the \
         digest budget plus its bounded pinned allowance. The budget is the one \
         knob that keeps reflection's per-turn cost arguable — if this fires, it \
         moved without anyone deciding to move it"
    );
}

/// The event-derived half of the evidence (#2460's definition of done, item 1):
/// cost, wall clock, retries and loop firings leave no message at all, so no
/// window over the transcript can reach them.
#[tokio::test]
async fn the_prompt_names_where_the_turn_spent_itself() {
    let mut friction = super::TurnFriction::default();
    friction.observe(&stella_protocol::AgentEvent::StepUsage {
        step: 9,
        role: stella_protocol::ModelCallRole::Worker,
        provider: "anthropic".into(),
        output_text: None,
        model: "claude".into(),
        input_tokens: 120_000,
        output_tokens: 900,
        cached_input_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: None,
        estimated_input_tokens: 0,
        cost_usd: 0.42,
        duration_ms: 42_000,
        retries: 0,
        tool_calls: 3,
        complete: true,
        finish_reason: None,
    });
    friction.observe(&stella_protocol::AgentEvent::LoopDetected {
        turn_instance: 0,
        kind: "stagnation".into(),
        pattern: vec!["bash".into()],
        repeats: 4,
        evidence: "same command, no progress".into(),
        aborted: false,
    });
    let transcript = vec![CompletionMessage::user("fix it")];
    let prompt = prompt_for_evidence(super::TurnEvidence {
        transcript: &transcript,
        friction: &friction,
        succeeded: false,
    })
    .await;
    assert!(
        prompt.contains("step 9 (worker)") && prompt.contains("$0.4200"),
        "the costliest step is where a turn's money went, and it is nowhere in \
         the transcript.\n\nprompt was:\n{prompt}"
    );
    assert!(
        prompt.contains("stagnation ×4"),
        "a loop-detector firing leaves no message behind, so a transcript window \
         can never recover it.\n\nprompt was:\n{prompt}"
    );
}
