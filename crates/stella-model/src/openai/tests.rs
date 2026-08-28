mod stream_fallback;

use super::*;
// The port itself, since the impl moved to `openai/provider.rs` and this
// file no longer imports the trait these tests dispatch through.
use crate::provider::Provider;
// The result types the delivery paths return: `openai.rs` itself no
// longer names them, since assembly moved to `stream.rs`/`unary.rs`.
use stella_protocol::{CompletionRequest, CompletionUsage, FinishReason, ToolCall};

use stella_protocol::tool::ToolSchema;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The audio/video arm of [`attachment_part`] is out of reach only
/// because `OPENAI_CAPS` switches those kinds off. Flipping one of those
/// bools is a routine one-line edit, so the arm it lands in has to
/// degrade like every other unsupported attachment instead of aborting
/// the process mid-turn.
#[test]
fn a_caps_flip_degrades_instead_of_aborting_the_turn() {
    use crate::attachment::{DialectCaps, wire_parts};
    use stella_protocol::{Attachment, AttachmentSource};
    let flipped = DialectCaps {
        images: true,
        pdfs: true,
        audio: true,
        video: true,
    };
    for (name, mime) in [("song.mp3", "audio/mpeg"), ("clip.mp4", "video/mp4")] {
        let attachment = Attachment {
            name: name.into(),
            media_type: mime.into(),
            byte_len: 3,
            source: AttachmentSource::Data {
                base64: "YWJj".into(),
            },
        };
        let parts: Vec<_> = wire_parts(&[attachment], flipped)
            .into_iter()
            .map(attachment_part)
            .collect();
        let [OpenAiContentPart::InputText { text }] = parts.as_slice() else {
            panic!("expected a degrade note for {mime}, got {parts:?}");
        };
        assert!(
            text.contains(mime),
            "the note names what was attached: {text}"
        );
    }
}

#[test]
fn user_attachments_map_to_input_image_and_input_file_parts() {
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
        ],
    )];
    let (_, input) = to_openai_input(&messages);
    assert_eq!(input.len(), 1);
    let json = serde_json::to_value(&input[0]).unwrap();
    let content = json["content"].as_array().unwrap();
    assert_eq!(content.len(), 4, "{json}");
    assert_eq!(content[0]["type"], "input_image");
    assert_eq!(content[0]["image_url"], "data:image/png;base64,aW1n");
    assert_eq!(content[1]["type"], "input_file");
    assert_eq!(content[1]["filename"], "b.pdf");
    assert_eq!(content[1]["file_data"], "data:application/pdf;base64,cGRm");
    // Audio degrades to a note on this dialect.
    assert_eq!(content[2]["type"], "input_text");
    assert!(
        content[2]["text"].as_str().unwrap().contains("c.mp3"),
        "{json}"
    );
    assert_eq!(content[3]["type"], "input_text");
    assert_eq!(content[3]["text"], "look");
}

