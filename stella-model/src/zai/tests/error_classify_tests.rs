//! Provider HTTP error-classification tests (issues #271 / #250), split
//! out of `zai/tests.rs` to keep that file under the 1500-line ratchet cap.
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
