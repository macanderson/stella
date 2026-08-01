use super::*;
use stella_protocol::CompletionRequest;
use stella_protocol::tool::ToolSchema;
use stella_protocol::{ToolOutput, ToolResult};
use wiremock::matchers::{body_string_contains, header_regex, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The audio arm of [`attachment_block`] is out of reach only because
/// `BEDROCK_CAPS` switches audio off. Flipping that bool is a routine
/// one-line edit, so the arm it lands in has to degrade like every other
/// unsupported attachment instead of aborting the process mid-turn.
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
    let attachment = Attachment {
        name: "song.mp3".into(),
        media_type: "audio/mpeg".into(),
        byte_len: 3,
        source: AttachmentSource::Data {
            base64: "YWJj".into(),
        },
    };
    let blocks: Vec<_> = wire_parts(&[attachment], flipped)
        .into_iter()
        .map(attachment_block)
        .collect();
    let [block] = blocks.as_slice() else {
        panic!("expected one degrade block, got {blocks:?}");
    };
    let note = block
        .text
        .as_deref()
        .unwrap_or_else(|| panic!("expected a text degrade note, got {block:?}"));
    assert!(
        note.contains("audio/mpeg"),
        "the note names what was attached: {note}"
    );
}

#[test]
fn user_attachments_map_to_converse_blocks_with_format_allowlists() {
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
            att("spec v2.pdf", "application/pdf", "cGRm"),
            att("d.mov", "video/quicktime", "dmlk"),
            att("weird.tiff", "image/tiff", "dGlm"),
        ],
    )];
    let (_, mapped) = to_bedrock_messages(&messages);
    assert_eq!(mapped.len(), 1);
    let json = serde_json::to_value(&mapped[0]).unwrap();
    let content = json["content"].as_array().unwrap();
    assert_eq!(content.len(), 5, "{json}");
    assert_eq!(content[0]["image"]["format"], "png");
    assert_eq!(content[0]["image"]["source"]["bytes"], "aW1n");
    // Document name sanitized to Converse's allowed character set
    // (the period is not allowed).
    assert_eq!(content[1]["document"]["format"], "pdf");
    assert_eq!(content[1]["document"]["name"], "spec v2-pdf");
    assert_eq!(content[2]["video"]["format"], "mov");
    // TIFF is outside the Converse image allowlist: degrade note.
    assert!(
        content[3]["text"].as_str().unwrap().contains("image/tiff"),
        "{json}"
    );
    assert_eq!(content[4]["text"], "look");
}

#[test]
fn cache_points_are_gated_to_supporting_model_families() {
    assert!(supports_cache_points(
        "us.anthropic.claude-sonnet-4-5-20250929-v1:0"
    ));
    assert!(supports_cache_points("us.amazon.nova-pro-v1:0"));
    assert!(!supports_cache_points("meta.llama4-maverick-17b-v1:0"));
}

fn test_provider(server_uri: &str) -> BedrockProvider {
    BedrockProvider::new(
        ApiKey::new("AKIDEXAMPLE"),
        ApiKey::new("wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY"),
        None,
        "us-east-1",
        "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
    )
    .with_base_url(server_uri.to_string())
}

#[test]
fn to_bedrock_messages_hoists_system_and_frames_tool_round_trips() {
    let messages = vec![
        CompletionMessage::system("You are a coding agent."),
        CompletionMessage::user("read a.rs"),
        CompletionMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            tool_calls: vec![ToolCall {
                call_id: "tooluse_abc".into(),
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
                call_id: "tooluse_abc".into(),
                output: ToolOutput::Ok {
                    content: "fn main(){}".into(),
                },
            }],
            attachments: Vec::new(),
        },
    ];
    let (system, mapped) = to_bedrock_messages(&messages);
    assert_eq!(system.len(), 1);
    assert_eq!(mapped.len(), 3);
    assert_eq!(mapped[1].role, "assistant");
    let tool_use = mapped[1].content[0].tool_use.as_ref().unwrap();
    assert_eq!(tool_use.tool_use_id, "tooluse_abc");
    // Converse frames tool results as a user-role message.
    assert_eq!(mapped[2].role, "user");
    let tool_result = mapped[2].content[0].tool_result.as_ref().unwrap();
    assert_eq!(tool_result.tool_use_id, "tooluse_abc");
    assert_eq!(tool_result.content[0].text, "fn main(){}");
    assert_eq!(tool_result.status, None);
}