/// Every request carries the session-stable `prompt_cache_key` so
/// OpenAI's implicit cache routes all of a session's prefix-sharing
/// turns to the same shard — and the cache telemetry the routing earns
/// (`input_tokens_details.cached_tokens`) lands in `CompletionUsage`.
///
/// This is the test the
/// parity matrix's `openai` row cites for its `Implicit` posture, and
/// that posture's contract is "the telemetry lands in
/// `CompletionUsage`". Until the usage assertion below existed, the
/// cited witness proved only the request key — the telemetry parse was
/// proven by an uncited pricing test the matrix guard could not see
/// (#1285).
#[tokio::test]
async fn complete_sends_a_session_stable_prompt_cache_key() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":100,\"output_tokens\":1,\"input_tokens_details\":{\"cached_tokens\":75}}}}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/responses"))
        .and(body_string_contains("\"prompt_cache_key\":\"stella-"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .expect(2)
        .mount(&server)
        .await;

    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test-openai"), "gpt-5.5").with_base_url(server.uri());
    let req = || CompletionRequest {
        messages: vec![CompletionMessage::user("hi")],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    };
    let first = provider.complete(req()).await.expect("first turn");
    provider.complete(req()).await.expect("second turn");
    // The same provider instance keys both turns identically — routing
    // them to the same cache shard is the whole point — and the shard's
    // hits are visible, not assumed.
    assert_eq!(first.usage.input_tokens, 100);
    assert_eq!(first.usage.cached_input_tokens, 75);
}

#[test]
fn reasoning_models_are_classified_by_family() {
    assert!(is_reasoning_model("gpt-5.5"));
    assert!(is_reasoning_model("gpt-5"));
    assert!(is_reasoning_model("o3-mini"));
    assert!(is_reasoning_model("o1"));
    assert!(!is_reasoning_model("gpt-4o"));
    assert!(!is_reasoning_model("gpt-4.1"));
}

/// The gpt-5 Responses API rejects `temperature` with HTTP 400. The engine
/// defaults temperature to `Some(0.0)`, so the adapter MUST drop it for a
/// reasoning model or every real OpenAI turn fails. Witness: even with a
/// caller-set temperature, the wire body for gpt-5.5 carries no
/// `temperature` key.
#[tokio::test]
async fn temperature_is_omitted_for_a_gpt5_reasoning_model() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test-openai"), "gpt-5.5").with_base_url(server.uri());
    provider
        .complete(CompletionRequest {
            messages: vec![CompletionMessage::user("hi")],
            max_output_tokens: None,
            temperature: Some(0.0), // the engine default that used to 400
            effort: None,
            tools: vec![],
            reasoning: None,
            params: None,
        })
        .await
        .expect("turn");

    let requests = server.received_requests().await.expect("recorded requests");
    let body = String::from_utf8_lossy(&requests[0].body);
    assert!(
        !body.contains("\"temperature\""),
        "gpt-5.5 request must not carry temperature, got: {body}"
    );
}

#[test]
fn to_openai_input_hoists_system_into_instructions_and_maps_user() {
    let messages = vec![
        CompletionMessage::system("You are a coding agent."),
        CompletionMessage::user("Fix the bug."),
    ];
    let (instructions, mapped) = to_openai_input(&messages);
    assert_eq!(instructions, Some("You are a coding agent.".to_string()));
    assert_eq!(mapped.len(), 1);
    match &mapped[0] {
        OpenAiInputItem::Message { role, .. } => assert_eq!(*role, "user"),
        other => panic!("expected a message item, got {other:?}"),
    }
}

#[test]
fn to_openai_input_frames_assistant_tool_calls_and_results_by_call_id() {
    use stella_protocol::{ToolOutput, ToolResult};
    let messages = vec![
        CompletionMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            tool_calls: vec![ToolCall {
                call_id: "call_9".into(),
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
                call_id: "call_9".into(),
                output: ToolOutput::Ok {
                    content: "fn main(){}".into(),
                    data: None,
                },
            }],
            attachments: Vec::new(),
        },
    ];
    let (_, mapped) = to_openai_input(&messages);
    assert_eq!(mapped.len(), 2);
    match &mapped[0] {
        OpenAiInputItem::FunctionCall { call_id, name, .. } => {
            assert_eq!(call_id, "call_9");
            assert_eq!(name, "read_file");
        }
        other => panic!("expected a function_call item, got {other:?}"),
    }
    match &mapped[1] {
        OpenAiInputItem::FunctionCallOutput { call_id, output } => {
            assert_eq!(call_id, "call_9");
            assert_eq!(output, "fn main(){}");
        }
        other => panic!("expected a function_call_output item, got {other:?}"),
    }
}

#[test]
fn to_openai_input_marks_error_results_loudly() {
    use stella_protocol::{ToolOutput, ToolResult};
    let messages = vec![CompletionMessage {
        role: MessageRole::Tool,
        content: String::new(),
        tool_calls: vec![],
        tool_results: vec![ToolResult {
            call_id: "call_1".into(),
            output: ToolOutput::error("no such file"),
        }],
        attachments: Vec::new(),
    }];
    let (_, mapped) = to_openai_input(&messages);
    assert_eq!(mapped.len(), 1);
    match &mapped[0] {
        OpenAiInputItem::FunctionCallOutput { output, .. } => {
            assert!(output.starts_with("ERROR:"))
        }
        other => panic!("expected a function_call_output item, got {other:?}"),
    }
}

#[test]
fn reasoning_effort_maps_low_directly_and_unsupported_tiers_to_high() {
    assert_eq!(map_reasoning_effort(ReasoningEffort::Low), "low");
    assert_eq!(map_reasoning_effort(ReasoningEffort::Medium), "medium");
    assert_eq!(map_reasoning_effort(ReasoningEffort::High), "high");
    assert_eq!(map_reasoning_effort(ReasoningEffort::Xhigh), "high");
    assert_eq!(map_reasoning_effort(ReasoningEffort::Max), "high");
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
        messages: vec![CompletionMessage::user(prompt)],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    }
}

