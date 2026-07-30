//! Tests for the Gemini direct adapter — split out of `gemini.rs` (#921)
//! so the adapter stays under the file-size ratchet, matching how
//! `anthropic.rs` and `zai.rs` already carry their suites.

use super::*;
use stella_protocol::CompletionRequest;
use stella_protocol::tool::ToolSchema;
use stella_protocol::{ToolOutput, ToolResult};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn user_attachments_map_to_inline_data_parts_of_every_kind() {
    use stella_protocol::{Attachment, AttachmentSource};
    let att = |name: &str, mime: &str, b64: &str| Attachment {
        name: name.into(),
        media_type: mime.into(),
        byte_len: 3,
        source: AttachmentSource::Data { base64: b64.into() },
    };
    let messages = vec![CompletionMessage::user_with_attachments(
        "look",
        vec![
            att("a.png", "image/png", "aW1n"),
            att("b.pdf", "application/pdf", "cGRm"),
            att("c.mp3", "audio/mpeg", "YXVk"),
            att("d.mp4", "video/mp4", "dmlk"),
        ],
    )];
    let (_, contents) = to_gemini_request_parts(&messages);
    assert_eq!(contents.len(), 1);
    let json = serde_json::to_value(&contents[0]).unwrap();
    let parts = json["parts"].as_array().unwrap();
    assert_eq!(parts.len(), 5, "{json}");
    for (idx, mime) in [
        (0, "image/png"),
        (1, "application/pdf"),
        (2, "audio/mpeg"),
        (3, "video/mp4"),
    ] {
        assert_eq!(parts[idx]["inlineData"]["mimeType"], mime, "{json}");
        assert!(parts[idx]["inlineData"]["data"].is_string());
    }
    assert_eq!(parts[4]["text"], "look");
}

#[test]
fn to_gemini_request_parts_hoists_system_and_maps_roles() {
    let messages = vec![
        CompletionMessage::system("You are a coding agent."),
        CompletionMessage::user("Fix the bug."),
    ];
    let (system, contents) = to_gemini_request_parts(&messages);
    assert_eq!(
        system.unwrap().parts[0].text,
        "You are a coding agent.".to_string()
    );
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0].role, "user");
}

#[test]
fn tool_results_become_function_response_parts_named_via_the_calling_turn() {
    // Gemini has no wire call ids: a functionResponse names the function
    // it answers. The adapter must recover that name from the assistant
    // turn that minted the call_id.
    let messages = vec![
        CompletionMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            tool_calls: vec![ToolCall {
                call_id: "call_0".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "a.rs"}),
            }],
            tool_results: vec![],
            attachments: Vec::new(),
        },
        CompletionMessage {
            role: MessageRole::Tool,
            content: String::new(),
            tool_calls: vec![],
            tool_results: vec![ToolResult {
                call_id: "call_0".into(),
                output: ToolOutput::Ok {
                    content: "fn main(){}".into(),
                },
            }],
            attachments: Vec::new(),
        },
    ];
    let (_, contents) = to_gemini_request_parts(&messages);
    assert_eq!(contents.len(), 2);
    assert_eq!(contents[0].role, "model");
    let call = contents[0].parts[0].function_call.as_ref().unwrap();
    assert_eq!(call.name, "read_file");
    assert_eq!(contents[1].role, "user");
    let response = contents[1].parts[0].function_response.as_ref().unwrap();
    assert_eq!(response.name, "read_file");
    assert_eq!(
        response.response,
        serde_json::json!({"output": "fn main(){}"})
    );
}

#[test]
fn error_results_are_framed_as_error_objects() {
    let messages = vec![CompletionMessage {
        role: MessageRole::Tool,
        content: String::new(),
        tool_calls: vec![],
        tool_results: vec![ToolResult {
            call_id: "call_1".into(),
            output: ToolOutput::Error {
                message: "no such file".into(),
            },
        }],
        attachments: Vec::new(),
    }];
    let (_, contents) = to_gemini_request_parts(&messages);
    let response = contents[0].parts[0].function_response.as_ref().unwrap();
    assert_eq!(
        response.response,
        serde_json::json!({"error": "no such file"})
    );
}

