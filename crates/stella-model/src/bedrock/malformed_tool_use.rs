// SPDX-License-Identifier: AGPL-3.0-only
//! What a `toolUse` block does to a turn when its id is missing.
//!
//! Converse answers once, so this adapter has one delivery path and one
//! parse. A block with no `toolUseId` fails that parse outright unless the
//! field defaults. The whole body goes with it, and the turn dies on a serde
//! message that names neither the tool nor the block it sat in. A reader
//! cannot tell that failure from a truncated body or a changed schema.
//!
//! The field now defaults, so the block parses and assembly refuses it by
//! name. The error says which block, which tool, and which field.
//!
//! Declared from `bedrock.rs`, so the suite in `bedrock/tests.rs` keeps its
//! own subject.

use super::*;
use stella_protocol::CompletionRequest;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn request() -> CompletionRequest {
    CompletionRequest {
        messages: vec![CompletionMessage::user("list the files")],
        max_output_tokens: Some(1024),
        temperature: None,
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    }
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

/// The witness. Without the fix the error is serde's own `missing field
/// toolUseId`, which names no tool and no block, so the assertions about
/// `index 0` and `bash` are the ones that fail.
#[tokio::test]
async fn a_tool_use_block_with_no_id_names_the_call_it_lost() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "output": {"message": {"role": "assistant", "content": [
                {"toolUse": {"name": "bash", "input": {"command": "ls"}}}
            ]}},
            "stopReason": "tool_use",
            "usage": {"inputTokens": 9, "outputTokens": 5}
        })))
        .mount(&server)
        .await;
    let provider = test_provider(&server.uri());

    let error = provider
        .complete(request())
        .await
        .expect_err("a call that cannot be dispatched must not be committed");

    let message = error.to_string();
    assert!(
        message.contains("index 0"),
        "the error names the block that was lost: {message}"
    );
    assert!(
        message.contains("`toolUseId`"),
        "the error names the field that never arrived: {message}"
    );
    assert!(
        message.contains("bash"),
        "the error names the tool the model asked for: {message}"
    );
    assert!(
        !error.is_retryable(),
        "the same request returns the same absent field: {error:?}"
    );
}

/// The constraint on the fix: an ordinary call still rides through. Only a
/// block that cannot be dispatched is fatal.
#[tokio::test]
async fn a_tool_use_block_with_an_id_still_completes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "output": {"message": {"role": "assistant", "content": [
                {"toolUse": {"toolUseId": "tooluse_1", "name": "bash", "input": {"command": "ls"}}}
            ]}},
            "stopReason": "tool_use",
            "usage": {"inputTokens": 9, "outputTokens": 5}
        })))
        .mount(&server)
        .await;
    let provider = test_provider(&server.uri());

    let result = provider.complete(request()).await.expect("a whole call");
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0].call_id, "tooluse_1");
    assert_eq!(result.tool_calls[0].name, "bash");
    assert_eq!(result.finish_reason, Some(FinishReason::ToolCalls));
}