async fn observed_stream(sse_body: &'static str) -> (CompletionResult, RecordingObserver) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;
    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5").with_base_url(server.uri());
    let observer = RecordingObserver::new();
    let result = provider
        .complete_observed(observed_req("go"), &observer)
        .await
        .expect("completion should succeed");
    (result, observer)
}

/// #612 -- OpenAI already streamed on the wire; only `complete_observed_ref`
/// was missing, so it inherited the trait's silent default and the deck
/// stayed blank for the whole turn.
#[tokio::test]
async fn complete_observed_streams_answer_deltas_in_order() {
    let (result, observer) = observed_stream(concat!(
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hel\"}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"lo!\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
    ))
    .await;

    assert_eq!(result.text, "Hello!");
    assert_eq!(
        observer.deltas.lock().unwrap().as_slice(),
        &["Hel".to_string(), "lo!".to_string()]
    );
}

/// The arguments.done event is a precise per-call boundary, so even a
/// stream's LAST tool call is announced -- the gap the chat-completions
/// dialects have, where a call can only be announced when the next one
/// starts.
#[tokio::test]
async fn complete_observed_announces_the_last_tool_call_too() {
    let (result, observer) = observed_stream(concat!(
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"read_file\",\"arguments\":\"\"}}\n\n",
        "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{\\\"path\\\":\"}\n\n",
        "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"\\\"src/lib.rs\\\"}\"}\n\n",
        "data: {\"type\":\"response.function_call_arguments.done\",\"output_index\":0,\"arguments\":\"{\\\"path\\\":\\\"src/lib.rs\\\"}\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
    ))
    .await;

    let announced = observer.calls.lock().unwrap().clone();
    assert_eq!(announced.len(), 1, "the only call must be announced");
    assert_eq!(
        announced, result.tool_calls,
        "the announced call must match the committed one exactly"
    );
    assert_eq!(
        announced[0].input,
        serde_json::json!({"path": "src/lib.rs"})
    );
}

/// The contract's hard rule: never announce a call whose input failed to
/// parse. Speculating on a malformed call would execute something the
/// model never asked for. The broken payload still reaches final
/// assembly, where the repair path owns it.
#[tokio::test]
async fn complete_observed_never_announces_a_call_whose_json_is_broken() {
    let (result, observer) = observed_stream(concat!(
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"read_file\",\"arguments\":\"\"}}\n\n",
        "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{\\\"path\\\": \"}\n\n",
        "data: {\"type\":\"response.function_call_arguments.done\",\"output_index\":0,\"arguments\":\"{\\\"path\\\": \"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
    ))
    .await;

    assert!(
        observer.calls.lock().unwrap().is_empty(),
        "a call with unparseable arguments must never be announced"
    );
    // It still lands in the result, as Null, for the repair path.
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0].input, Value::Null);
}