#[test]
fn failed_tool_results_carry_error_status_not_a_text_prefix() {
    let messages = vec![CompletionMessage {
        role: MessageRole::Tool,
        content: String::new(),
        tool_calls: vec![],
        tool_results: vec![ToolResult {
            call_id: "tooluse_1".into(),
            output: ToolOutput::Error {
                message: "no such file".into(),
            },
        }],
        attachments: Vec::new(),
    }];
    let (_, mapped) = to_bedrock_messages(&messages);
    let tool_result = mapped[0].content[0].tool_result.as_ref().unwrap();
    assert_eq!(tool_result.status, Some("error"));
    assert_eq!(tool_result.content[0].text, "no such file");
}

#[tokio::test]
async fn complete_posts_a_signed_converse_request_and_parses_text() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(
            "/model/us.anthropic.claude-sonnet-4-5-20250929-v1%3A0/converse",
        ))
        .and(header_regex(
            "authorization",
            r"^AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/\d{8}/us-east-1/bedrock/aws4_request, SignedHeaders=content-type;host;x-amz-date, Signature=[0-9a-f]{64}$",
        ))
        .and(header_regex("x-amz-date", r"^\d{8}T\d{6}Z$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "output": {"message": {"role": "assistant", "content": [{"text": "Hello from Bedrock"}]}},
            "stopReason": "end_turn",
            "usage": {"inputTokens": 9, "outputTokens": 5, "cacheReadInputTokens": 3, "cacheWriteInputTokens": 2}
        })))
        .mount(&server)
        .await;

    let provider = test_provider(&server.uri());
    let req = CompletionRequest {
        messages: vec![CompletionMessage::user("hi")],
        max_output_tokens: Some(1024),
        temperature: Some(0.0),
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    };

    let result = provider
        .complete(req)
        .await
        .expect("completion should succeed");
    assert_eq!(result.text, "Hello from Bedrock");
    // Converse reports cache reads OUTSIDE `inputTokens`; the adapter
    // folds them back in so cached stays a subset of input (9 + 3).
    assert_eq!(result.usage.input_tokens, 12);
    assert_eq!(result.usage.output_tokens, 5);
    assert_eq!(result.usage.cached_input_tokens, 3);
    assert_eq!(result.usage.cache_write_tokens, 2);
    assert!(result.usage.reported);
}

/// Prompt caching on Converse is opt-in via `cachePoint` blocks: one
/// after the system blocks (tools+system tier) and one at the tail of
/// the last message (conversation-incremental tier). Without them
/// Bedrock caches nothing.
#[tokio::test]
async fn complete_sends_cache_points_for_claude_models() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains(
            "\"cachePoint\":{\"type\":\"default\"}",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "output": {"message": {"role": "assistant", "content": [{"text": "ok"}]}},
            "stopReason": "end_turn",
            "usage": {"inputTokens": 4, "outputTokens": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let provider = test_provider(&server.uri());
    let req = CompletionRequest {
        messages: vec![
            CompletionMessage::system("You are a coding agent."),
            CompletionMessage::user("hi"),
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
    assert_eq!(result.text, "ok");
}

#[tokio::test]
async fn complete_parses_tool_use_blocks_into_tool_calls() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "output": {"message": {"role": "assistant", "content": [
                {"text": "Let me read that."},
                {"toolUse": {"toolUseId": "tooluse_xyz", "name": "read_file", "input": {"path": "src/lib.rs"}}}
            ]}},
            "stopReason": "tool_use",
            "usage": {"inputTokens": 30, "outputTokens": 12}
        })))
        .mount(&server)
        .await;

    let provider = test_provider(&server.uri());
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
    assert_eq!(result.text, "Let me read that.");
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0].call_id, "tooluse_xyz");
    assert_eq!(result.tool_calls[0].name, "read_file");
    assert_eq!(
        result.tool_calls[0].input,
        serde_json::json!({"path": "src/lib.rs"})
    );
}

