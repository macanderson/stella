//! Provider HTTP error-classification tests (issues #271 / #250), split out
//! of `zai/tests.rs` to keep that file navigable — the status ladder
//! (401/402/403/429/5xx/invalid-model) is one coherent subject and reads
//! better on its own than buried among the dialect and streaming tests.
//! `use super::*;` re-exports the parent test module's helpers and imports.

use super::*;

fn hi_request() -> CompletionRequest {
    CompletionRequest {
        messages: vec![CompletionMessage::user("hi")],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    }
}

/// The live repro behind issue #271, reproduced through the public
/// `complete()` API exactly as it happens for real: a mistyped/decommissioned
/// model slug gets OpenRouter's `HTTP 400 "<slug> is not a valid model ID"`.
/// Must stay non-retryable (a 400 falls to `classify_http_status`'s `Terminal`
/// arm, which `stella-core::retry::retry_with_backoff` excludes) AND carry a
/// recovery hint.
#[tokio::test]
async fn complete_maps_openrouter_400_invalid_model_to_a_terminal_error_with_a_recovery_hint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            r#"{"error":{"message":"openrouter/auto is not a valid model ID","code":400}}"#,
        ))
        .mount(&server)
        .await;

    let provider = ZaiProvider::new(ApiKey::new("sk-or-test"), "openrouter/auto")
        .with_base_url(server.uri())
        .with_identity("openrouter", "OpenRouter");

    let err = provider.complete(hi_request()).await.unwrap_err();
    assert!(!err.is_retryable(), "a 400 must never be retried: {err:?}");
    assert!(matches!(err, ProviderError::Terminal(_)));
    let msg = err.to_string();
    assert!(msg.contains("is not a valid model ID"), "{msg}");
    assert!(msg.contains("SETTINGS tab"), "{msg}");
    assert!(msg.contains("--model provider/slug"), "{msg}");
}

/// Issue #250: a revoked/mistyped OpenRouter key. The message must carry the
/// provider's own reason — without it, a 401 and the 403 test below read as
/// the byte-identical "rejected the credential", giving no way to tell a bad
/// key from a valid key the account just can't use yet.
#[tokio::test]
async fn complete_maps_openrouter_401_to_auth_error_with_the_provider_reason() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_string(r#"{"error":{"message":"No auth credentials found"}}"#),
        )
        .mount(&server)
        .await;

    let provider = ZaiProvider::new(ApiKey::new("sk-or-bad"), "openrouter/auto")
        .with_base_url(server.uri())
        .with_identity("openrouter", "OpenRouter");

    let err = provider.complete(hi_request()).await.unwrap_err();
    assert!(!err.is_retryable());
    assert!(matches!(err, ProviderError::Auth(_)));
    let msg = err.to_string();
    assert!(msg.contains("No auth credentials found"), "{msg}");
    assert!(msg.contains("SETTINGS tab"), "{msg}");
}

/// Issue #250: a VALID OpenRouter key whose account hasn't enabled the
/// requested model — a 403, not a 401, and a different fix (switch models)
/// than a bad key (replace the key). Must read as distinct from the 401
/// case above, not the same "credentials failed" text.
#[tokio::test]
async fn complete_maps_openrouter_403_model_not_enabled_to_a_distinct_hint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(403).set_body_string(
            r#"{"error":{"message":"openrouter/auto is not enabled for this account"}}"#,
        ))
        .mount(&server)
        .await;

    let provider = ZaiProvider::new(ApiKey::new("sk-or-test"), "openrouter/auto")
        .with_base_url(server.uri())
        .with_identity("openrouter", "OpenRouter");

    let err = provider.complete(hi_request()).await.unwrap_err();
    assert!(!err.is_retryable());
    assert!(matches!(err, ProviderError::Auth(_)));
    let msg = err.to_string();
    assert!(msg.contains("is not enabled for this account"), "{msg}");
    assert!(msg.contains("--model provider/slug"), "{msg}");
    assert!(
        !msg.contains("revoked"),
        "{msg}: must not read like a bad-key (401) message"
    );
}

