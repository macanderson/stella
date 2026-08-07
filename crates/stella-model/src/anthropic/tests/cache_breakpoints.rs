//! The two message-level cache breakpoints (#1837).
//!
//! A sibling module rather than more lines in `tests.rs`, which is a
//! grandfathered god file closed to growth.

use super::super::{
    AnthropicContentBlock, AnthropicMediaSource, AnthropicMessage, stamp_remembered_tail,
    stamp_tail_cache_breakpoint,
};

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
    let remembered = stamp_tail_cache_breakpoint(&mut first).expect("a stampable tail");
    assert_eq!(remembered, (1, 0), "the newest stampable block");

    // Turn two: the history has grown. Both breakpoints are placed.
    let mut second = vec![
        msg("user", vec![text("ask")]),
        msg("assistant", vec![text("answer one")]),
        msg("user", vec![text("ask again")]),
        msg("assistant", vec![text("answer two")]),
    ];
    stamp_remembered_tail(&mut second, remembered);
    let tail = stamp_tail_cache_breakpoint(&mut second).expect("a stampable tail");

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
    stamp_remembered_tail(&mut messages, (99, 99));
    stamp_remembered_tail(&mut messages, (0, 0));

    let body = serde_json::to_string(&messages).expect("serializes");
    assert!(
        !body.contains("cache_control"),
        "nothing stampable, nothing stamped: {body}"
    );
}
