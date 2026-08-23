//! Unit tests for the shared HTTP plumbing: the non-success classification
//! ladder, the `Retry-After` parser, the body/reason snippets, and the
//! bounded stream reads. Split from the parent `http.rs` (file-size gate).

use super::*;

fn pricing() -> Pricing {
    Pricing {
        input_usd_per_mtok: 3.0,
        output_usd_per_mtok: 15.0,
        cached_input_usd_per_mtok: 0.3,
        cache_write_usd_per_mtok: 3.75,
    }
}

/// A stream cut before anything arrived has nothing to report, and must
/// say so. A zeroed envelope here is indistinguishable downstream from a
/// real call that cost nothing — the misreading this whole path exists to
/// prevent. This is the normal early-drop case on the OpenAI-shaped
/// dialects, which send usage only in the final chunk.
#[test]
fn nothing_observed_yields_no_partial_at_all() {
    assert!(partial_usage(&CompletionUsage::default(), "", Some(&pricing())).is_none());
}

/// Text without a usage frame still counts: those tokens were generated
/// and are billable, so an estimate beats a zero we know to be wrong.
#[test]
fn streamed_text_alone_is_enough_to_report_an_estimate() {
    let partial = partial_usage(&CompletionUsage::default(), "hello there", Some(&pricing()))
        .expect("text that arrived is recoverable");
    assert!(partial.usage.output_tokens > 0);
    assert!(
        !partial.input_reported,
        "no input frame arrived, so the input side is not the provider's"
    );
    assert!(!partial.usage.is_complete());
}

/// The Anthropic-shaped case: `message_start` already reported the input
/// side, so it is the provider's own figure and is priced as such.
#[test]
fn a_reported_input_frame_is_marked_as_the_providers_own() {
    let usage = CompletionUsage {
        input_tokens: 14_000,
        cached_input_tokens: 12_000,
        // Mid-flight `message_delta` can set this before the stream dies.
        reported: true,
        ..Default::default()
    };
    let partial =
        partial_usage(&usage, "Hello", Some(&pricing())).expect("reported input is recoverable");
    assert!(partial.input_reported);
    assert_eq!(partial.usage.input_tokens, 14_000);
    // 2k uncached @ $3 + 12k cached @ $0.30, plus the estimated output at
    // $15 — the cache split is the point: billing all 14k at the input
    // rate would be $0.042, over four times the truth.
    let expected = 0.006 + 0.0036 + (partial.usage.output_tokens as f64 / 1_000_000.0) * 15.0;
    assert!(
        (partial.cost_usd - expected).abs() < 1e-9,
        "cache split must be priced: got {}, expected {expected}",
        partial.cost_usd
    );
    assert!(
        !partial.usage.is_complete(),
        "a mid-flight `reported` flag must never survive into a partial"
    );
}

/// `attach_partial` must leave an error alone when there is nothing to
/// hang on it, rather than decorating it with an empty envelope.
#[test]
fn attach_partial_is_a_no_op_when_nothing_was_observed() {
    let err = attach_partial(
        ProviderError::transport("connect refused"),
        &CompletionUsage::default(),
        "",
        Some(&pricing()),
    );
    assert!(err.partial_usage().is_none());
}

#[test]
fn client_builds_without_panicking() {
    // Smoke test: the connect-timeout client constructs on this platform.
    let _ = client();
}

/// The reference epoch every date case below is stated against:
/// `Sun, 06 Nov 1994 08:49:37 GMT`, RFC 9110 §5.6.7's own example.
const RFC_EXAMPLE_EPOCH: i64 = 784_111_777;

#[test]
fn delta_seconds_is_still_the_common_form() {
    assert_eq!(retry_after_ms_at("120", 0), Some(120_000));
    assert_eq!(retry_after_ms_at("  30 ", 0), Some(30_000));
    assert_eq!(retry_after_ms_at("0", 0), Some(0));
}

