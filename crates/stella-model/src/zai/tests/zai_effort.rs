// SPDX-License-Identifier: AGPL-3.0-only
//! Z.ai reasoning-effort mapping: the enum-to-`reasoning_effort` table and the
//! wire body it produces alongside GLM's `thinking` on/off object. Split out of
//! the parent `tests.rs` (file-size gate), mirroring `openrouter_effort`.

use super::*;
use wiremock::MockServer;

/// Z.ai carries reasoning DEPTH in `reasoning_effort`, alongside the `thinking`
/// on/off object.
///
/// This test replaces an earlier one asserting the exact opposite — that the
/// z.ai identity must never send the field, on the stated grounds that "effort
/// has no dial there". That premise was wrong, and the cost of it was
/// measurable: probed live against `glm-5.2` on the coding-plan endpoint
/// (2026-08-04) the parameter is accepted and honoured, with `minimal` driving
/// `reasoning_tokens` to 0. Until then a pinned effort was validated, hashed
/// into the published benchmark posture, and then never put on the wire, so a
/// benchmark arm pinned to `medium` actually ran at GLM's own deeper default
/// and spent ~2x the tokens of an OpenRouter arm configured identically.
///
/// The byte-stability half of the old contract is deliberately kept: a caller
/// who pinned no effort still sends no field, so this cannot silently re-pin
/// anyone who was relying on the model's own default depth.
#[tokio::test]
async fn zai_identity_maps_pinned_effort_to_reasoning_effort() {
    let server = MockServer::start().await;
    mock_ok(&server).await;

    let provider =
        ZaiProvider::new(ApiKey::new("sk-test-zai"), "glm-5.2").with_base_url(server.uri());
    let req = |reasoning, effort| CompletionRequest {
        messages: vec![CompletionMessage::user("hi")],
        max_output_tokens: None,
        temperature: None,
        effort,
        tools: vec![],
        reasoning,
        params: None,
    };

    provider
        .complete(req(None, Some(ReasoningEffort::Low)))
        .await
        .expect("low");
    provider
        .complete(req(Some(true), Some(ReasoningEffort::Medium)))
        .await
        .expect("medium");
    // `xhigh`/`max` collapse to GLM's top rung rather than inventing one.
    provider
        .complete(req(None, Some(ReasoningEffort::Max)))
        .await
        .expect("max");
    // Explicit off wins over a pinned effort: `thinking:{disabled}` is the
    // off switch, and a second differently-spelled one would contradict it.
    provider
        .complete(req(Some(false), Some(ReasoningEffort::High)))
        .await
        .expect("off");
    // Byte-stability: reasoning on but NO effort pinned keeps the field off the
    // wire. Unlike xAI, `thinking` already said "on", so inventing a middle
    // tier here would change what every pre-existing z.ai caller sends.
    provider.complete(req(Some(true), None)).await.expect("on");
    // Neither expressed → nothing, as before.
    provider.complete(req(None, None)).await.expect("bare");

    let requests = server.received_requests().await.expect("recorded requests");
    let bodies: Vec<String> = requests
        .iter()
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .collect();
    assert!(
        bodies[0].contains("\"reasoning_effort\":\"low\""),
        "{}",
        bodies[0]
    );
    assert!(
        bodies[1].contains("\"reasoning_effort\":\"medium\""),
        "{}",
        bodies[1]
    );
    assert!(
        bodies[2].contains("\"reasoning_effort\":\"high\""),
        "{}",
        bodies[2]
    );
    assert!(!bodies[3].contains("reasoning_effort"), "{}", bodies[3]);
    assert!(!bodies[4].contains("reasoning_effort"), "{}", bodies[4]);
    assert!(!bodies[5].contains("reasoning_effort"), "{}", bodies[5]);

    // Depth rides alongside the on/off object; it does not replace it, and the
    // OpenRouter-shaped `reasoning` object still never appears here.
    assert!(
        bodies[1].contains("\"thinking\":{\"type\":\"enabled\"}"),
        "{}",
        bodies[1]
    );
    assert!(
        bodies[3].contains("\"thinking\":{\"type\":\"disabled\"}"),
        "{}",
        bodies[3]
    );
    assert!(!bodies[0].contains("\"reasoning\":"), "{}", bodies[0]);
}

/// `minimal` is reachable only by turning reasoning off, never by asking for a
/// cheap tier. It is an OFF switch wearing a tier's name (measured: 0 reasoning
/// tokens), so mapping `Low` onto it would stop a caller who asked for shallow
/// thinking from thinking at all.
#[test]
fn map_zai_effort_never_yields_minimal() {
    for effort in [
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
        ReasoningEffort::Xhigh,
        ReasoningEffort::Max,
    ] {
        assert_ne!(map_zai_effort(effort), "minimal", "{effort:?}");
    }
    assert_eq!(map_zai_effort(ReasoningEffort::Low), "low");
    assert_eq!(map_zai_effort(ReasoningEffort::Medium), "medium");
    assert_eq!(map_zai_effort(ReasoningEffort::Xhigh), "high");
    assert_eq!(map_zai_effort(ReasoningEffort::Max), "high");
}

/// The output allowance an un-capped reasoning turn gets, and the parity that
/// makes it correct: `anthropic.rs` picks 32k for a thinking model and this
/// adapter must not disagree. Reasoning spends from the same allowance as the
/// answer, so a provider's own non-thinking-sized default truncates the turn
/// mid-thought — and on this dialect that lands as a `finish_reason: length`
/// step carrying chain-of-thought and no tool call, the exact shape that ended
/// 30 of 30 cap-hit steps in the 2026-07-31 Terminal-Bench bundle.
#[test]
fn an_uncapped_reasoning_turn_gets_thinking_headroom() {
    assert_eq!(reasoning_aware_max_tokens(None, Some(true)), Some(32_000));
}

/// A caller who pinned a cap keeps it verbatim — the default only fills the
/// hole where no preference was expressed. Overriding an explicit ceiling
/// would silently spend a caller's money on a budget they declined.
#[test]
fn a_pinned_cap_is_honored_whatever_the_reasoning_setting() {
    for reasoning in [None, Some(false), Some(true)] {
        assert_eq!(
            reasoning_aware_max_tokens(Some(8_192), reasoning),
            Some(8_192),
            "an explicit cap survives reasoning={reasoning:?}"
        );
    }
}

/// Reasoning off (or unstated) leaves the field absent, so a request without
/// a cap serializes byte-identical to what this adapter has always sent —
/// the prompt-cache stability contract the params work established.
#[test]
fn a_non_reasoning_turn_still_sends_no_cap() {
    assert_eq!(reasoning_aware_max_tokens(None, Some(false)), None);
    assert_eq!(reasoning_aware_max_tokens(None, None), None);
}