/// Issue #250: some gateways answer out-of-credits with HTTP 402 rather than
/// folding it into a 403. The 402 must get dedicated billing wording rather
/// than falling into the generic `Terminal` bucket, and stay non-retryable.
/// The mock expects exactly one request: a 402 whose body carries no afford
/// hint must also never trigger the afford-capped retry below.
#[tokio::test]
async fn complete_maps_openrouter_402_to_a_terminal_billing_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(402)
                .set_body_string(r#"{"error":{"message":"account balance depleted"}}"#),
        )
        .expect(1)
        .mount(&server)
        .await;

    let provider = ZaiProvider::new(ApiKey::new("sk-or-test"), "openrouter/auto")
        .with_base_url(server.uri())
        .with_identity("openrouter", "OpenRouter");

    let err = provider.complete(hi_request()).await.unwrap_err();
    assert!(!err.is_retryable());
    assert!(matches!(err, ProviderError::Terminal(_)));
    let msg = err.to_string();
    assert!(msg.contains("payment required"), "{msg}");
    assert!(msg.contains("out of credits"), "{msg}");
    assert!(msg.contains("account balance depleted"), "{msg}");
}

/// The shape that killed three bench runs whole (h2h891, fivetools5,
/// gate89high1): OpenRouter answers a request whose `max_tokens` ceiling the
/// credit balance cannot cover with HTTP 402 naming the ceiling it *can*
/// serve — "You requested up to 128000 tokens, but can only afford 47365" —
/// and treating that as terminal aborts a trial the balance could still have
/// funded. The adapter must retry the same request ONCE with `max_tokens`
/// reduced to 90% of the hint (47365 → 42628); the success mock here matches
/// only a body carrying that reduced ceiling, so a retry without the
/// reduction — or no retry at all, the pre-fix behavior — fails this test.
#[tokio::test]
async fn complete_retries_an_afford_hinted_402_with_reduced_max_tokens() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(402).set_body_string(
            r#"{"error":{"message":"This request requires more credits, or fewer max_tokens. You requested up to 128000 tokens, but can only afford 47365."}}"#,
        ))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    let sse_body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{}}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":1}}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains("\"max_tokens\":42628"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .expect(1)
        .mount(&server)
        .await;

    let provider = ZaiProvider::new(ApiKey::new("sk-or-test"), "openrouter/auto")
        .with_base_url(server.uri())
        .with_identity("openrouter", "OpenRouter");

    let mut req = hi_request();
    req.max_output_tokens = Some(128_000);
    let result = provider
        .complete(req)
        .await
        .expect("the afford-hinted 402 must be retried at the reduced ceiling");
    assert_eq!(result.text, "ok");
}

/// The floor half of the afford retry: a hint below
/// `AFFORD_RETRY_FLOOR_TOKENS` names a ceiling too small to fit an agent
/// turn, so the rejection stays the terminal billing abort — the mock
/// expects exactly one request, proving no doomed retry went out.
#[tokio::test]
async fn a_402_afford_hint_below_the_floor_stays_terminal() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(402).set_body_string(
            r#"{"error":{"message":"You requested up to 128000 tokens, but can only afford 4096."}}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let provider = ZaiProvider::new(ApiKey::new("sk-or-test"), "openrouter/auto")
        .with_base_url(server.uri())
        .with_identity("openrouter", "OpenRouter");

    let err = provider.complete(hi_request()).await.unwrap_err();
    assert!(!err.is_retryable());
    assert!(matches!(err, ProviderError::Terminal(_)));
    assert!(err.to_string().contains("payment required"), "{err}");
}