#[test]
fn thought_signatures_round_trip_through_the_minted_call_id() {
    // Gemini 3 attaches a thoughtSignature to a functionCall part and
    // requires it echoed back on the matching history part. The engine's
    // ToolCall has no slot for it, so it rides inside the minted call_id
    // — this test pins the full mint → history-replay round trip.
    let messages = vec![CompletionMessage {
        role: MessageRole::Assistant,
        content: String::new(),
        tool_calls: vec![ToolCall {
            call_id: "call_0#c2lnbmF0dXJl".into(),
            name: "bash".into(),
            input: serde_json::json!({"command": "ls"}),
        }],
        tool_results: vec![],
        attachments: Vec::new(),
    }];
    let (_, contents) = to_gemini_request_parts(&messages);
    let part = &contents[0].parts[0];
    assert_eq!(part.thought_signature.as_deref(), Some("c2lnbmF0dXJl"));
    let call = part.function_call.as_ref().unwrap();
    assert_eq!(call.name, "bash");
}

#[test]
fn thinking_level_maps_low_directly_and_everything_else_to_high() {
    assert_eq!(map_thinking_level(ReasoningEffort::Low), "low");
    assert_eq!(map_thinking_level(ReasoningEffort::Medium), "high");
    assert_eq!(map_thinking_level(ReasoningEffort::High), "high");
    assert_eq!(map_thinking_level(ReasoningEffort::Xhigh), "high");
    assert_eq!(map_thinking_level(ReasoningEffort::Max), "high");
}

/// #612 -- the deck must see the answer as it arrives.
///
/// Gemini already streamed on the wire; only `complete_observed_ref` was
/// missing, so it inherited the trait's silent default and the deck
/// stayed blank until the whole turn completed. Thought-summary parts
/// stay silent, matching anthropic and zai.
#[tokio::test]
async fn complete_observed_streams_answer_deltas_in_order_never_thoughts() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"planning...\",\"thought\":true}]}}]}\n\n",
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hel\"}]}}]}\n\n",
        "data: {\"candidates\":[{\"finishReason\":\"STOP\",\"content\":{\"parts\":[{\"text\":\"lo!\"}]}}]}\n\n",
    );
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider =
        GeminiProvider::new(ApiKey::new("k"), "gemini-3-pro").with_base_url(server.uri());
    let observer = RecordingObserver::new();
    let result = provider
        .complete_observed(observed_req("say hello"), &observer)
        .await
        .expect("completion should succeed");

    assert_eq!(result.text, "Hello!");
    assert_eq!(
        observer.deltas.lock().unwrap().as_slice(),
        &["Hel".to_string(), "lo!".to_string()],
        "answer deltas must arrive in order, and thought parts must not"
    );
}

/// A `functionCall` part arrives whole, so it is announced on arrival --
/// and the announced call must be byte-identical to the committed one,
/// which is what makes mid-stream tool speculation safe.
#[tokio::test]
async fn complete_observed_announces_a_tool_call_identical_to_the_committed_one() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"read_file\",\"args\":{\"path\":\"a.rs\"}}}]}}]}\n\n",
        "data: {\"candidates\":[{\"finishReason\":\"STOP\",\"content\":{\"parts\":[]}}]}\n\n",
    );
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider =
        GeminiProvider::new(ApiKey::new("k"), "gemini-3-pro").with_base_url(server.uri());
    let observer = RecordingObserver::new();
    let result = provider
        .complete_observed(observed_req("read it"), &observer)
        .await
        .expect("completion should succeed");

    let announced = observer.calls.lock().unwrap().clone();
    assert_eq!(
        announced.len(),
        1,
        "the call must be announced exactly once"
    );
    assert_eq!(
        announced, result.tool_calls,
        "the announced call must match the committed one exactly"
    );
}

