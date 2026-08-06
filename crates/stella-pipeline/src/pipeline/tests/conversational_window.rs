//! The conversational fast path's input bound (#1840).
//!
//! Split out rather than appended to `tests.rs`, which is a grandfathered
//! god file closed to growth — the sibling-module shape AGENTS.md asks for.

use super::super::{CONVERSATIONAL_HISTORY_MESSAGES, conversational_window};
use stella_protocol::CompletionMessage;

/// **Witness (#1840).** The conversational reply must not be handed the whole
/// transcript.
///
/// `run_conversational` cloned the running history and dispatched all of it
/// with `tools: Vec::new()` and a replaced system message — so not one byte of
/// the prefix matched the worker's cached one, and a 90k-token session where
/// the user types "thanks" re-billed all 90k at the full input rate plus the
/// 1.25x cache-write premium. Every chat interjection in a long session paid
/// it again.
///
/// A pure-function test over the windowing rather than a scripted dispatch:
/// the cost is decided entirely by which messages are selected, and asserting
/// that directly cannot be satisfied by a provider double that happens to be
/// cheap.
#[test]
fn the_conversational_window_bounds_a_long_transcript() {
    let mut history = vec![CompletionMessage::system("worker system prompt")];
    for i in 0..200 {
        history.push(CompletionMessage::user(format!("u{i}")));
        history.push(CompletionMessage::assistant(format!("a{i}")));
    }

    let window = conversational_window(&history);

    assert_eq!(
        window.len(),
        1 + CONVERSATIONAL_HISTORY_MESSAGES,
        "the system message plus a bounded tail, not 401 messages"
    );
    assert_eq!(
        window[0].content, "worker system prompt",
        "the leading system message is kept — the caller's may not be one, and \
         dropping it would destroy the context the window exists to preserve"
    );
    assert_eq!(
        window.last().unwrap().content,
        "a199",
        "and the tail is the MOST RECENT exchange, not the oldest — a window \
         taken from the front would answer about the wrong thing"
    );
}

/// A conversation shorter than the window is untouched: the bound must be
/// invisible for the ordinary case, or it would truncate real context.
#[test]
fn a_short_conversation_passes_through_the_window_whole() {
    let history = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hello"),
        CompletionMessage::assistant("hi"),
    ];
    assert_eq!(conversational_window(&history), history);
}

/// A caller whose history does NOT lead with a system message keeps every
/// message it had. `run` seeds one when the history is empty, but nothing
/// enforces it — and treating a non-system first message as droppable is how
/// the window would silently eat the very context it is meant to keep.
#[test]
fn a_history_with_no_system_message_keeps_its_first_message() {
    let history = vec![
        CompletionMessage::user("the only instruction this caller gave"),
        CompletionMessage::assistant("ok"),
    ];
    let window = conversational_window(&history);
    assert_eq!(window, history);
    assert_eq!(window[0].content, "the only instruction this caller gave");
}