/// The parser behind the one 402 retry, pinned at its edges: the real
/// classifier-wrapped message parses to 90% of the hint, the floor is
/// inclusive, and every malformed shape — no hint, no digits, an absurd
/// count — degrades to `None`, which is the ordinary terminal abort.
#[test]
fn afford_hint_parsing_is_defensive() {
    let real = "OpenRouter rejected the request (HTTP 402: payment required): This request \
                requires more credits, or fewer max_tokens. You requested up to 128000 \
                tokens, but can only afford 47365. — the account is out of credits";
    assert_eq!(afford_capped_max_tokens(real), Some(42_628));
    // The floor is inclusive: exactly 8192 affordable still retries...
    assert_eq!(
        afford_capped_max_tokens("can only afford 8192"),
        Some(7_372)
    );
    // ...and one token under it does not.
    assert_eq!(afford_capped_max_tokens("can only afford 8191"), None);
    // No hint at all (the existing dedicated-billing-wording shape).
    assert_eq!(afford_capped_max_tokens("account balance depleted"), None);
    // The phrase with no number after it.
    assert_eq!(
        afford_capped_max_tokens("can only afford more credits"),
        None
    );
    // A count that cannot be a token ceiling.
    assert_eq!(
        afford_capped_max_tokens("can only afford 99999999999999999999999999 tokens"),
        None
    );
}

/// End-to-end funnel proof for the shared chat-completions adapter (#2680):
/// an OpenAI-dialect `context_length_exceeded` 400 must reach the caller as
/// `ProviderError::ContextOverflow` — the class the engine's reactive
/// overflow recovery keys on — through the public `complete()` API, not just
/// through `classify_http_status` in isolation. Non-retryable: the recovery
/// re-issues only after compacting, never verbatim.
#[tokio::test]
async fn complete_maps_a_context_length_exceeded_400_to_context_overflow() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            r#"{"error":{"message":"This model's maximum context length is 131072 tokens.","type":"invalid_request_error","code":"context_length_exceeded"}}"#,
        ))
        .mount(&server)
        .await;

    let provider = ZaiProvider::new(ApiKey::new("sk-or-test"), "openrouter/auto")
        .with_base_url(server.uri())
        .with_identity("openrouter", "OpenRouter");

    let err = provider.complete(hi_request()).await.unwrap_err();
    assert!(
        matches!(err, ProviderError::ContextOverflow { .. }),
        "overflow must classify distinctly, got {err:?}"
    );
    assert!(!err.is_retryable(), "verbatim re-issue rejects identically");
    let msg = err.to_string();
    assert!(msg.contains("context window"), "{msg}");
}

/// The 429 pre-check runs ahead of the shared ladder, so it is the one arm
/// that could still hard-code "Z.ai" while every other status reports under
/// the re-identified provider's label. It did — an OpenRouter throttle read
/// "Z.ai HTTP 429", the exact wrong-credential misdirection
/// [`ZaiProvider::with_identity`] exists to prevent. Asserted on both
/// branches of the split, since they format the message independently.
#[test]
fn a_429_reports_under_the_re_identified_provider_never_z_ai() {
    let throttled = r#"{"error":{"message":"rate limit exceeded"}}"#;
    let exhausted = r#"{"error":{"message":"insufficient balance"}}"#;

    let throttle = classify_zai_429("OpenRouter", throttled, Some(1_000));
    assert!(matches!(throttle, ProviderError::RateLimited { .. }));
    let msg = throttle.to_string();
    assert!(msg.contains("OpenRouter"), "{msg}");
    assert!(!msg.contains("Z.ai"), "{msg}");

    let billing = classify_zai_429("DeepSeek", exhausted, None);
    assert!(matches!(billing, ProviderError::Terminal(_)));
    let msg = billing.to_string();
    assert!(msg.contains("DeepSeek"), "{msg}");
    assert!(!msg.contains("Z.ai"), "{msg}");

    // The default identity is unchanged: Z.ai still reports as Z.ai.
    let native = classify_zai_429("Z.ai", "too many requests", None);
    assert!(native.to_string().contains("Z.ai HTTP 429"), "{native}");
}