/// A no-argument call omits `args` on the wire. It must still be
/// announced, as `{}` rather than `null` -- the same normalization the
/// committed call gets.
#[tokio::test]
async fn complete_observed_announces_a_no_argument_call_as_an_empty_object() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"repo_status\"}}]}}]}\n\n",
        "data: {\"candidates\":[{\"finishReason\":\"STOP\",\"content\":{\"parts\":[]}}]}\n\n",
    );
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider =
        GeminiProvider::new(ApiKey::new("k"), "gemini-3-pro").with_base_url(server.uri());
    let observer = RecordingObserver::new();
    let result = provider
        .complete_observed(observed_req("status"), &observer)
        .await
        .expect("completion should succeed");

    let announced = observer.calls.lock().unwrap().clone();
    assert_eq!(announced.len(), 1);
    assert_eq!(announced[0].input, serde_json::json!({}));
    assert_eq!(announced, result.tool_calls);
}

/// Shared observer double. Mirrors the anthropic and zai copies.
struct RecordingObserver {
    calls: std::sync::Mutex<Vec<ToolCall>>,
    deltas: std::sync::Mutex<Vec<String>>,
}

impl RecordingObserver {
    fn new() -> Self {
        Self {
            calls: std::sync::Mutex::new(Vec::new()),
            deltas: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl stella_protocol::ToolCallObserver for RecordingObserver {
    fn tool_call_streamed(&self, call: &ToolCall) {
        self.calls.lock().unwrap().push(call.clone());
    }
    fn text_delta(&self, delta: &str) {
        self.deltas.lock().unwrap().push(delta.to_string());
    }
}

fn observed_req(prompt: &str) -> CompletionRequest {
    CompletionRequest {
        messages: vec![
            CompletionMessage::system("system"),
            CompletionMessage::user(prompt),
        ],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    }
}

#[tokio::test]
async fn complete_streams_and_aggregates_text_excluding_thought_parts() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"planning...\",\"thought\":true}]}}]}\n\n",
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hel\"}]}}]}\n\n",
        "data: {\"candidates\":[{\"finishReason\":\"STOP\",\"content\":{\"parts\":[{\"text\":\"lo!\"}]}}],\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\":3,\"thoughtsTokenCount\":5,\"cachedContentTokenCount\":2}}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/models/gemini-3-pro:streamGenerateContent"))
        .and(query_param("alt", "sse"))
        .and(header("x-goog-api-key", "test-gemini-key"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider = GeminiProvider::new(ApiKey::new("test-gemini-key"), "gemini-3-pro")
        .with_base_url(server.uri());

    let req = CompletionRequest {
        messages: vec![
            CompletionMessage::system("system"),
            CompletionMessage::user("say hello"),
        ],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    };

    let result = provider
        .complete(req)
        .await
        .expect("completion should succeed");
    assert_eq!(result.text, "Hello!");
    assert_eq!(result.usage.input_tokens, 10);
    // candidates (3) + thoughts (5): thinking tokens are billed output.
    assert_eq!(result.usage.output_tokens, 8);
    assert_eq!(result.usage.cached_input_tokens, 2);
    assert!(result.usage.reported);
    assert_eq!(result.model, "gemini-3-pro");
}

/// The clean-EOF twin of the streaming test above: a well-formed Gemini
/// stream that simply ENDS without any candidate carrying a `finishReason`
/// (close-delimited HTTP/1.1 proxies, LB idle-reaps, and truncating gateways
/// surface a dropped connection as clean EOF, not a reqwest error) must fail
/// as a retryable Transport disconnect — never commit the partial "Hel" as a
/// successful completion. Mirrors #362's openai/zai clean-EOF guards.
#[tokio::test]
async fn complete_returns_transport_err_on_clean_eof_without_finish_reason() {
    let server = MockServer::start().await;
    // Text (and even a usageMetadata frame) arrive, then the stream ends —
    // no candidate ever carries a `finishReason`. `usageMetadata` riding an
    // early chunk proves it is NOT treated as a terminal marker: Gemini can
    // emit it before the final chunk, so only `finishReason` ends a turn.
    let sse_body = concat!(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hel\"}]}}],",
        "\"usageMetadata\":{\"promptTokenCount\":8,\"candidatesTokenCount\":1}}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/models/gemini-3-pro:streamGenerateContent"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider = GeminiProvider::new(ApiKey::new("test-gemini-key"), "gemini-3-pro")
        .with_base_url(server.uri());
    let req = CompletionRequest {
        messages: vec![CompletionMessage::user("hi")],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    };

    let err = provider.complete(req).await.unwrap_err();
    assert!(
        matches!(err, ProviderError::Transport(_)),
        "expected Transport, got {err:?}"
    );
    assert!(err.is_retryable(), "a disconnect must be retryable");
    let msg = err.to_string();
    assert!(msg.contains("Gemini"), "names the provider: {msg}");
    assert!(
        msg.contains("finishReason"),
        "names the missing terminal event: {msg}"
    );
}

#[tokio::test]
async fn complete_mints_call_ids_and_captures_thought_signatures() {
    let server = MockServer::start().await;
    // Two parallel functionCall parts in one turn — Gemini sends args as
    // complete JSON objects (no fragment reassembly) and a Gemini 3
    // thoughtSignature on the first part of the group.
    let sse_body = concat!(
        "data: {\"candidates\":[{\"finishReason\":\"STOP\",\"content\":{\"parts\":[",
        "{\"functionCall\":{\"name\":\"read_file\",\"args\":{\"path\":\"src/lib.rs\"}},\"thoughtSignature\":\"c2ln\"},",
        "{\"functionCall\":{\"name\":\"bash\",\"args\":{\"command\":\"ls\"}}}",
        "]}}],\"usageMetadata\":{\"promptTokenCount\":20,\"candidatesTokenCount\":12}}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/models/gemini-3-pro:streamGenerateContent"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider =
        GeminiProvider::new(ApiKey::new("test-key"), "gemini-3-pro").with_base_url(server.uri());

    let req = CompletionRequest {
        messages: vec![CompletionMessage::user("read src/lib.rs and list files")],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![ToolSchema {
            name: "read_file".into(),
            description: "Read a file".into(),
            input_schema: serde_json::json!({"type":"object"}),
            read_only: false,
        }],
        reasoning: None,
        params: None,
    };

    let result = provider.complete(req).await.expect("should succeed");
    assert_eq!(result.tool_calls.len(), 2);
    assert_eq!(result.tool_calls[0].call_id, "call_0#c2ln");
    assert_eq!(result.tool_calls[0].name, "read_file");
    assert_eq!(
        result.tool_calls[0].input,
        serde_json::json!({"path": "src/lib.rs"})
    );
    assert_eq!(result.tool_calls[1].call_id, "call_1");
    assert_eq!(result.tool_calls[1].name, "bash");
}

#[tokio::test]
async fn complete_normalizes_a_no_arg_call_to_an_empty_object_not_null() {
    let server = MockServer::start().await;
    // A no-argument Gemini/Vertex tool call omits `args` on the wire, so
    // the `#[serde(default)]` field deserializes to `Value::Null`. It must
    // surface as an empty object, not null — `Value::Null` is the
    // malformed-call sentinel `driver.rs::execute_with_repair` checks, so a
    // valid no-arg call reported as null would be wrongly "repaired".
    let sse_body = concat!(
        "data: {\"candidates\":[{\"finishReason\":\"STOP\",\"content\":{\"parts\":[",
        "{\"functionCall\":{\"name\":\"now\"}}",
        "]}}],\"usageMetadata\":{\"promptTokenCount\":5,\"candidatesTokenCount\":2}}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/models/gemini-3-pro:streamGenerateContent"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider =
        GeminiProvider::new(ApiKey::new("test-key"), "gemini-3-pro").with_base_url(server.uri());

    let req = CompletionRequest {
        messages: vec![CompletionMessage::user("what time is it")],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![ToolSchema {
            name: "now".into(),
            description: "Current time".into(),
            input_schema: serde_json::json!({"type":"object"}),
            read_only: false,
        }],
        reasoning: None,
        params: None,
    };

    let result = provider.complete(req).await.expect("should succeed");
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0].name, "now");
    assert_eq!(result.tool_calls[0].input, serde_json::json!({}));
    assert!(!result.tool_calls[0].input.is_null());
}

#[tokio::test]
async fn complete_maps_403_to_auth_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
        .mount(&server)
        .await;

    let provider =
        GeminiProvider::new(ApiKey::new("bad-key"), "gemini-3-pro").with_base_url(server.uri());

    let req = CompletionRequest {
        messages: vec![CompletionMessage::user("hi")],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    };

    let err = provider.complete(req).await.unwrap_err();
    assert!(matches!(err, ProviderError::Auth(_)));
    assert!(!err.is_retryable());
}

#[tokio::test]
async fn complete_maps_400_api_key_invalid_to_auth_not_terminal() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_string(
                "{\"error\":{\"code\":400,\"status\":\"INVALID_ARGUMENT\",\"details\":[{\"reason\":\"API_KEY_INVALID\"}]}}",
            ))
            .mount(&server)
            .await;

