//! OpenRouter reasoning-effort mapping: the enum-to-`reasoning.effort`
//! ladder and the request object it produces. Split out of the parent
//! `tests.rs` (file-size gate): the pure-function assertions over the
//! mapping, plus the wire body they produce through a mock gateway.

use super::*;
use wiremock::MockServer;

/// The top two effort tiers must reach the wire as themselves. This used to
/// collapse to `high`, which made a pinned `xhigh` indistinguishable from
/// asking for `high` — silently, with nothing anywhere saying so. OpenRouter
/// documents the full ladder and normalizes each tier onto the routed model,
/// so the distinction is real and must survive the mapping.
#[test]
fn openrouter_effort_keeps_the_top_tiers_distinct() {
    use stella_protocol::ReasoningEffort;
    assert_eq!(map_openrouter_effort(ReasoningEffort::Low), "low");
    assert_eq!(map_openrouter_effort(ReasoningEffort::Medium), "medium");
    assert_eq!(map_openrouter_effort(ReasoningEffort::High), "high");
    assert_eq!(map_openrouter_effort(ReasoningEffort::Xhigh), "xhigh");
    assert_eq!(map_openrouter_effort(ReasoningEffort::Max), "max");

    // Every tier must map to something distinct, or the enum is lying about
    // offering five levels.
    let mapped: std::collections::BTreeSet<_> = [
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
        ReasoningEffort::Xhigh,
        ReasoningEffort::Max,
    ]
    .into_iter()
    .map(map_openrouter_effort)
    .collect();
    assert_eq!(
        mapped.len(),
        5,
        "efforts collapsed onto each other: {mapped:?}"
    );
}

/// The default OpenRouter posture is effort-pinned with thinking on, so the
/// body it produces is worth pinning too: an `effort` alone (never paired
/// with a redundant `enabled`, which the gateway treats as contradictory).
#[test]
fn openrouter_reasoning_body_for_the_default_posture() {
    use stella_protocol::ReasoningEffort;
    let reasoning = openrouter_reasoning(Some(true), Some(ReasoningEffort::Xhigh))
        .expect("an effort-pinned turn sends a reasoning object");
    assert_eq!(reasoning.effort, Some("xhigh"));
    assert_eq!(reasoning.enabled, None);
}

#[tokio::test]
async fn openrouter_identity_maps_reasoning_to_the_gateway_object() {
    let server = MockServer::start().await;
    mock_ok(&server).await;

    let provider = ZaiProvider::new(ApiKey::new("sk-or-test"), "openrouter/auto")
        .with_base_url(server.uri())
        .with_identity("openrouter", "OpenRouter");
    let req = |reasoning, effort| CompletionRequest {
        messages: vec![CompletionMessage::user("hi")],
        max_output_tokens: None,
        temperature: None,
        effort,
        tools: vec![],
        reasoning,
        params: None,
    };
    // Pinned effort (reasoning not suppressed) → {"effort": …}.
    provider
        .complete(req(None, Some(ReasoningEffort::Xhigh)))
        .await
        .expect("effort");
    // Explicit off wins even over a pinned effort → {"enabled": false}.
    provider
        .complete(req(Some(false), Some(ReasoningEffort::High)))
        .await
        .expect("off");
    // Bare on with no effort → {"enabled": true}.
    provider.complete(req(Some(true), None)).await.expect("on");

    let requests = server.received_requests().await.expect("recorded requests");
    let bodies: Vec<String> = requests
        .iter()
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .collect();
    // Xhigh reaches the wire AS xhigh. This assertion used to expect
    // "high" — the gateway modelled only three tiers when it was written,
    // and the collapse silently erased the distinction the caller asked for.
    assert!(
        bodies[0].contains("\"reasoning\":{\"effort\":\"xhigh\"}"),
        "{}",
        bodies[0]
    );
    assert!(
        bodies[1].contains("\"reasoning\":{\"enabled\":false}"),
        "{}",
        bodies[1]
    );
    assert!(
        bodies[2].contains("\"reasoning\":{\"enabled\":true}"),
        "{}",
        bodies[2]
    );
    // GLM's thinking object is Z.ai-only; OpenRouter never sees it.
    assert!(!bodies[0].contains("thinking"), "{}", bodies[0]);
}