/// Issue #940: the HTTP-date form was ignored, so a provider fronted by a
/// CDN — the deployments that actually rate-limit — had its stated window
/// dropped and the driver retried back inside it on computed backoff.
#[test]
fn an_http_date_becomes_the_remaining_wait() {
    assert_eq!(
        imf_fixdate_epoch_secs("Sun, 06 Nov 1994 08:49:37 GMT"),
        Some(RFC_EXAMPLE_EPOCH),
        "the RFC's own example must land on its own epoch second"
    );
    assert_eq!(
        retry_after_ms_at("Sun, 06 Nov 1994 08:49:37 GMT", RFC_EXAMPLE_EPOCH - 90),
        Some(90_000),
        "the hint is the wait remaining, not the timestamp"
    );
}

/// A window that has already closed means "retry now" — expressed as a
/// zero hint (which the retry policy floors at its own base delay), never
/// as a negative wait and never as a dropped hint.
#[test]
fn an_elapsed_http_date_is_zero_not_negative_and_not_dropped() {
    assert_eq!(
        retry_after_ms_at("Sun, 06 Nov 1994 08:49:37 GMT", RFC_EXAMPLE_EPOCH + 600),
        Some(0)
    );
}

#[test]
fn the_calendar_arithmetic_handles_leap_years_and_epoch_boundaries() {
    for (header, epoch) in [
        ("Thu, 01 Jan 1970 00:00:00 GMT", 0),
        ("Wed, 31 Dec 1969 23:59:59 GMT", -1),
        // 2000 is a leap year (divisible by 400); 1900 was not.
        ("Tue, 29 Feb 2000 12:00:00 GMT", 951_825_600),
        ("Mon, 01 Mar 2100 00:00:00 GMT", 4_107_542_400),
        // A leap second collapses onto the same POSIX second as :59.
        ("Sat, 31 Dec 2016 23:59:60 GMT", 1_483_228_799),
    ] {
        assert_eq!(imf_fixdate_epoch_secs(header), Some(epoch), "{header}");
    }
}

/// Strictness is the point: a header we cannot read exactly is one whose
/// meaning we would be guessing at, and `None` only costs the fallback to
/// computed backoff. The two obsolete RFC 9110 §5.6.7 formats are among
/// these deliberately.
#[test]
fn a_malformed_or_obsolete_date_yields_no_hint_rather_than_a_wrong_one() {
    for header in [
        "",
        "soon",
        "-30",
        "1.5",
        "Sun, 06 Nov 1994 08:49:37 PST", // only GMT is in the grammar
        "Sun, 06 Nov 1994 08:49:37 GMT extra", // trailing junk
        "Sun, 06 Nov 1994 08:49 GMT",    // no seconds
        "Sun, 06 Xyz 1994 08:49:37 GMT", // not a month
        "Sun, 06 Nov 1994 25:49:37 GMT", // hour out of range
        "Sun, 32 Nov 1994 08:49:37 GMT", // day out of range
        "Sunday, 06-Nov-94 08:49:37 GMT", // obsolete RFC 850 form
        "Sun Nov  6 08:49:37 1994",      // obsolete asctime form
    ] {
        assert_eq!(retry_after_ms_at(header, 0), None, "{header:?}");
    }
}

#[test]
fn the_header_map_path_reads_both_forms() {
    let mut headers = reqwest::header::HeaderMap::new();
    assert_eq!(parse_retry_after_ms(&headers), None);
    headers.insert(reqwest::header::RETRY_AFTER, "45".parse().unwrap());
    assert_eq!(parse_retry_after_ms(&headers), Some(45_000));
    // Long past, so the answer is deterministic against the real clock.
    headers.insert(
        reqwest::header::RETRY_AFTER,
        "Sun, 06 Nov 1994 08:49:37 GMT".parse().unwrap(),
    );
    assert_eq!(parse_retry_after_ms(&headers), Some(0));
}