#[tokio::test]
async fn complete_streams_and_aggregates_text_deltas_from_a_mock_server() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hel\"}\n\n",
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"lo!\"}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":12,\"output_tokens\":2,\"input_tokens_details\":{\"cached_tokens\":4}}}}\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/responses"))
        .and(header("authorization", "Bearer sk-test-openai"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test-openai"), "gpt-5.5").with_base_url(server.uri());

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
    assert_eq!(result.usage.input_tokens, 12);
    assert_eq!(result.usage.output_tokens, 2);
    assert_eq!(result.usage.cached_input_tokens, 4);
    assert!(result.usage.reported);
    assert_eq!(result.model, "gpt-5.5");
}

#[tokio::test]
async fn complete_reassembles_a_streamed_tool_call_split_across_many_chunks() {
    let server = MockServer::start().await;
    // The Responses API announces the function_call item once (with its
    // call_id and name) via `response.output_item.added`, then streams
    // `arguments` as string fragments across several
    // `response.function_call_arguments.delta` events keyed by
    // `output_index` — the exact dialect quirk this test proves the
    // adapter handles, mirroring `zai.rs`'s equivalent test for the
    // OpenAI-compatible dialect's own (structurally different) fragment
    // shape.
    let sse_body = concat!(
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"read_file\",\"arguments\":\"\"}}\n\n",
        "event: response.function_call_arguments.delta\n",
        "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{\\\"path\\\":\"}\n\n",
        "event: response.function_call_arguments.delta\n",
        "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"\\\"src/lib.rs\\\"}\"}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":40,\"output_tokens\":15}}}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5").with_base_url(server.uri());

    let req = CompletionRequest {
        messages: vec![CompletionMessage::user("read src/lib.rs")],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![ToolSchema {
            name: "read_file".into(),
            description: "Read a file".into(),
            input_schema: serde_json::json!({"type":"object"}),
            read_only: false,
            speculation_safe: false,
        }],
        reasoning: None,
        params: None,
    };

    let result = provider.complete(req).await.expect("should succeed");
    assert_eq!(result.tool_calls.len(), 1);
    let call = &result.tool_calls[0];
    assert_eq!(call.call_id, "call_1");
    assert_eq!(call.name, "read_file");
    assert_eq!(call.input, serde_json::json!({"path": "src/lib.rs"}));
    assert_eq!(result.usage.input_tokens, 40);
    assert_eq!(result.usage.output_tokens, 15);
}

#[tokio::test]
async fn complete_falls_back_to_null_when_streamed_arguments_never_parse() {
    let server = MockServer::start().await;
    // Arguments arrive but never form valid JSON (e.g. a dropped
    // fragment) — the adapter must fall back to `Value::Null`, the exact
    // sentinel `stella-core`'s `driver.rs::execute_with_repair` checks
    // for, rather than executing the tool with garbage input.
    let sse_body = concat!(
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_2\",\"name\":\"bash\",\"arguments\":\"\"}}\n\n",
        "event: response.function_call_arguments.delta\n",
        "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{not valid json\"}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5").with_base_url(server.uri());

    let req = CompletionRequest {
        messages: vec![CompletionMessage::user("run ls")],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![ToolSchema {
            name: "bash".into(),
            description: "Run a command".into(),
            input_schema: serde_json::json!({"type":"object"}),
            read_only: false,
            speculation_safe: false,
        }],
        reasoning: None,
        params: None,
    };

    let result = provider.complete(req).await.expect("should succeed");
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0].input, Value::Null);
}

#[tokio::test]
async fn complete_maps_401_to_auth_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
        .mount(&server)
        .await;

    let provider =
        OpenAiProvider::new(ApiKey::new("bad-key"), "gpt-5.5").with_base_url(server.uri());

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
async fn complete_maps_403_to_auth_error() {
    // A permission-denied key is a credential failure, not a generic
    // terminal error. Regression for the drift where only 401 was mapped
    // to Auth here while sibling adapters mapped 401|403.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
        .mount(&server)
        .await;

    let provider =
        OpenAiProvider::new(ApiKey::new("limited-key"), "gpt-5.5").with_base_url(server.uri());

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
async fn complete_maps_429_to_rate_limited_with_retry_after_and_it_is_retryable() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "2")
                .set_body_string("rate limited"),
        )
        .mount(&server)
        .await;

    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5").with_base_url(server.uri());

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
            assert_eq!(retry_after_ms, Some(2000));
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

#[tokio::test]
async fn complete_computes_nonzero_cost_from_catalog_pricing() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1000,\"output_tokens\":500,\"input_tokens_details\":{\"cached_tokens\":200}}}}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5").with_base_url(server.uri());
    let req = CompletionRequest {
        messages: vec![CompletionMessage::user("hi")],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    };

    let result = provider.complete(req).await.expect("should succeed");
    // Cached input is billed at its own rate — assert against the catalog
    // computation so the wiring (and the cached-token split) is proven.
    let expected = Catalog::seed()
        .resolve("gpt-5.5")
        .unwrap()
        .pricing
        .cost_usd(&CompletionUsage {
            reported: true,
            input_tokens: 1000,
            output_tokens: 500,
            cached_input_tokens: 200,
            cache_write_tokens: 0,
            reasoning_tokens: None,
        });
    assert!(result.cost_usd > 0.0, "cost must be non-zero");
    assert_eq!(result.cost_usd, expected);
}

#[tokio::test]
async fn complete_maps_5xx_to_retryable_transport() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
        .mount(&server)
        .await;

    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5").with_base_url(server.uri());
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
    assert!(matches!(err, ProviderError::Transport { .. }));
    assert!(err.is_retryable(), "5xx must be retryable");
}