/// A no-argument Converse tool call omits `input` entirely, so the
/// `#[serde(default)]` field lands as `Value::Null`. It must surface as
/// an empty object: `Value::Null` is the malformed-call sentinel
/// `driver.rs::execute_with_repair` checks for, and a valid no-arg call
/// reported as null would be wrongly "repaired" (the twin of gemini.rs's
/// `complete_normalizes_a_no_arg_call_to_an_empty_object_not_null`).
#[tokio::test]
async fn complete_normalizes_a_no_arg_tool_use_to_an_empty_object_not_null() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "output": {"message": {"role": "assistant", "content": [
                {"toolUse": {"toolUseId": "tooluse_now", "name": "now"}}
            ]}},
            "stopReason": "tool_use",
            "usage": {"inputTokens": 5, "outputTokens": 2}
        })))
        .mount(&server)
        .await;

    let provider = test_provider(&server.uri());
    let result = provider
        .complete(CompletionRequest {
            messages: vec![CompletionMessage::user("what time is it")],
            max_output_tokens: None,
            temperature: None,
            effort: None,
            tools: vec![],
            reasoning: None,
            params: None,
        })
        .await
        .expect("should succeed");

    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0].name, "now");
    assert_eq!(result.tool_calls[0].input, serde_json::json!({}));
    assert!(!result.tool_calls[0].input.is_null());
}

#[tokio::test]
async fn complete_maps_403_to_auth_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(403).set_body_string(
            "{\"message\":\"The security token included in the request is invalid.\"}",
        ))
        .mount(&server)
        .await;

    let provider = test_provider(&server.uri());
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
async fn complete_maps_429_throttling_to_rate_limited() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429).set_body_string("{\"message\":\"Too many requests\"}"),
        )
        .mount(&server)
        .await;

    let provider = test_provider(&server.uri());
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
    assert!(matches!(err, ProviderError::RateLimited { .. }));
    assert!(err.is_retryable());
}

#[tokio::test]
async fn complete_sends_the_session_token_header_when_configured() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header_regex(
            "x-amz-security-token",
            r"^IQoJb3JpZ2luX2VjEXAMPLETOKEN$",
        ))
        .and(header_regex(
            "authorization",
            r"SignedHeaders=content-type;host;x-amz-date;x-amz-security-token,",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "output": {"message": {"role": "assistant", "content": [{"text": "ok"}]}},
            "usage": {"inputTokens": 1, "outputTokens": 1}
        })))
        .mount(&server)
        .await;

    let provider = BedrockProvider::new(
        ApiKey::new("AKIDEXAMPLE"),
        ApiKey::new("wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY"),
        Some(ApiKey::new("IQoJb3JpZ2luX2VjEXAMPLETOKEN")),
        "us-east-1",
        "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
    )
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

    let result = provider.complete(req).await.expect("should succeed");
    assert_eq!(result.text, "ok");
}

/// Of the `GenerationParams`, only `top_p` has a Converse
/// `inferenceConfig` slot; the rest must be dropped silently (the
/// never-fail contract), not guessed into `additionalModelRequestFields`
/// this adapter doesn't have.
#[tokio::test]
async fn generation_params_forward_only_top_p_into_inference_config() {
    use stella_protocol::GenerationParams;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "output": {"message": {"role": "assistant", "content": [{"text": "ok"}]}},
            "usage": {"inputTokens": 1, "outputTokens": 1}
        })))
        .mount(&server)
        .await;

    let provider = test_provider(&server.uri());
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
                top_k: Some(40),
                seed: Some(7),
                ..Default::default()
            }),
        })
        .await
        .expect("should succeed");

    let requests = server.received_requests().await.expect("recorded requests");
    let body = String::from_utf8_lossy(&requests[0].body);
    assert!(
        body.contains("\"inferenceConfig\":{\"topP\":0.9}"),
        "{body}"
    );
    assert!(!body.contains("topK"), "{body}");
    assert!(!body.contains("seed"), "{body}");
}