#[test]
fn the_unary_read_bound_covers_a_whole_generation_not_a_gap_between_chunks() {
    let _ = unary_client();
    // The property that makes `unary_client` worth having. A unary caller
    // has no first token to reset the read clock, so its bound has to
    // cover the entire generation; if it ever slipped to or below the
    // stream-idle bound, Bedrock would be back to failing every
    // completion slower than two minutes as a *retryable* Transport and
    // paying for all four attempts (#547).
    //
    // reqwest exposes no getter for a built client's timeouts, so this
    // pins the relationship between the two consts the clients are built
    // from. A wire-level witness would have to out-wait
    // STREAM_IDLE_TIMEOUT itself — two minutes of wall clock per run.
    assert!(
        UNARY_READ_TIMEOUT > STREAM_IDLE_TIMEOUT,
        "a unary read bound must exceed the per-chunk stream bound: \
         {UNARY_READ_TIMEOUT:?} vs {STREAM_IDLE_TIMEOUT:?}"
    );
}

/// The other half of #547. Raising the bound to 600s only widened the
/// window; what multiplied it by four was the classification. A unary read
/// timeout consumed the whole generation, so re-issuing the identical
/// request just waits again — it must not be retryable.
#[tokio::test]
async fn a_unary_read_timeout_is_terminal_not_retryable_transport() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        // Accept, then hold the connection without writing a byte — the
        // "LB accepts and black-holes" shape `UNARY_READ_TIMEOUT` exists
        // for, compressed so the test costs milliseconds instead of 600s.
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
        .mount(&server)
        .await;

    let client = reqwest::Client::builder()
        .read_timeout(Duration::from_millis(50))
        .build()
        .expect("client builds");
    let error = client
        .post(server.uri())
        .send()
        .await
        .expect_err("the stalled response must time out");
    assert!(error.is_timeout(), "expected a timeout, got {error}");

    let classified = classify_unary_dispatch_error("Bedrock", &error);
    assert!(
        matches!(classified, ProviderError::Terminal(_)),
        "a read timeout must be Terminal, got {classified:?}"
    );
    assert!(
        !classified.is_retryable(),
        "a retryable read timeout costs four full attempts, not one: {classified:?}"
    );
}

/// The converse case, so the fix above cannot over-reach: a *connect*
/// failure never reached the model and nothing was generated, so it stays
/// retryable.
#[tokio::test]
async fn a_connect_failure_stays_retryable_transport() {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_millis(50))
        .build()
        .expect("client builds");
    // Port 1 on loopback refuses immediately.
    let error = client
        .post("http://127.0.0.1:1/")
        .send()
        .await
        .expect_err("connection must fail");

    let classified = classify_unary_dispatch_error("Bedrock", &error);
    assert!(
        classified.is_retryable(),
        "a connect failure generated nothing and must stay retryable: {classified:?}"
    );
}

#[tokio::test]
async fn next_stream_read_reports_a_stalled_stream_as_idle_not_a_failure() {
    // A stream that never yields must time out, not hang — and report the
    // stall as `Idle`, which is the bit the fallback classifies on: a stall
    // before the first byte is a broken streaming path, while a transport
    // failure says nothing about streaming specifically. `pending()` models a
    // connection that opened and then went silent.
    let mut stalled = futures_util::stream::pending::<reqwest::Result<Vec<u8>>>();
    let read = next_stream_read(&mut stalled, Duration::from_millis(20)).await;
    assert!(
        matches!(read, StreamRead::Idle),
        "a stalled stream must report Idle, never Failed"
    );
}

#[tokio::test]
async fn next_stream_read_passes_through_a_ready_item_and_end_of_stream() {
    let mut ready = futures_util::stream::iter(vec![Ok::<_, reqwest::Error>(vec![1u8, 2, 3])]);
    match next_stream_read(&mut ready, Duration::from_millis(50)).await {
        StreamRead::Item(item) => assert_eq!(item, vec![1u8, 2, 3]),
        _ => panic!("a ready item is not an error"),
    }
    assert!(
        matches!(
            next_stream_read::<_, Vec<u8>>(&mut ready, Duration::from_millis(50)).await,
            StreamRead::End
        ),
        "clean end of stream is End"
    );
}