#[tokio::test]
async fn complete_returns_err_on_response_failed_not_truncated_ok() {
    let server = MockServer::start().await;
    // Text arrives, then `response.failed`: the turn must error, not
    // return the partial "Hel".
    let sse_body = concat!(
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hel\"}\n\n",
        "event: response.failed\n",
        "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"server_error\",\"message\":\"upstream failure\"}}}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5").with_base_url(server.uri());
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
    // server_error ⇒ retryable Transport.
    assert!(matches!(err, ProviderError::Transport { .. }));
    assert!(err.is_retryable());
}

/// Hitting the output cap must arrive as `FinishReason::Length`, not as a
/// terminal error.
///
/// This test previously asserted the opposite, and that assertion was the
/// bug held in place: every sibling adapter (zai, Anthropic, Gemini/Vertex,
/// Bedrock) reports a cap hit as `Length`, and the driver answers it with
/// an in-turn continuation. Returning `Terminal` made this one dialect the
/// place where the same event killed the turn outright — and
/// non-retryably — so identical work on an identical model succeeded or
/// died depending only on which adapter carried the stream. A provider's
/// wire shape must not decide engine policy.
#[tokio::test]
async fn output_cap_arrives_as_length_not_a_terminal_error() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n",
        "event: response.incomplete\n",
        "data: {\"type\":\"response.incomplete\",\"response\":{\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5").with_base_url(server.uri());
    let req = CompletionRequest {
        messages: vec![CompletionMessage::user("hi")],
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
        .expect("cap hit is not an error");
    assert_eq!(result.finish_reason, Some(FinishReason::Length));
    // The partial survives: it is what the continuation resumes from.
    assert_eq!(result.text, "partial");
}

/// The other half of the same contract. Only the cap is "keep going" —
/// a content filter is a stop, and continuing past one would trade a loud
/// refusal for a silent retry loop.
#[tokio::test]
async fn a_non_cap_incompletion_is_still_terminal() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n",
        "event: response.incomplete\n",
        "data: {\"type\":\"response.incomplete\",\"response\":{\"incomplete_details\":{\"reason\":\"content_filter\"}}}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5").with_base_url(server.uri());
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
    match err {
        ProviderError::Terminal(msg) => assert!(msg.contains("content_filter"), "{msg}"),
        other => panic!("expected Terminal incomplete error, got {other:?}"),
    }
}

/// The clean-EOF twin of the tests above: a well-formed stream that
/// simply ENDS without `response.completed` (close-delimited proxies,
/// LM-Studio-style local gateways, LB idle-reaps surface a dropped
/// connection as clean EOF, not a reqwest error) must fail as a
/// retryable Transport disconnect — never commit the partial "Hel" as a
/// successful completion.
#[tokio::test]
async fn complete_returns_transport_err_on_clean_eof_without_completed() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hel\"}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5").with_base_url(server.uri());
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
        matches!(err, ProviderError::Transport { .. }),
        "expected Transport, got {err:?}"
    );
    assert!(err.is_retryable(), "a disconnect must be retryable");
    let msg = err.to_string();
    assert!(
        msg.contains("response.completed"),
        "names the missing terminal event: {msg}"
    );
}

/// Minimal happy-path SSE body for tests that only inspect the request.
const OK_SSE: &str = concat!(
    "event: response.output_text.delta\n",
    "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n",
    "event: response.completed\n",
    "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
);

async fn mock_ok(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(OK_SSE, "text/event-stream"))
        .mount(server)
        .await;
}

async fn first_request_body(server: &MockServer) -> String {
    let requests = server.received_requests().await.expect("recorded requests");
    String::from_utf8_lossy(&requests[0].body).into_owned()
}

