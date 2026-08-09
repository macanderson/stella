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