/// The real-world repro (issue #271): OpenRouter answers a doubled/typo'd
/// model slug with HTTP 400 and a body naming the exact slug it rejected.
#[test]
fn classify_http_status_400_invalid_model_names_a_recovery_hint() {
    let err = classify_http_status(
        "OpenRouter",
        reqwest::StatusCode::BAD_REQUEST,
        None,
        r#"{"error":{"message":"openrouter/openrouter/auto is not a valid model ID","code":400}}"#,
        "openrouter/openrouter/auto",
    );
    assert!(!err.is_retryable(), "a 400 must never be retried: {err:?}");
    assert!(matches!(err, ProviderError::Terminal(_)));
    let msg = err.to_string();
    assert!(msg.contains("is not a valid model ID"), "{msg}");
    assert!(msg.contains("SETTINGS tab"), "{msg}");
    assert!(msg.contains("--model provider/slug"), "{msg}");
    assert!(msg.contains("settings.json"), "{msg}");
}

/// A 404 "model not found" must get the same hint as a 400 — both are
/// the "this model can't be reached" family the issue calls out
/// together ("invalid model, invalid request shape, 404 model").
#[test]
fn classify_http_status_404_model_not_found_also_gets_the_hint() {
    let err = classify_http_status(
        "OpenAI",
        reqwest::StatusCode::NOT_FOUND,
        None,
        r#"{"error":{"message":"The model `gpt-nope` does not exist"}}"#,
        "gpt-nope",
    );
    assert!(!err.is_retryable());
    assert!(err.to_string().contains("SETTINGS tab"));
}

/// A 4xx that has nothing to do with the model (a malformed tool-call
/// schema, say) must NOT get the model-recovery hint — it would be a
/// non sequitur ("change your model" doesn't fix a bad JSON schema).
/// Guards the precision of `mentions_configured_model` against the
/// obvious failure mode of firing on every non-auth 4xx.
#[test]
fn classify_http_status_400_unrelated_to_the_model_has_no_recovery_hint() {
    let err = classify_http_status(
        "OpenAI",
        reqwest::StatusCode::BAD_REQUEST,
        None,
        r#"{"error":{"message":"messages: array is too long"}}"#,
        "gpt-5.5",
    );
    let msg = err.to_string();
    assert!(!msg.contains("SETTINGS tab"), "{msg}");
    assert!(!msg.contains("--model"), "{msg}");
}

/// Issue #250: a 401 must fold the provider's own reason into the message
/// rather than discarding the response body, so the user sees why.
#[test]
fn classify_http_status_401_folds_in_the_provider_reason_and_a_credential_hint() {
    let err = classify_http_status(
        "OpenAI",
        reqwest::StatusCode::UNAUTHORIZED,
        None,
        r#"{"error":{"message":"Incorrect API key provided: sk-***abcd"}}"#,
        "gpt-5.5",
    );
    assert!(!err.is_retryable());
    assert!(matches!(err, ProviderError::Auth(_)));
    let msg = err.to_string();
    assert!(msg.contains("Incorrect API key provided"), "{msg}");
    assert!(msg.contains("SETTINGS tab"), "{msg}");
    assert!(msg.contains("api_key"), "{msg}");
}

/// 403 with billing language ⇒ the credits/spend-cap hint, distinct from
/// a bad key and from "model not enabled" (issue #250 acceptance: the
/// three must read as different failures, not one "credentials failed").
#[test]
fn classify_http_status_403_with_billing_language_hints_at_credits() {
    let err = classify_http_status(
        "Anthropic",
        reqwest::StatusCode::FORBIDDEN,
        None,
        r#"{"error":{"message":"Your organization has insufficient credit balance"}}"#,
        "claude-opus",
    );
    assert!(!err.is_retryable());
    let msg = err.to_string();
    assert!(msg.contains("credits"), "{msg}");
    assert!(!msg.contains("enable it"), "{msg}");
}