#[tokio::test]
async fn generation_params_forward_top_p_service_tier_and_verbosity() {
    use stella_protocol::GenerationParams;
    let server = MockServer::start().await;
    mock_ok(&server).await;

    // gpt-4.1 is a sampling model, so `top_p` passes the reasoning gate.
    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-4.1").with_base_url(server.uri());
    provider
        .complete(CompletionRequest {
            messages: vec![CompletionMessage::user("hi")],
            max_output_tokens: None,
            temperature: None,
            effort: None,
            tools: vec![],
            reasoning: None,
            params: Some(GenerationParams {
                top_p: Some(0.9),
                // No Responses API slot — silently dropped, never a 400.
                top_k: Some(40),
                frequency_penalty: None,
                presence_penalty: None,
                repetition_penalty: None,
                seed: None,
                verbosity: Some(stella_protocol::Verbosity::Low),
                service_tier: Some(stella_protocol::ServiceTier::Priority),
            }),
        })
        .await
        .expect("should succeed");

    let body = first_request_body(&server).await;
    assert!(body.contains("\"top_p\":0.9"), "{body}");
    assert!(body.contains("\"service_tier\":\"priority\""), "{body}");
    assert!(body.contains("\"text\":{\"verbosity\":\"low\"}"), "{body}");
    assert!(!body.contains("top_k"), "{body}");
}

/// `top_p` rides the same reasoning-model gate as `temperature`: the
/// gpt-5 family rejects sampling parameters with HTTP 400, so a caller
/// override must be dropped there — while the non-sampling routing hint
/// (`service_tier`) still goes through.
#[tokio::test]
async fn top_p_is_omitted_for_a_reasoning_model_like_temperature() {
    use stella_protocol::GenerationParams;
    let server = MockServer::start().await;
    mock_ok(&server).await;

    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5").with_base_url(server.uri());
    provider
        .complete(CompletionRequest {
            messages: vec![CompletionMessage::user("hi")],
            max_output_tokens: None,
            temperature: None,
            effort: None,
            tools: vec![],
            reasoning: None,
            params: Some(GenerationParams {
                top_p: Some(0.9),
                service_tier: Some(stella_protocol::ServiceTier::Flex),
                ..Default::default()
            }),
        })
        .await
        .expect("should succeed");

    let body = first_request_body(&server).await;
    assert!(!body.contains("top_p"), "{body}");
    assert!(body.contains("\"service_tier\":\"flex\""), "{body}");
}

/// An explicit `reasoning: Some(false)` must win over a pinned effort —
/// the caller asked for thinking OFF, so no `reasoning` object rides.
#[tokio::test]
async fn reasoning_false_suppresses_the_reasoning_object_even_with_effort() {
    let server = MockServer::start().await;
    mock_ok(&server).await;

    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5").with_base_url(server.uri());
    provider
        .complete(CompletionRequest {
            messages: vec![CompletionMessage::user("hi")],
            max_output_tokens: None,
            temperature: None,
            effort: Some(ReasoningEffort::High),
            tools: vec![],
            reasoning: Some(false),
            params: None,
        })
        .await
        .expect("should succeed");

    let body = first_request_body(&server).await;
    assert!(!body.contains("\"reasoning\""), "{body}");
}

/// A bare `Some(true)` with no effort turns thinking on at the API's
/// middle tier rather than silently doing nothing.
#[tokio::test]
async fn reasoning_true_without_effort_defaults_to_medium() {
    let server = MockServer::start().await;
    mock_ok(&server).await;

    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5").with_base_url(server.uri());
    provider
        .complete(CompletionRequest {
            messages: vec![CompletionMessage::user("hi")],
            max_output_tokens: None,
            temperature: None,
            effort: None,
            tools: vec![],
            reasoning: Some(true),
            params: None,
        })
        .await
        .expect("should succeed");

    let body = first_request_body(&server).await;
    assert!(
        body.contains("\"reasoning\":{\"effort\":\"medium\"}"),
        "{body}"
    );
}

/// The prompt-cache stability contract: a request without params or a
/// reasoning preference serializes with none of the new keys.
#[tokio::test]
async fn absent_params_and_reasoning_add_no_keys_to_the_body() {
    let server = MockServer::start().await;
    mock_ok(&server).await;

    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5").with_base_url(server.uri());
    provider
        .complete(CompletionRequest {
            messages: vec![CompletionMessage::user("hi")],
            max_output_tokens: None,
            temperature: None,
            effort: None,
            tools: vec![],
            reasoning: None,
            params: None,
        })
        .await
        .expect("should succeed");

    let body = first_request_body(&server).await;
    for key in ["top_p", "service_tier", "verbosity", "\"reasoning\""] {
        assert!(!body.contains(key), "unexpected `{key}` in: {body}");
    }
}

