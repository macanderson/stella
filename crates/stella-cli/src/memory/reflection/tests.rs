//! The reflection prompt must ask for the thing itself, and must not undercut
//! its own instruction.
//!
//! Three failures are pinned here, because all three were the same mistake and
//! the third is the one this file now spends most of its assertions on.
//!
//! #768 asked "what should change next time" — a question about the agent —
//! and got eight process notes out of ten. #944 over-corrected into "where
//! things live" and filled a store with one-grep facts, then landed its repair
//! in two halves, leaving the prompt telling the model to discard exactly the
//! kind of fact its own worked example held up as ideal. The repair for *that*
//! was a rediscovery-cost test operationalized as surprise, which measures
//! novelty when what a memory is worth is savings — so it discarded, by rule,
//! every fact that is cheap to look up and expensive to lack.
//!
//! Each repair replaced one guess about the model's topic with another. The
//! prompt now names no topics, asks the counterfactual directly, and requires
//! each lesson to arrive with its trigger and the moment it would have changed.
//! So there are two classes of assertion below: that the question and its
//! grounding are present, and that no topic list has grown back.
//!
//! These are deliberately coarse. They do not score prompt quality — nothing
//! here can. They pin what has actually gone wrong before.
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
            upstream_provider: None,
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
async fn the_prompt_states_the_action_change_test_on_both_outcomes() {
    for succeeded in [true, false] {
        let prompt = prompt_for(succeeded).await;
        assert!(
            prompt.contains("WOULD KNOWING THIS HAVE CHANGED WHAT YOU ACTUALLY DID"),
            "succeeded={succeeded}: the prompt no longer applies the \
             action-change test. It is the only thing standing between an open \
             invitation to record and a store full of true, useless facts — the \
             topic lists that used to do that job are deliberately gone"
        );
        assert!(
            prompt.contains("changes nothing and is worth nothing"),
            "succeeded={succeeded}: the test is stated without its cheap half. \
             A model told only to keep what changed an action still keeps what \
             it would have grepped anyway, because that is technically true; \
             the discard has to be spelled out"
        );
    }
}

/// The half of the change that is not a rewording: a lesson must arrive with
/// its trigger and with the moment it would have changed.
///
/// This is what replaces the topic guidance rather than merely deleting it. An
/// open question with no required justification moves the guess from the prompt
/// author to the model without making it any more checkable than it was — the
/// answer is still an assertion that something would have helped. The two
/// fields are the argument the assertion has to come with, and `saves` in
/// particular is refutable against the very transcript in front of the model.
#[tokio::test]
async fn every_lesson_must_carry_its_trigger_and_the_moment_it_would_have_changed() {
    for succeeded in [true, false] {
        let prompt = prompt_for(succeeded).await;
        assert!(
            prompt.contains("\"trigger\"") && prompt.contains("\"saves\""),
            "succeeded={succeeded}: the response shape no longer asks for both \
             fields, so nothing makes a lesson say when it applies or what it \
             would have bought"
        );
        assert!(
            prompt.contains("If you cannot name the moment, do not record the lesson"),
            "succeeded={succeeded}: `saves` is requested but not enforced. \
             Without the refusal it degrades into a third place to restate the \
             lesson, and the grounding this change exists for is gone"
        );
    }
}