/// 403 with "not enabled for this key" ⇒ the model-permission hint, not
/// the credits hint and not the generic fallback.
#[test]
fn classify_http_status_403_model_not_enabled_hints_at_switching_models() {
    let err = classify_http_status(
        "OpenRouter",
        reqwest::StatusCode::FORBIDDEN,
        None,
        r#"{"error":{"message":"model 'x/y' not enabled for this key"}}"#,
        "x/y",
    );
    let msg = err.to_string();
    assert!(msg.contains("isn't enabled for this model"), "{msg}");
    assert!(msg.contains("enable it"), "{msg}");
    assert!(msg.contains("--model provider/slug"), "{msg}");
    assert!(!msg.contains("credits"), "{msg}");
}

/// A bare 403 with neither billing nor model language falls to the
/// generic permission hint — never silently identical to the 401 (key
/// revoked) message, since the key here is valid.
#[test]
fn classify_http_status_403_generic_falls_back_to_a_permission_hint() {
    let err = classify_http_status(
        "Anthropic",
        reqwest::StatusCode::FORBIDDEN,
        None,
        r#"{"error":{"message":"request blocked by organization policy"}}"#,
        "claude-opus",
    );
    let msg = err.to_string();
    assert!(msg.contains("lacks permission"), "{msg}");
    assert!(!msg.contains("credits"), "{msg}");
    assert!(!msg.contains("enable it"), "{msg}");
}

/// 402 (some gateways use Payment Required rather than folding
/// out-of-credits into a 403) must read as a distinct billing failure,
/// not the generic `Terminal("{label} HTTP {status}: {body}")` bucket —
/// asserted against an EMPTY body so the message can only be coming from
/// the new dedicated arm, never from an echoed body.
#[test]
fn classify_http_status_402_is_terminal_with_a_billing_hint() {
    let err = classify_http_status(
        "OpenAI",
        reqwest::StatusCode::PAYMENT_REQUIRED,
        None,
        "",
        "gpt-5.5",
    );
    assert!(!err.is_retryable());
    assert!(matches!(err, ProviderError::Terminal(_)));
    let msg = err.to_string();
    assert!(msg.contains("payment required"), "{msg}");
    assert!(msg.contains("out of credits"), "{msg}");
}

/// The 402 that killed three benchmark runs, replayed verbatim off the
/// trial that died of it. A gateway prices the request against the
/// ceiling the caller *asks for*, so a 128K ask is refused while the
/// balance still funds dozens of ordinary calls — and terminating on it
/// threw away every one of them. It must classify as the recoverable
/// output-budget rejection, carrying the figure the provider named so
/// the engine can ask for less.
#[test]
fn classify_http_status_402_naming_an_affordable_ceiling_is_recoverable() {
    let body = r#"{"error":{"message":"This request requires more credits, or fewer max_tokens. You requested up to 128000 tokens, but can only afford 117676. To increase, visit https://openrouter.ai/settings/credits and add more credits","code":402}}"#;
    let err = classify_http_status(
        "OpenRouter",
        reqwest::StatusCode::PAYMENT_REQUIRED,
        None,
        body,
        "anthropic/claude-sonnet-5",
    );
    let ProviderError::OutputBudgetExceeded {
        affordable_output_tokens,
        ..
    } = &err
    else {
        panic!("expected OutputBudgetExceeded, got {err:?}");
    };
    assert_eq!(*affordable_output_tokens, Some(117_676));
    // Never retried as-is: the identical ceiling rejects identically.
    assert!(!err.is_retryable());
}

/// The other 402, which must stay terminal: no ceiling is small enough
/// to fund an account with no credit at all, so clamping and re-issuing
/// would only spend rungs on a request that cannot succeed.
#[test]
fn classify_http_status_402_out_of_credit_stays_terminal() {
    let body = r#"{"error":{"message":"Insufficient credits. Add more credits to continue."}}"#;
    let err = classify_http_status(
        "OpenRouter",
        reqwest::StatusCode::PAYMENT_REQUIRED,
        None,
        body,
        "anthropic/claude-sonnet-5",
    );
    assert!(matches!(err, ProviderError::Terminal(_)), "{err:?}");
}