/// The byte-stability contract: with no overrides at all, no
/// `inferenceConfig` key rides — exactly the pre-params request body.
#[tokio::test]
async fn absent_params_omit_inference_config_entirely() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "output": {"message": {"role": "assistant", "content": [{"text": "ok"}]}},
            "usage": {"inputTokens": 1, "outputTokens": 1}
        })))
        .mount(&server)
        .await;

    let provider = test_provider(&server.uri());
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

    let requests = server.received_requests().await.expect("recorded requests");
    let body = String::from_utf8_lossy(&requests[0].body);
    assert!(!body.contains("inferenceConfig"), "{body}");
    assert!(!body.contains("topP"), "{body}");
    assert!(!body.contains("additionalModelRequestFields"), "{body}");
}

/// The reasoning switch reaches the wire as `additionalModelRequestFields
/// .reasoning_config` in the Anthropic legacy shape, with the same
/// effort→budget mapping and budget↔max_tokens coupling as
/// `anthropic.rs`'s legacy branch: an uncapped thinking turn raises the
/// ceiling to budget + 8192, and temperature is omitted (the API rejects
/// it alongside thinking).
#[tokio::test]
async fn reasoning_true_sends_reasoning_config_and_raises_max_tokens() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "output": {"message": {"role": "assistant", "content": [{"text": "ok"}]}},
            "usage": {"inputTokens": 1, "outputTokens": 1}
        })))
        .mount(&server)
        .await;

    let provider = test_provider(&server.uri());
    provider
        .complete(CompletionRequest {
            messages: vec![CompletionMessage::user("think hard")],
            max_output_tokens: None,
            temperature: Some(0.3),
            effort: Some(stella_protocol::ReasoningEffort::High),
            tools: vec![],
            reasoning: Some(true),
            params: None,
        })
        .await
        .expect("should succeed");

    let requests = server.received_requests().await.expect("recorded requests");
    let body = String::from_utf8_lossy(&requests[0].body);
    assert!(
        body.contains(
            "\"additionalModelRequestFields\":{\"reasoning_config\":\
             {\"type\":\"enabled\",\"budget_tokens\":16384}}"
        ),
        "{body}"
    );
    assert!(
        body.contains("\"inferenceConfig\":{\"maxTokens\":24576}"),
        "budget + 8192 headroom, temperature omitted with thinking on: {body}"
    );
    assert!(!body.contains("temperature"), "{body}");
}

/// A caller cap at or below the 1024-token thinking floor leaves no legal
/// budget (`budget < max_tokens` is an API requirement) — thinking is
/// omitted rather than sent as a ValidationException, and the request
/// falls back to the plain no-thinking shape (temperature honored).
#[tokio::test]
async fn a_cap_at_the_thinking_floor_omits_reasoning_config_not_a_400() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "output": {"message": {"role": "assistant", "content": [{"text": "ok"}]}},
            "usage": {"inputTokens": 1, "outputTokens": 1}
        })))
        .mount(&server)
        .await;

    let provider = test_provider(&server.uri());
    provider
        .complete(CompletionRequest {
            messages: vec![CompletionMessage::user("think hard")],
            max_output_tokens: Some(1024),
            temperature: Some(0.0),
            effort: None,
            tools: vec![],
            reasoning: Some(true),
            params: None,
        })
        .await
        .expect("should succeed");

    let requests = server.received_requests().await.expect("recorded requests");
    let body = String::from_utf8_lossy(&requests[0].body);
    assert!(!body.contains("additionalModelRequestFields"), "{body}");
    assert!(
        body.contains("\"maxTokens\":1024") && body.contains("\"temperature\":0.0"),
        "{body}"
    );
}