/// The fourth failure this prompt could have, guarded before it happens.
///
/// Three times the prompt has been wrong by guessing at the model's topic
/// (#768's "what should change", #944's "where things live", and the
/// rediscovery test's blindness to cheap-to-find/expensive-to-lack facts). Each
/// repair replaced one guess with another. The instruction now names no topics
/// at all, and these are the two enumerations that carried the last guess — if
/// either returns, so has the failure mode.
#[tokio::test]
async fn the_prompt_prescribes_no_topics() {
    for succeeded in [true, false] {
        let prompt = prompt_for(succeeded).await;
        assert!(
            !prompt.contains("do NOT record: where files live"),
            "succeeded={succeeded}: the forbidden-topic list is back. It was a \
             guess about what a lesson may be about, made by whoever wrote the \
             prompt rather than by the model that watched the turn"
        );
        assert!(
            !prompt.contains("DO record what inspection cannot reveal"),
            "succeeded={succeeded}: the prescribed-topic list is back, which is \
             the same guess wearing the other sign"
        );
        assert!(
            prompt.contains("There is no approved list of topics"),
            "succeeded={succeeded}: the model is no longer told that the choice \
             is its own. Silence is not the same instruction — it leaves the \
             model's own prior about what a memory looks like in force, \
             unstated and different per provider"
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

/// The framing question, pinned on both outcomes.
///
/// Every previous version asked a proxy — "what should change next time",
/// "where things live", "what surprised you" — and got exactly the class of
/// answer the proxy described. The counterfactual is the objective itself, and
/// the phrasing that carries it is the no-memory clause: it is what stops the
/// model answering from the position it is actually in, which is the one
/// position a future session will never be in.
#[tokio::test]
async fn both_outcomes_ask_the_counterfactual_not_a_proxy() {
    for succeeded in [true, false] {
        let prompt = prompt_for(succeeded).await;
        assert!(
            prompt.contains("with no memory of anything that happened here"),
            "succeeded={succeeded}: the counterfactual frame is gone. Without \
             it the model answers as itself-now, which knows the answer, rather \
             than as itself-next-time, which is the reader being written for"
        );
        assert!(
            prompt.contains("What do you want to have been told before you start"),
            "succeeded={succeeded}: the question is no longer about what would \
             have helped, and a question about anything else gets an answer \
             about anything else — three times over, so far"
        );
        assert!(
            !prompt.contains("What SURPRISED you?"),
            "succeeded={succeeded}: the surprise proxy is back. It measures \
             novelty, and what a memory is worth is savings; the two come apart \
             on every fact that is cheap to find and expensive to lack"
        );
        assert!(
            !prompt.contains("where things live"),
            "succeeded={succeeded}: the prompt is asking for file locations \
             again, which is the over-correction #944 was fixing"
        );
    }
}

/// A turn is allowed to teach more than three things.
///
/// The old cap was below the number of genuinely distinct findings a busy turn
/// produces, so on those turns the cap — not the test — was what decided, and
/// it decided by arithmetic, silently, after the model had already done the
/// work of finding them.
#[tokio::test]
async fn the_per_turn_lesson_cap_is_above_what_a_turn_typically_teaches() {
    let many = (1..=12)
        .map(|i| format!(r#"{{"lesson": "lesson {i}", "kind": "domain"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    let parsed = super::parse_lessons_checked(&format!("{{\"lessons\": [{many}]}}"), &[]);
    let super::ReflectionParse::Lessons(lessons) = parsed else {
        panic!("a well-formed lesson array must parse as lessons");
    };
    assert_eq!(
        lessons.len(),
        super::MAX_LESSONS_PER_TURN,
        "the parser truncates to a different number than the prompt promises, \
         so the model is told a cap it is not held to"
    );
    // A `const` block, because the predicate is constant and clippy's
    // `assertions_on_constants` is right that a runtime assert on it is
    // theatre: it can only fail on a code change, never on a run. Const
    // evaluation makes that literal — a cap put back below what a turn teaches
    // stops the build rather than waiting for someone to run this test.
    const {
        assert!(
            super::MAX_LESSONS_PER_TURN > 3,
            "the cap is back below what a turn teaches, which makes it the \
             decision again rather than a backstop"
        );
    }
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
/// contract, and because this is that cap's one guard — a response that
/// overruns it parses to zero lessons *exactly like an empty one*, so the
/// symptom of undersizing it is not a truncated lesson but a turn that
/// silently taught nothing. Both halves of the number are pinned here, because
/// each was undersized once with that same invisible symptom:
///
/// - The **written contract** rose to 4096 when a lesson became three prose
///   fields and the per-turn cap became [`super::MAX_LESSONS_PER_TURN`].
/// - The **headroom on top** exists because the cap sent for a year was the
///   written contract alone. `max_output_tokens` is one number on the wire and
///   a reasoning model bills its thinking against it, so that cap came back
///   spent entirely on reasoning: `finish_reason: length`, empty text, zero
///   lessons, and a learning plane frozen for nine days with every surface
///   reporting health (#2174).
#[tokio::test]
async fn reflection_dispatches_low_effort_with_a_cap_that_leaves_room_to_think() {
    let (shape, reasoning) = dispatch_shape(super::ReflectionPosture::default()).await;
    assert_eq!(
        shape,
        (
            Some(ReasoningEffort::Low),
            Some(stella_core::starvation::with_reasoning_headroom(4096)),
        ),
        "reflection must dispatch as a bounded, pinned-low management call \
         whose cap covers its output contract PLUS thinking room"
    );
    assert!(
        shape.1.expect("a cap is sent") > 4096,
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
/// asserted alongside it: reflection's own low pin is a default, and an
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

/// **Witness (#4382), the half that shows what the slice buys.**
///
/// The transcript reflection is handed decides what the prompt argues about,
/// and #2460's selection does not know where a turn starts: given a whole
/// session it selects across turns, so an earlier turn's work reaches the
/// prompt that mines lessons and rates the *current* execution. The
/// interactive doors now hand over the turn slice
/// (`agent::reflect::InteractiveTurn::evidence`); this is why that matters,
/// asserted on the bytes that actually go on the wire.
#[tokio::test]
async fn an_earlier_turns_work_reaches_the_prompt_when_the_whole_session_is_handed_over() {
    const EARLIER_TURN: &str = "swapped db() for withTenantDb in the tenant loader";

    let mut session = vec![
        CompletionMessage::user("fix the tenant leak"),
        CompletionMessage::assistant(EARLIER_TURN),
    ];
    let turn_start = session.len();
    session.extend(tool_exchange("c1", "task_list", ToolOutput::ok("6 done")));
    session.push(CompletionMessage::assistant("six tasks, all done"));

    let whole = prompt_for_evidence(super::TurnEvidence::from_transcript(&session, true)).await;
    assert!(
        whole.contains(EARLIER_TURN),
        "the pre-fix call handed the session over and the earlier turn arrived with it"
    );

    let this_turn = prompt_for_evidence(super::TurnEvidence::from_transcript(
        &session[turn_start..],
        true,
    ))
    .await;
    assert!(
        !this_turn.contains(EARLIER_TURN),
        "and the turn slice leaves it behind: {this_turn}"
    );
    assert!(
        this_turn.contains("task_list"),
        "without losing what this turn did: {this_turn}"
    );
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
                ToolOutput::error("migration lock held by another connection"),
            ));
            continue;
        }
        transcript.extend(tool_exchange(
            &format!("call_{step}"),
            "read_file",
            ToolOutput::Ok {
                content: format!("routine step {step}, nothing to learn here"),
                data: None,
            },
        ));
    }
    transcript.push(CompletionMessage::assistant("done — tests pass"));
    let at = transcript
        .iter()
        .position(|message| message.content == THE_FACT)
        .expect("the fact was planted");
    assert!(
        transcript.len() - at > 12,
        "the fact sits at index {at} of {}, inside a twelve-message tail — this \
         witness would pass on the old digest and prove nothing",
        transcript.len()
    );

    let prompt = prompt_for_evidence(super::TurnEvidence::from_transcript(&transcript, true)).await;
    assert!(
        prompt.contains(THE_FACT),
        "reflection is being asked what it learned by a witness that cannot see \
         where the turn went wrong: the fact at index {at} of {} never reached \
         the prompt.\n\nprompt was:\n{prompt}",
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
        ToolOutput::error(THE_ERROR),
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
                data: None,
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

/// One turn's journal in the shape `agent::run_turn` hands to
/// [`super::digest::TurnFriction::from_events`]: a tool that failed, its
/// pairing start, a retry, and the model call that paid for it all.
fn journal() -> Vec<stella_protocol::AgentEvent> {
    use stella_protocol::{AgentEvent, ToolCall, ToolOutput};
    vec![
        AgentEvent::ToolStart {
            call: ToolCall {
                call_id: "c1".into(),
                name: "bash".into(),
                input: serde_json::json!({ "command": "cargo test" }),
            },
        },
        AgentEvent::ToolResult {
            call_id: "c1".into(),
            output: ToolOutput::Error {
                message: "linker failed: ld returned 1".into(),
                class: None,
            },
            duration_ms: 91_000,
            speculated: false,
        },
        AgentEvent::Retry {
            attempt: 1,
            reason: "429 rate limited".into(),
        },
        AgentEvent::StepUsage {
            upstream_provider: None,
            step: 4,
            role: stella_protocol::ModelCallRole::Worker,
            provider: "anthropic".into(),
            output_text: None,
            model: "claude".into(),
            input_tokens: 80_000,
            output_tokens: 500,
            cached_input_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: None,
            estimated_input_tokens: 0,
            cost_usd: 0.31,
            duration_ms: 12_000,
            retries: 1,
            tool_calls: 1,
            complete: true,
            finish_reason: None,
            sub_agent_id: None,
        },
    ]
}

/// **The #3946 witness.** `the_prompt_names_where_the_turn_spent_itself` below
/// proves the digest *renders* friction once something hands it a ledger. This
/// proves something does.
///
/// Between #3865 (which deleted `FrictionTap`, the ledger's only producer) and
/// this change, every production path built its evidence through
/// [`super::TurnEvidence::from_transcript`], which hardcodes a permanently
/// empty ledger — so the friction section of every shipped digest rendered as
/// nothing at all. The second half of this test is that regression, still
/// executable: same transcript, same turn, no producer, no section.
#[tokio::test]
async fn the_folded_journal_reaches_the_prompt_and_an_unfolded_turn_says_nothing() {
    let events = journal();
    let friction = super::digest::TurnFriction::from_events(&events);
    let transcript = vec![CompletionMessage::user("run the tests")];

    let wired = prompt_for_evidence(super::TurnEvidence::with_friction(
        &transcript,
        &friction,
        false,
    ))
    .await;
    assert!(
        wired.contains("Where this turn spent itself"),
        "a folded journal must render the friction section.\n\nprompt was:\n{wired}"
    );
    assert!(
        wired.contains("linker failed"),
        "the failed tool call is the turn's friction, and `run_turn` saw it.\
         \n\nprompt was:\n{wired}"
    );
    assert!(
        wired.contains("429 rate limited"),
        "a retry leaves no message behind — only the event stream has it.\
         \n\nprompt was:\n{wired}"
    );
    assert!(
        wired.contains("$0.3100"),
        "what the turn cost is nowhere in the transcript.\n\nprompt was:\n{wired}"
    );

    // Anti-vacuity, and the regression itself: the identical turn built the way
    // every door built it before #3946 reaches the model with none of the above.
    let unwired =
        prompt_for_evidence(super::TurnEvidence::from_transcript(&transcript, false)).await;
    assert!(
        !unwired.contains("Where this turn spent itself"),
        "`from_transcript`'s empty ledger must render no friction section — if \
         it does, this test proves nothing about the wiring.\n\nprompt was:\n{unwired}"
    );
}

/// One tool call's pass through a goal round, as the arc's journal carries it.
fn round_tool(
    call_id: &str,
    name: &str,
    command: &str,
    error: Option<&str>,
) -> Vec<stella_protocol::AgentEvent> {
    use stella_protocol::{AgentEvent, ToolCall, ToolOutput};
    vec![
        AgentEvent::ToolStart {
            call: ToolCall {
                call_id: call_id.into(),
                name: name.into(),
                input: serde_json::json!({ "command": command }),
            },
        },
        AgentEvent::ToolResult {
            call_id: call_id.into(),
            output: match error {
                Some(message) => ToolOutput::Error {
                    message: message.into(),
                    class: None,
                },
                None => ToolOutput::Ok {
                    content: "ok".into(),
                    data: None,
                },
            },
            duration_ms: 4_000,
            speculated: false,
        },
    ]
}

/// A three-round goal arc's journal, in the shape `run_goal_turn` hands to
/// [`super::digest::TurnFriction::per_goal_round`]: every round calls a tool
/// the other two never call, and each round is closed by its own `GoalVerdict`
/// — the split key, carrying the round number.
fn goal_journal() -> Vec<stella_protocol::AgentEvent> {
    let verdict = |round: usize, met: bool| stella_protocol::AgentEvent::GoalVerdict {
        round,
        met,
        reasoning: format!("round {round}"),
        cost_usd: 0.01,
    };
    let mut events = round_tool("c1", "bash", "cargo test", Some("round one linker failure"));
    events.push(verdict(1, false));
    events.extend(round_tool("c2", "read_file", "src/lib.rs", None));
    events.push(verdict(2, false));
    events.extend(round_tool("c3", "edit_file", "src/leak.rs", None));
    events.push(verdict(3, true));
    events
}

/// **The #3962 witness.** A goal run is several turns reflected on ONCE, and
/// the ledger it reflects with must be one per round.
///
/// Before this, `/goal` passed an explicitly-empty ledger, because the only
/// alternative on offer — one fold over the whole arc — is not a smaller
/// answer but a wrong one: it reports round 1's failed `bash` as something the
/// round that ran last did. That is the misattribution #3552 named on the
/// wrapper's `TurnFacts`, and the second half of this test is that wrong
/// answer, executable, so the first half cannot be satisfied by producing it.
#[tokio::test]
async fn a_goal_arcs_rounds_reflect_separately_and_never_borrow_each_others_friction() {
    let events = goal_journal();
    let rounds = super::TurnFriction::per_goal_round(&events);
    assert_eq!(
        rounds.len(),
        3,
        "each `GoalVerdict` closes a round, so a three-round arc folds to three ledgers"
    );
    let transcript = vec![CompletionMessage::user("fix the leak and prove it")];

    // Round 3 alone: the round that did NOT run `cargo test` must not be able
    // to say it did, whatever else the arc contains.
    let third_only = prompt_for_evidence(super::TurnEvidence::with_rounds(
        &transcript,
        &rounds[2..],
        true,
    ))
    .await;
    assert!(
        !third_only.contains("round one linker failure"),
        "round 3's ledger holds round 1's failure — the rounds were folded \
         together.\n\nprompt was:\n{third_only}"
    );

    // The whole arc: each round renders its own section, labelled, in order.
    let wired =
        prompt_for_evidence(super::TurnEvidence::with_rounds(&transcript, &rounds, true)).await;
    let first_at = wired
        .find("Where round 1 of 3 spent itself")
        .expect("round 1 must name itself as round 1 of 3");
    let third_at = wired
        .find("Where round 3 of 3 spent itself")
        .expect("round 3 must name itself as round 3 of 3");
    let failure_at = wired
        .find("round one linker failure")
        .expect("round 1's failure is the arc's friction and must survive selection");
    assert!(
        first_at < failure_at && failure_at < third_at,
        "round 1's failure must be rendered inside round 1's section, ahead of \
         round 3's.\n\nprompt was:\n{wired}"
    );

    // Anti-vacuity, and the wrong answer this issue exists to forbid: the same
    // journal folded as ONE ledger puts every round's tools under a single
    // heading that speaks for "this turn", where nothing tells a reader which
    // round ran which — the third round inherits the first round's failure.
    let merged = super::TurnFriction::from_events(&events);
    let conflated = prompt_for_evidence(super::TurnEvidence::with_friction(
        &transcript,
        &merged,
        true,
    ))
    .await;
    assert!(
        conflated.contains("Where this turn spent itself")
            && conflated.contains("round one linker failure")
            && !conflated.contains("Where round 1 of 3"),
        "the merged fold must still render — this half is the regression, and a \
         test that cannot produce it proves nothing.\n\nprompt was:\n{conflated}"
    );
}

/// The other half of the #3946 witness: the test above builds its evidence by
/// hand, so it would still pass with every production call site reverted. This
/// asserts the wiring itself — that a ledger is folded and that reflection is
/// handed it, at every door that reflects.
///
/// Source-level for the reason `every_recalling_driver_arms_the_control_first`
/// (`memory/tests/ab_control.rs`) is: observing it otherwise means standing up a
/// provider, an MCP connect, a store and a renderer task to watch one
/// assignment. The names asserted on are real items, so a rename breaks the
/// compile too.
#[test]
fn the_reflecting_doors_fold_a_ledger_and_reflect_with_it() {
    const AGENT: &str = include_str!("../../agent.rs");
    const GOAL: &str = include_str!("../../agent/goal.rs");
    const REFLECT: &str = include_str!("../../agent/reflect.rs");
    const DECK: &str = include_str!("../../command_deck/authoring.rs");
    const FORWARDER: &str = include_str!("../../command_deck/forwarder.rs");
    const WRAPPED_ONE_SHOT: &str = include_str!("../../wrapper_plugin.rs");
    const WRAPPED_GOAL: &str = include_str!("../../agent/goal/goal_wrapped.rs");

    assert!(
        AGENT.contains("TurnFriction::from_events(&collected)"),
        "run_turn must fold the journal its renderer drained; without a producer \
         every digest's friction section is empty, which is the #3946 regression"
    );

    // The one-shot door — the surface a benchmark run drives.
    let door = GOAL
        .find("pub(crate) async fn run_raw_one_shot")
        .expect("the raw one-shot door must still be named this");
    let next_door = GOAL[door..]
        .find("pub async fn run_goal_cmd")
        .map_or(GOAL.len(), |offset| door + offset);
    assert!(
        GOAL[door..next_door].contains("TurnEvidence::with_rounds("),
        "run_raw_one_shot must reflect with the ledgers it folded, not \
         from_transcript's empty one — the slice constructor because its
         wrapped arm drives one turn per hold (#3976)"
    );

    // **The wrapped arms (#3976).** Both used to leave the slot at its default
    // and say so in a comment, so a `--pipeline` run's reflection saw no cost,
    // no wall clock, no retries and no loop firings — the #3946 regression
    // scoped to one path. The slice above is satisfiable by the raw arm alone,
    // which is why each wrapped producer is named here too.
    assert!(
        WRAPPED_ONE_SHOT.contains("self.friction.push(friction)"),
        "RawTurnDriver must fold each driven turn's ledger; it runs the turn \
         through the same `run_turn` every raw door does, so the journal is \
         reachable and leaving it unread is a choice rather than a limit"
    );
    assert!(
        WRAPPED_GOAL.contains("TurnFriction::per_goal_round(&rendered.events)"),
        "the wrapped `/goal` arm must split its journal at each round's own \
         verdict, exactly as the raw arm does"
    );

    // The interactive door — one ledger for a plain prompt, a slice of them
    // for `/goal`, so the shared helper takes the slice shape.
    assert!(
        REFLECT.contains("TurnEvidence::with_rounds(self.turn_slice(), self.friction"),
        "reflect_on_interactive_turn must reflect with the turn's ledger, over \
         the turn's own slice — the whole session made the lessons and the \
         self-review describe an execution they are not keyed to (#4382)"
    );
    assert!(
        DECK.contains("TurnEvidence::with_friction(turn,"),
        "and so must the deck's lead turn, which had the same mismatch: its \
         gate read the slice and its evidence read the session (#4382)"
    );

    // The `/goal` doors (#3962), both of which reflect ONCE over an arc of
    // several turns: the fold is per round, and the reflection takes the slice.
    assert!(
        GOAL.contains("TurnFriction::per_goal_round(&rendered.events)"),
        "run_goal_turn must split its journal at each round's own verdict; one \
         fold over the whole arc reports round 1's tools as the last round's"
    );
    assert!(
        GOAL.contains("TurnEvidence::with_rounds("),
        "the headless `stella goal` door must reflect with its per-round ledgers"
    );
    assert!(
        AGENT.contains("friction: &goal_rounds"),
        "the REPL's `/goal` handler must hand reflection the arc's per-round \
         ledgers, not the empty one it passed while this was unwired"
    );

    // The Command Deck, which #3946 did not touch at all.
    assert!(
        DECK.contains("TurnEvidence::with_friction("),
        "the deck's lead turn must reflect with the ledger its lane forwarder \
         folded, not a transcript-only digest"
    );
    assert!(
        FORWARDER.contains("friction.observe(&event)"),
        "the lane forwarder is where a deck turn's ledger is folded — it is the \
         one seam every lane shares"
    );
}

/// The event-derived half of the evidence (#2460's definition of done, item 1):
/// cost, wall clock, retries and loop firings leave no message at all, so no
/// window over the transcript can reach them.
#[tokio::test]
async fn the_prompt_names_where_the_turn_spent_itself() {
    let mut friction = super::digest::TurnFriction::default();
    friction.observe(&stella_protocol::AgentEvent::StepUsage {
        upstream_provider: None,
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
        sub_agent_id: None,
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
        friction: std::slice::from_ref(&friction),
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