/// The third 402 shape, recorded verbatim off the wire (#4380): the balance
/// is committed to the caller's own concurrent calls, and the provider states
/// the recovery — wait. Retryable, so the ladder and the parked wait reach it;
/// and the message must not repeat the out-of-credit claim, which was believed
/// by the user whose next prompt was "ran out of money :(" while the same key
/// went on funding calls.
#[test]
fn classify_http_status_402_naming_in_flight_requests_is_retryable() {
    let body = r#"{"error":{"message":"This request would exceed your available credits given your current in-flight requests. Retry after in-flight requests settle, or add credits.","code":402}}"#;
    let err = classify_http_status(
        "OpenRouter",
        reqwest::StatusCode::PAYMENT_REQUIRED,
        None,
        body,
        "moonshotai/kimi-k3",
    );
    assert!(err.is_retryable(), "{err:?}");
    let ProviderError::RateLimited { retry_after_ms, .. } = &err else {
        panic!("expected RateLimited, got {err:?}");
    };
    // No Retry-After header on this refusal: the ladder's own backoff runs.
    assert_eq!(*retry_after_ms, None);
    let msg = err.to_string();
    assert!(msg.contains("in-flight"), "{msg}");
    assert!(!msg.contains("out of credits"), "{msg}");
}

/// The rejection is recognised by shape even when the provider states no
/// figure — the engine then halves its own ask rather than treating a
/// repairable refusal as the end of the turn.
#[test]
fn classify_http_status_402_without_a_figure_is_still_recoverable() {
    let err = classify_http_status(
        "OpenRouter",
        reqwest::StatusCode::PAYMENT_REQUIRED,
        None,
        r#"{"error":{"message":"This request requires more credits, or fewer max_tokens."}}"#,
        "m",
    );
    assert!(
        matches!(
            err,
            ProviderError::OutputBudgetExceeded {
                affordable_output_tokens: None,
                ..
            }
        ),
        "{err:?}"
    );
}

/// A provider fronted by a CDN answers a 5xx with a multi-kilobyte HTML
/// error page. The classified error must carry a bounded head plus an
/// explicit elision marker, never the whole page — the message reaches
/// the transcript, the log, and the user's terminal.
#[test]
fn classify_http_status_bounds_a_huge_body_instead_of_echoing_it_whole() {
    let huge = "x".repeat(50_000);
    let err = classify_http_status(
        "OpenRouter",
        reqwest::StatusCode::BAD_GATEWAY,
        None,
        &huge,
        "openrouter/auto",
    );
    assert!(err.is_retryable(), "a 5xx stays retryable: {err:?}");
    let msg = err.to_string();
    assert!(
        msg.len() < 2_000,
        "the echoed body must be bounded, got {} bytes",
        msg.len()
    );
    assert!(msg.contains("more chars"), "names the elision: {msg}");
}

/// The bound must not perturb a normal, short structured error body —
/// every real provider reason is a sentence or two and rides verbatim.
#[test]
fn classify_http_status_leaves_a_short_body_verbatim() {
    let err = classify_http_status(
        "OpenAI",
        reqwest::StatusCode::BAD_GATEWAY,
        None,
        "upstream down",
        "gpt-5.5",
    );
    let msg = err.to_string();
    assert!(msg.contains("upstream down"), "{msg}");
    assert!(!msg.contains("more chars"), "{msg}");
}

/// The word "access" is how providers spell a *generic* permission
/// refusal, not model enablement. Keying the enablement hint on it told
/// users to "enable this model for your key/org" for every plain
/// access-denied 403 — a confident wrong diagnosis. It must fall to the
/// scopes/organization hint instead.
#[test]
fn classify_http_status_403_access_denied_is_a_permission_hint_not_a_model_hint() {
    let err = classify_http_status(
        "OpenAI",
        reqwest::StatusCode::FORBIDDEN,
        None,
        r#"{"error":{"message":"Access denied for this resource"}}"#,
        "gpt-5.5",
    );
    let msg = err.to_string();
    assert!(msg.contains("lacks permission"), "{msg}");
    assert!(!msg.contains("enable it"), "{msg}");
}

