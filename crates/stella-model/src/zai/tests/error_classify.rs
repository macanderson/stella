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
#[tokio::test]
async fn complete_maps_openrouter_402_to_a_terminal_billing_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(402)
                .set_body_string(r#"{"error":{"message":"account balance depleted"}}"#),
        )
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

/// The third 402 (#4380), replayed verbatim off the session that died of it:
/// the balance is committed to the caller's *own* concurrent calls, and the
/// provider says to wait. Terminal, it aborted a turn with 46 model calls and
/// four edits in flight and told the user the account was out of credit —
/// while the same key funded $0.10 of calls two minutes later. It must reach
/// the caller retryable so the ladder re-issues, and it must not claim the
/// account is out of credits.
#[tokio::test]
async fn complete_maps_an_openrouter_in_flight_402_to_a_retryable_rate_limit() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(402).set_body_string(
            r#"{"error":{"message":"This request would exceed your available credits given your current in-flight requests. Retry after in-flight requests settle, or add credits.","code":402}}"#,
        ))
        .mount(&server)
        .await;

    let provider = ZaiProvider::new(ApiKey::new("sk-or-test"), "moonshotai/kimi-k3")
        .with_base_url(server.uri())
        .with_identity("openrouter", "OpenRouter");

    let err = provider.complete(hi_request()).await.unwrap_err();
    assert!(
        err.is_retryable(),
        "the provider itself says to retry: {err:?}"
    );
    assert!(matches!(err, ProviderError::RateLimited { .. }), "{err:?}");
    let msg = err.to_string();
    assert!(msg.contains("in-flight"), "{msg}");
    assert!(
        !msg.contains("out of credits"),
        "{msg}: the provider did not say that"
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

/// #3859's witness on this dialect: a `code: 529` frame arriving *inside* an
/// open stream, after a usage frame already reported what the prompt cost.
///
/// Folded into the combined transport arm the brownout could not park, so a
/// sustained one burned the ~16s inline ladder and aborted a turn with
/// wall-clock budget left. Classified as an `Overloaded` that carries no
/// accounting, the input tokens the gateway had already reported died at the
/// `?`. Which side of the response boundary the gateway shed load on is
/// invisible to the user and must decide neither, so the test pins both.
#[tokio::test]
async fn a_mid_stream_529_frame_parks_and_keeps_the_usage_the_stream_reported() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{}}],\"usage\":{\"prompt_tokens\":61000,\"completion_tokens\":2,\"prompt_tokens_details\":{\"cached_tokens\":58000}}}\n\n",
        "data: {\"error\":{\"message\":\"Provider is overloaded\",\"code\":529}}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider =
        ZaiProvider::new(ApiKey::new("sk-test-zai"), "glm-5.2").with_base_url(server.uri());
    let err = provider.complete(hi_request()).await.unwrap_err();

    assert!(matches!(err, ProviderError::Overloaded { .. }), "{err:?}");
    assert!(
        err.is_park_eligible(),
        "an in-band brownout is the same condition waiting fixes: {err:?}"
    );

    let partial = err
        .partial_usage()
        .expect("the usage frame that already arrived must survive the error");
    assert_eq!(partial.usage.input_tokens, 61_000);
    assert_eq!(partial.usage.cached_input_tokens, 58_000);
    assert!(partial.input_reported);
}