/// The Responses API defaults `store` to `true`, retaining the whole
/// replayed conversation server-side. A BYOK agent must opt out on every
/// request — the wire body has to carry `store: false` explicitly, since
/// omitting the field IS the retention default.
#[tokio::test]
async fn every_request_opts_out_of_server_side_storage() {
    let server = MockServer::start().await;
    mock_ok(&server).await;

    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5").with_base_url(server.uri());
    provider
        .complete(CompletionRequest {
            messages: vec![CompletionMessage::user("hi")],
            max_output_tokens: None,
            temperature: None,
            effort: None,
            tools: vec![],
            reasoning: None,
            params: None,
        })
        .await
        .expect("should succeed");

    let body = first_request_body(&server).await;
    assert!(body.contains("\"store\":false"), "{body}");
}

/// The engine branches on `finish_reason` (the driver's truncation
/// diagnostics, and anything downstream distinguishing "the model wants a
/// tool" from "the model is done"). OpenAI used to hard-code `None`, so it
/// was the one provider where that distinction was unavailable — both
/// mappings are pinned here.
#[tokio::test]
async fn complete_maps_finish_reason_to_stop_and_tool_calls() {
    let server = MockServer::start().await;
    mock_ok(&server).await;
    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5").with_base_url(server.uri());
    let plain = provider
        .complete(CompletionRequest {
            messages: vec![CompletionMessage::user("hi")],
            max_output_tokens: None,
            temperature: None,
            effort: None,
            tools: vec![],
            reasoning: None,
            params: None,
        })
        .await
        .expect("should succeed");
    assert_eq!(plain.finish_reason, Some(FinishReason::Stop));

    let tool_server = MockServer::start().await;
    let sse_body = concat!(
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"read_file\",\"arguments\":\"\"}}\n\n",
        "event: response.function_call_arguments.delta\n",
        "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{}\"}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&tool_server)
        .await;
    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5").with_base_url(tool_server.uri());
    let called = provider
        .complete(CompletionRequest {
            messages: vec![CompletionMessage::user("read it")],
            max_output_tokens: None,
            temperature: None,
            effort: None,
            tools: vec![ToolSchema {
                name: "read_file".into(),
                description: "Read a file".into(),
                input_schema: serde_json::json!({"type":"object"}),
                read_only: true,
                speculation_safe: false,
            }],
            reasoning: None,
            params: None,
        })
        .await
        .expect("should succeed");
    assert_eq!(called.finish_reason, Some(FinishReason::ToolCalls));
}

/// `prompt_cache_key` exists to keep one session's turns on one cache
/// shard AND to keep fleet siblings on *different* ones. A pid+nanos key
/// alone cannot promise the second: two providers built back-to-back can
/// read the same nanosecond, and identical keys serialize the whole fleet
/// onto one shard — the opposite of the point. Driven through
/// `prompt_cache_key_at` because a real clock will not hand the same
/// instant to two calls on demand.
#[test]
fn siblings_minted_in_the_same_nanosecond_get_distinct_prompt_cache_keys() {
    const SAME_INSTANT: u128 = 1_700_000_000_000_000_000;
    let a = prompt_cache_key_at(SAME_INSTANT);
    let b = prompt_cache_key_at(SAME_INSTANT);
    assert_ne!(a, b, "same-nanos siblings must not share a cache shard");
}

/// The end-to-end pin: every construction actually routes through the
/// sequenced helper. Mirrors zai.rs's
/// `distinct_provider_constructions_get_distinct_session_ids`.
#[test]
fn distinct_provider_constructions_get_distinct_prompt_cache_keys() {
    let keys: std::collections::BTreeSet<String> = (0..3)
        .map(|_| OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5").prompt_cache_key)
        .collect();
    assert_eq!(keys.len(), 3, "every construction mints a distinct key");
}

/// Pricing is resolved SCOPED to `openai`. The unscoped `Catalog::resolve`
/// documents "the first row wins", and after `stella models refresh`
/// merges the models.dev master list the same slug legitimately appears
/// under several providers — so an unscoped lookup could cost an OpenAI
/// turn at a gateway's list price with no symptom.
#[test]
fn pricing_is_scoped_to_openai_and_never_adopts_another_providers_row() {
    let ours = OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5");
    assert!(ours.pricing.is_some(), "own rows still resolve");

    let foreign = OpenAiProvider::new(ApiKey::new("sk-test"), "glm-5.2");
    assert!(
        foreign.pricing.is_none(),
        "a Z.ai row must never price an OpenAI turn"
    );
}