/// Anthropic's documented overflow shape: HTTP 400 `invalid_request_error`
/// with "prompt is too long: N tokens > M maximum". Parity witness for the
/// `anthropic` row on the overflow axis (`provider_parity::OVERFLOW_POSTURE`).
#[test]
fn an_anthropic_prompt_too_long_400_classifies_as_context_overflow() {
    let err = classify_http_status(
        "Anthropic",
        reqwest::StatusCode::BAD_REQUEST,
        None,
        r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 200468 tokens > 200000 maximum"}}"#,
        "claude-opus",
    );
    assert!(
        matches!(err, ProviderError::ContextOverflow { .. }),
        "{err:?}"
    );
    assert!(
        !err.is_retryable(),
        "identical re-issue rejects identically"
    );
    let msg = err.to_string();
    assert!(msg.contains("context window"), "{msg}");
    assert!(msg.contains("prompt is too long"), "{msg}");
}

/// The OpenAI-compatible overflow shape, detected by the machine code
/// alone — the message is free to vary per gateway. Parity witness for the
/// `openai` row (and the shared-adapter rows that declare this signature).
#[test]
fn an_openai_context_length_exceeded_400_classifies_as_context_overflow() {
    let err = classify_http_status(
        "OpenAI",
        reqwest::StatusCode::BAD_REQUEST,
        None,
        r#"{"error":{"message":"This model's maximum context length is 128000 tokens. However, your messages resulted in 143211 tokens.","type":"invalid_request_error","param":"messages","code":"context_length_exceeded"}}"#,
        "gpt-5.5",
    );
    assert!(
        matches!(err, ProviderError::ContextOverflow { .. }),
        "{err:?}"
    );
    assert!(!err.is_retryable());

    // The code alone is sufficient — a gateway that rewrites the prose
    // but forwards the code still classifies.
    let code_only = classify_http_status(
        "OpenRouter",
        reqwest::StatusCode::BAD_REQUEST,
        None,
        r#"{"error":{"message":"upstream rejected the request","code":"context_length_exceeded"}}"#,
        "openrouter/auto",
    );
    assert!(
        matches!(code_only, ProviderError::ContextOverflow { .. }),
        "{code_only:?}"
    );
}

/// Gemini/Vertex spell overflow as INVALID_ARGUMENT prose. Parity witness
/// for the `gemini` and `vertex` rows on the overflow axis.
#[test]
fn a_gemini_token_count_overflow_400_classifies_as_context_overflow() {
    let err = classify_http_status(
        "Gemini",
        reqwest::StatusCode::BAD_REQUEST,
        None,
        r#"{"error":{"code":400,"message":"The input token count (1189033) exceeds the maximum number of tokens allowed (1048576).","status":"INVALID_ARGUMENT"}}"#,
        "gemini-3-pro",
    );
    assert!(
        matches!(err, ProviderError::ContextOverflow { .. }),
        "{err:?}"
    );
    assert!(!err.is_retryable());
}

/// Bedrock's Converse ValidationException, reported flat at the top level
/// rather than under an `error` object. Parity witness for the `bedrock`
/// row on the overflow axis.
#[test]
fn a_bedrock_input_too_long_validation_400_classifies_as_context_overflow() {
    let err = classify_http_status(
        "Bedrock",
        reqwest::StatusCode::BAD_REQUEST,
        None,
        r#"{"__type":"ValidationException","message":"Input is too long for requested model."}"#,
        "us.anthropic.claude-opus",
    );
    assert!(
        matches!(err, ProviderError::ContextOverflow { .. }),
        "{err:?}"
    );
    assert!(!err.is_retryable());
}