/// Records what an observed call announced, so a test can assert on the
/// difference between "one synthetic delta" and the trait default's
/// total silence.
#[derive(Default)]
struct RecordingObserver {
    deltas: std::sync::Mutex<Vec<String>>,
    calls: std::sync::Mutex<Vec<ToolCall>>,
}

impl ToolCallObserver for RecordingObserver {
    fn tool_call_streamed(&self, call: &ToolCall) {
        self.calls.lock().unwrap().push(call.clone());
    }

    fn text_delta(&self, delta: &str) {
        self.deltas.lock().unwrap().push(delta.to_string());
    }
}

fn observed_request() -> CompletionRequest {
    CompletionRequest {
        messages: vec![CompletionMessage::user("hi")],
        max_output_tokens: Some(1024),
        temperature: Some(0.0),
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    }
}

/// The #1132 acceptance: an observed Bedrock turn announces its answer
/// instead of leaving the observer with nothing. Converse is not a
/// stream, so one delta carrying the whole answer is the finest
/// granularity available — but it is emphatically not zero, which is
/// what the trait's default `complete_observed_ref` would have left a
/// watching host with.
#[tokio::test]
async fn an_observed_turn_emits_the_whole_answer_as_one_delta() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "output": {"message": {"role": "assistant", "content": [{"text": "Hello from Bedrock"}]}},
            "stopReason": "end_turn",
            "usage": {"inputTokens": 9, "outputTokens": 5}
        })))
        .mount(&server)
        .await;

    let provider = test_provider(&server.uri());
    let observer = RecordingObserver::default();
    let result = provider
        .complete_observed(observed_request(), &observer)
        .await
        .expect("completion should succeed");

    assert_eq!(result.text, "Hello from Bedrock");
    assert_eq!(
        observer.deltas.lock().unwrap().as_slice(),
        ["Hello from Bedrock"],
        "the observed path must announce the answer exactly once"
    );
}

/// A tool-call-only turn has no answer text. Announcing a zero-length
/// delta would tell an observer counting deltas that an answer arrived,
/// so the emission is skipped entirely.
///
/// This also pins the deliberate non-announcement of tool calls: with
/// the whole body already parsed, speculation started here would beat
/// dispatch by microseconds while paying a spawn and emitting a
/// discard — see the module docs.
#[tokio::test]
async fn a_tool_only_turn_announces_neither_empty_text_nor_tool_calls() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "output": {"message": {"role": "assistant", "content": [
                {"toolUse": {"toolUseId": "call-1", "name": "read_file", "input": {"path": "a.txt"}}}
            ]}},
            "stopReason": "tool_use",
            "usage": {"inputTokens": 9, "outputTokens": 5}
        })))
        .mount(&server)
        .await;

    let provider = test_provider(&server.uri());
    let observer = RecordingObserver::default();
    let result = provider
        .complete_observed(observed_request(), &observer)
        .await
        .expect("completion should succeed");

    assert_eq!(
        result.tool_calls.len(),
        1,
        "the call still reaches dispatch"
    );
    assert!(
        observer.deltas.lock().unwrap().is_empty(),
        "empty answer text must not be announced as a delta"
    );
    assert!(
        observer.calls.lock().unwrap().is_empty(),
        "Converse cannot announce a call before the body lands, so it does not pretend to"
    );
}

/// A failed observed call must fail, not report an empty answer: the
/// error propagates and nothing is announced.
#[tokio::test]
async fn an_observed_turn_that_fails_announces_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(403).set_body_string("nope"))
        .mount(&server)
        .await;

    let provider = test_provider(&server.uri());
    let observer = RecordingObserver::default();
    let err = provider
        .complete_observed(observed_request(), &observer)
        .await
        .expect_err("403 must surface as an error");

    assert!(matches!(err, ProviderError::Auth(_)), "{err:?}");
    assert!(observer.deltas.lock().unwrap().is_empty());
}
