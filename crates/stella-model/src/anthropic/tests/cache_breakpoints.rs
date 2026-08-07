//! The two message-level cache breakpoints (#1837) and the prompt-cache
//! TTL window they ask for (#1839).
//!
//! A sibling module rather than more lines in `tests.rs`, which is a
//! grandfathered god file closed to growth.

use super::*;

/// The default-window marker most of these tests stamp with.
fn marker() -> AnthropicCacheControl {
    ephemeral_cache(CacheTtl::FiveMinutes)
}

/// **Witness (#1837).** The second request must anchor a breakpoint at the
/// position the FIRST request wrote the cache to.
///
/// The adapter placed two breakpoints — the system block (covering tools via
/// Anthropic's cache hierarchy) and the newest content block. The second sits
/// on content that did not exist on the previous request, so nothing anchored
/// the position the previous turn actually wrote to, and the conversation tier
/// was served only by Anthropic's bounded automatic lookback (~20 content
/// blocks).
///
/// That bound is easy to exceed: one fan-out step emits ~11 content blocks
/// (a `tool_use` + a `tool_result` per call), so two steps push the previous
/// write out of lookback and the whole replayed history re-bills at the full
/// input rate and re-writes at 1.25x. Anthropic allows four breakpoints; two
/// were simply unused.
#[test]
fn the_next_request_re_anchors_the_previous_requests_tail() {
    let text = |s: &str| AnthropicContentBlock::Text {
        text: s.to_string(),
        cache_control: None,
    };
    let msg = |role, blocks| AnthropicMessage {
        role,
        content: blocks,
    };

    // Turn one: two messages. The tail lands on the newest block.
    let mut first = vec![
        msg("user", vec![text("ask")]),
        msg("assistant", vec![text("answer one")]),
    ];
    let remembered = stamp_tail_cache_breakpoint(&mut first, marker()).expect("a stampable tail");
    assert_eq!(remembered, (1, 0), "the newest stampable block");

    // Turn two: the history has grown. Both breakpoints are placed.
    let mut second = vec![
        msg("user", vec![text("ask")]),
        msg("assistant", vec![text("answer one")]),
        msg("user", vec![text("ask again")]),
        msg("assistant", vec![text("answer two")]),
    ];
    stamp_remembered_tail(&mut second, remembered, marker());
    let tail = stamp_tail_cache_breakpoint(&mut second, marker()).expect("a stampable tail");

    assert_ne!(
        tail, remembered,
        "the two message breakpoints must be different positions — spending \
         both on one anchor is the bug, not the fix"
    );
    let stamped: Vec<(usize, usize)> = second
        .iter()
        .enumerate()
        .flat_map(|(mi, m)| {
            m.content.iter().enumerate().filter_map(move |(bi, b)| {
                matches!(
                    b,
                    AnthropicContentBlock::Text {
                        cache_control: Some(_),
                        ..
                    }
                )
                .then_some((mi, bi))
            })
        })
        .collect();
    assert_eq!(
        stamped,
        vec![(1, 0), (3, 0)],
        "one anchor at the previous turn's write position, one at the new tail"
    );
}

/// A remembered position that no longer holds a stampable block degrades to
/// the old behaviour — one message breakpoint — rather than panicking or
/// marking the wrong kind of block.
///
/// Reachable whenever compaction rewrites or evicts under the position. A
/// `cache_control` marker is an ANCHOR, not an assertion: the worst case here
/// is a cache write, never a wrong answer, which is why a remembered position
/// is preferred to hashing every block on every request.
#[test]
fn a_stale_remembered_position_is_a_no_op_not_a_panic() {
    let mut messages = vec![AnthropicMessage {
        role: "user",
        content: vec![AnthropicContentBlock::Image {
            source: AnthropicMediaSource::base64("image/png", "aGk=".into()),
        }],
    }];

    // Past the end entirely, and onto a block this schema cannot mark.
    stamp_remembered_tail(&mut messages, (99, 99), marker());
    stamp_remembered_tail(&mut messages, (0, 0), marker());

    let body = serde_json::to_string(&messages).expect("serializes");
    assert!(
        !body.contains("cache_control"),
        "nothing stampable, nothing stamped: {body}"
    );
}

/// A minimal SSE reply for the wire-shape tests below — the assertions are
/// about the REQUEST; the response only needs to parse.
fn minimal_sse() -> &'static str {
    concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    )
}

fn simple_request() -> CompletionRequest {
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

/// **Witness (#1839).** A session configured for the 1-hour window must put
/// BOTH halves of the opt-in on the wire: `ttl: "1h"` on the cache markers
/// and the `extended-cache-ttl-2025-04-11` beta header that unlocks the
/// field. Either half alone silently buys nothing — the field without the
/// header is rejected, the header without the field changes no window.
#[tokio::test]
async fn one_hour_ttl_sends_the_ttl_field_and_the_beta_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("anthropic-beta", "extended-cache-ttl-2025-04-11"))
        .and(body_string_contains("\"ttl\":\"1h\""))
        .respond_with(ResponseTemplate::new(200).set_body_raw(minimal_sse(), "text/event-stream"))
        .expect(1)
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(ApiKey::new("sk-test"), "claude-fable-5")
        .with_base_url(server.uri())
        .with_cache_ttl(CacheTtl::OneHour);
    provider
        .complete(simple_request())
        .await
        .expect("the mock only matches a request carrying both TTL halves");
}

/// **Witness (#1839).** The default window sends today's exact request: no
/// `ttl` field anywhere and no `anthropic-beta` header. This is the
/// byte-stability half of the contract (invariant 7) — a user who never
/// touched the knob must keep their existing cached prefixes and request
/// shape.
#[tokio::test]
async fn the_default_window_sends_no_ttl_field_and_no_beta_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(minimal_sse(), "text/event-stream"))
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(ApiKey::new("sk-test"), "claude-fable-5")
        .with_base_url(server.uri());
    provider
        .complete(simple_request())
        .await
        .expect("should succeed");

    let requests = server.received_requests().await.expect("recording is on");
    assert_eq!(requests.len(), 1);
    let sent = &requests[0];
    assert!(
        sent.headers.get("anthropic-beta").is_none(),
        "no beta header on the default window"
    );
    let body = String::from_utf8(sent.body.clone()).expect("utf8 body");
    assert!(
        body.contains("cache_control"),
        "the breakpoints themselves must still be present: {body}"
    );
    assert!(
        !body.contains("\"ttl\""),
        "no ttl field on the default window: {body}"
    );
}
