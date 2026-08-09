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

use std::sync::Mutex;

use async_trait::async_trait;
use stella_protocol::{
    CompletionMessage, CompletionRequestRef, CompletionResult, CompletionUsage, MessageRole,
    Provider, ProviderError, ReasoningEffort,
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
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".stella")).expect("workspace");
    let provider = CapturingProvider::default();
    let transcript = vec![
        CompletionMessage::user("fix the leak"),
        CompletionMessage::assistant("swapped db() for withTenantDb"),
    ];
    super::reflect_on_turn(
        &provider,
        "capturing",
        dir.path(),
        &transcript,
        &["testing".to_string()],
        succeeded,
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
    assert!(
        super::MAX_LESSONS_PER_TURN > 3,
        "the cap is back below what a turn teaches, which makes it the decision \
         again rather than a backstop"
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
        &transcript,
        &["testing".to_string()],
        true,
        None,
        posture,
    )
    .await
    .expect("the stub provider cannot fail");
    let shape = *provider.shape.lock().expect("shape lock");
    let reasoning = *provider.reasoning.lock().expect("reasoning lock");
    (shape, reasoning)
}