/// A bare 413 needs no body at all: Payload Too Large has no other
/// meaning on a completion endpoint.
#[test]
fn a_413_classifies_as_context_overflow_without_a_body_signature() {
    let err = classify_http_status(
        "OpenAI",
        reqwest::StatusCode::PAYLOAD_TOO_LARGE,
        None,
        "",
        "gpt-5.5",
    );
    assert!(
        matches!(err, ProviderError::ContextOverflow { .. }),
        "{err:?}"
    );
}

/// Precision guard: a 400 whose message merely *mentions* size or context
/// in another sense must not classify as overflow — a false positive here
/// makes the engine compact and retry a request that failed for a
/// different reason entirely.
#[test]
fn an_unrelated_400_never_classifies_as_context_overflow() {
    for body in [
        r#"{"error":{"message":"messages: array is too long"}}"#,
        r#"{"error":{"message":"tool schema too deeply nested for this context"}}"#,
        r#"{"error":{"message":"invalid JSON in tool arguments","code":"invalid_request"}}"#,
        // A numeric `code` (OpenRouter echoes the HTTP status) is not the
        // overflow code.
        r#"{"error":{"message":"bad request","code":400}}"#,
    ] {
        let err = classify_http_status(
            "OpenAI",
            reqwest::StatusCode::BAD_REQUEST,
            None,
            body,
            "gpt-5.5",
        );
        assert!(
            matches!(err, ProviderError::Terminal(_)),
            "{body} must stay Terminal, got {err:?}"
        );
    }
}

/// 429/5xx stay retryable — pinned here so a future edit to the
/// 401/403/402/model-hint arms can't silently regress the retryable side.
#[test]
fn classify_http_status_429_and_5xx_stay_retryable() {
    let rate_limited = classify_http_status(
        "OpenAI",
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        Some(1_500),
        "",
        "gpt-5.5",
    );
    assert!(rate_limited.is_retryable());
    assert!(matches!(rate_limited, ProviderError::RateLimited { .. }));

    let server_error = classify_http_status(
        "OpenAI",
        reqwest::StatusCode::BAD_GATEWAY,
        None,
        "upstream down",
        "gpt-5.5",
    );
    assert!(server_error.is_retryable());
    assert!(matches!(server_error, ProviderError::Transport { .. }));
}

/// The #2742 witness on the classifier: 529 is the one 5xx whose recovery
/// is a function of waiting, so it must reach `stella-core`'s parked wait
/// as a park-eligible class — not as `Transport`, which the park path
/// declines and which therefore aborted a turn with budget left.
#[test]
fn classify_http_status_529_is_park_eligible_overloaded_not_transport() {
    let err = classify_http_status(
        "Anthropic",
        reqwest::StatusCode::from_u16(HTTP_OVERLOADED).expect("529 is a valid status"),
        Some(90_000),
        "overloaded",
        "claude-fable-5",
    );
    assert!(
        matches!(
            err,
            ProviderError::Overloaded {
                retry_after_ms: Some(90_000),
                ..
            }
        ),
        "{err:?}"
    );
    assert!(err.is_retryable());
    assert!(
        err.is_park_eligible(),
        "a 529 brownout is survived by waiting"
    );
    assert!(err.to_string().contains("529"), "{err}");
}

/// The split is exactly one status wide: every other 5xx keeps the
/// `Transport` class, because retrying a 500 or a 503 in five minutes is
/// no likelier to work than retrying it in one second.
#[test]
fn classify_http_status_other_5xx_stay_transport_and_never_park() {
    for code in [500u16, 502, 503, 504] {
        let err = classify_http_status(
            "OpenAI",
            reqwest::StatusCode::from_u16(code).expect("valid status"),
            None,
            "upstream down",
            "gpt-5.5",
        );
        assert!(
            matches!(err, ProviderError::Transport { .. }),
            "HTTP {code}: {err:?}"
        );
        assert!(err.is_retryable(), "HTTP {code}");
        assert!(!err.is_park_eligible(), "HTTP {code}");
    }
}