    let provider =
        GeminiProvider::new(ApiKey::new("bad-key"), "gemini-3-pro").with_base_url(server.uri());

    let req = CompletionRequest {
        messages: vec![CompletionMessage::user("hi")],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    };

    let err = provider.complete(req).await.unwrap_err();
    assert!(matches!(err, ProviderError::Auth(_)));
}

#[tokio::test]
async fn complete_maps_429_to_rate_limited_with_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "3")
                .set_body_string("quota exceeded"),
        )
        .mount(&server)
        .await;

    let provider =
        GeminiProvider::new(ApiKey::new("key"), "gemini-3-pro").with_base_url(server.uri());

    let req = CompletionRequest {
        messages: vec![CompletionMessage::user("hi")],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    };

    let err = provider.complete(req).await.unwrap_err();
    assert!(err.is_retryable());
    match err {
        ProviderError::RateLimited { retry_after_ms, .. } => {
            assert_eq!(retry_after_ms, Some(3000));
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

/// A bare request for config-shape tests; overrides are patched in.
fn bare_request() -> CompletionRequest {
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

#[test]
fn generation_config_forwards_params_in_camel_case() {
    use stella_protocol::GenerationParams;
    let req = CompletionRequest {
        params: Some(GenerationParams {
            top_p: Some(0.9),
            top_k: Some(40),
            frequency_penalty: Some(0.5),
            presence_penalty: Some(0.25),
            seed: Some(7),
            // No generateContent slot — silently dropped.
            repetition_penalty: Some(1.1),
            verbosity: Some(stella_protocol::Verbosity::High),
            service_tier: Some(stella_protocol::ServiceTier::Priority),
        }),
        ..bare_request()
    };
    let config = build_generation_config(req.as_borrowed()).expect("params force a config");
    // Assert on the serialized string (the exact bytes reqwest sends),
    // not a `to_value` round-trip — `to_value` widens f32 through f64
    // and would perturb 0.9 into 0.90000003….
    let body = serde_json::to_string(&config).unwrap();
    // The REST API is camelCase — snake_case keys here would be silently
    // ignored by the server, which is why the casing is pinned.
    assert!(body.contains("\"topP\":0.9"), "{body}");
    assert!(body.contains("\"topK\":40"), "{body}");
    assert!(body.contains("\"seed\":7"), "{body}");
    assert!(body.contains("\"frequencyPenalty\":0.5"), "{body}");
    assert!(body.contains("\"presencePenalty\":0.25"), "{body}");
    for dropped in ["repetition", "verbosity", "serviceTier", "top_p"] {
        assert!(!body.contains(dropped), "`{dropped}` leaked into: {body}");
    }
}

/// The byte-stability contract: no overrides at all — including a params
/// struct carrying only fields this dialect can't express — must omit
/// `generationConfig` entirely, exactly the pre-params wire shape.
#[test]
fn generation_config_is_omitted_without_any_forwardable_override() {
    use stella_protocol::GenerationParams;
    assert!(build_generation_config(bare_request().as_borrowed()).is_none());
    let unforwardable = CompletionRequest {
        params: Some(GenerationParams {
            verbosity: Some(stella_protocol::Verbosity::Low),
            service_tier: Some(stella_protocol::ServiceTier::Flex),
            ..Default::default()
        }),
        ..bare_request()
    };
    assert!(
        build_generation_config(unforwardable.as_borrowed()).is_none(),
        "verbosity/service_tier alone must not produce an empty config"
    );
}

/// `reasoning: Some(false)` → thinkingLevel "low": Gemini 3 cannot fully
/// disable thinking, so the lowest level is the closest honest mapping —
/// and the explicit off wins over a pinned effort. A bare `Some(true)`
/// picks "high" (there is no medium tier).
#[test]
fn reasoning_switch_maps_to_thinking_levels() {
    let off = CompletionRequest {
        reasoning: Some(false),
        effort: Some(ReasoningEffort::Max),
        ..bare_request()
    };
    let config = build_generation_config(off.as_borrowed()).expect("reasoning forces a config");
    let json = serde_json::to_value(&config).unwrap();
    assert_eq!(json["thinkingConfig"]["thinkingLevel"], "low", "{json}");

    let on = CompletionRequest {
        reasoning: Some(true),
        ..bare_request()
    };
    let config = build_generation_config(on.as_borrowed()).expect("reasoning forces a config");
    let json = serde_json::to_value(&config).unwrap();
    assert_eq!(json["thinkingConfig"]["thinkingLevel"], "high", "{json}");

    // A pinned effort with no reasoning switch maps as it always has.
    let effort_only = CompletionRequest {
        effort: Some(ReasoningEffort::Low),
        ..bare_request()
    };
    let config =
        build_generation_config(effort_only.as_borrowed()).expect("effort forces a config");
    let json = serde_json::to_value(&config).unwrap();
    assert_eq!(json["thinkingConfig"]["thinkingLevel"], "low", "{json}");
}

/// End-to-end: the config rides the request body to the wire.
#[tokio::test]
async fn complete_sends_generation_config_params_on_the_wire() {
    use stella_protocol::GenerationParams;
    let server = MockServer::start().await;
    // Carries a terminal `finishReason` so the stream reads as a clean
    // completion (this test only inspects the outgoing request body).
    let sse_body = "data: {\"candidates\":[{\"finishReason\":\"STOP\",\"content\":{\"parts\":[{\"text\":\"ok\"}]}}]}\n\n";
    Mock::given(method("POST"))
        .and(path("/models/gemini-3-pro:streamGenerateContent"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider =
        GeminiProvider::new(ApiKey::new("test-key"), "gemini-3-pro").with_base_url(server.uri());
    provider
        .complete(CompletionRequest {
            params: Some(GenerationParams {
                top_p: Some(0.9),
                top_k: Some(40),
                ..Default::default()
            }),
            reasoning: Some(false),
            ..bare_request()
        })
        .await
        .expect("should succeed");

    let requests = server.received_requests().await.expect("recorded requests");
    let body = String::from_utf8_lossy(&requests[0].body);
    assert!(body.contains("\"topP\":0.9"), "{body}");
    assert!(body.contains("\"topK\":40"), "{body}");
    assert!(
        body.contains("\"thinkingConfig\":{\"thinkingLevel\":\"low\"}"),
        "{body}"
    );
}
