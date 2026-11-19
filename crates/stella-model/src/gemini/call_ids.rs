// SPDX-License-Identifier: AGPL-3.0-only
//! The call ids this adapter mints, since Gemini's wire carries none.
//!
//! The engine hands a call's id to its hooks and to each MCP request it
//! makes. A recorder joins the two on that id. So the id must not repeat from
//! one step to the next, even when the model calls the same tool with the
//! same input again.
//!
//! Declared from `gemini.rs`, so the suite in `gemini/tests.rs` keeps its own
//! subject.

use super::*;

/// One response of one `functionCall` part, with `responseId` when given.
fn response(response_id: Option<&str>, signature: Option<&str>) -> GeminiStreamChunk {
    let mut part = serde_json::json!({
        "functionCall": { "name": "search_code", "args": { "query": "retry policy" } }
    });
    if let Some(sig) = signature {
        part["thoughtSignature"] = sig.into();
    }
    let mut chunk = serde_json::json!({ "candidates": [{ "content": { "parts": [part] } }] });
    if let Some(id) = response_id {
        chunk["responseId"] = id.into();
    }
    serde_json::from_value(chunk).expect("a chunk parses")
}

/// The id of the one call a response holds.
fn minted(chunk: GeminiStreamChunk) -> String {
    let mut assembly = GeminiAssembly::default();
    assembly
        .absorb(chunk, "Gemini", None)
        .expect("the chunk folds");
    assert_eq!(assembly.tool_calls.len(), 1);
    assembly.tool_calls.remove(0).call_id
}

/// The same call, asked for again on the next step, gets a new id.
#[test]
fn the_same_call_in_two_responses_gets_two_ids() {
    let first = minted(response(Some("rsp-A1"), None));
    let again = minted(response(Some("rsp-B2"), None));
    assert_eq!(first, "call_rsp-A1_0");
    assert_eq!(again, "call_rsp-B2_0");
}

/// Two responses with no `responseId` still give their calls two ids.
#[test]
fn responses_with_no_id_give_their_calls_two_ids() {
    let first = minted(response(None, None));
    let again = minted(response(None, None));
    assert_ne!(first, again);
    for id in [&first, &again] {
        assert!(id.starts_with("call_local-"), "{id}");
        assert!(id.ends_with("_0"), "{id}");
    }
}

/// The signature still rides after the separator, and splits back out.
#[test]
fn a_signature_rides_after_the_response_id() {
    let id = minted(response(Some("rsp-A1"), Some("sig123")));
    assert_eq!(id, "call_rsp-A1_0#sig123");
    assert_eq!(split_call_id(&id), ("call_rsp-A1_0", Some("sig123")));
}

/// Two `responseId`s that differ only in characters a call id cannot carry
/// give their calls two ids. Neither id moves the signature split.
#[test]
fn response_ids_that_differ_in_odd_characters_stay_apart() {
    let first = minted(response(Some("a#b"), Some("sig123")));
    let again = minted(response(Some("a/b"), Some("sig123")));
    let (first_id, first_sig) = split_call_id(&first);
    let (again_id, again_sig) = split_call_id(&again);
    assert_ne!(first_id, again_id);
    assert_eq!(first_sig, Some("sig123"));
    assert_eq!(again_sig, Some("sig123"));
    assert_eq!(usable_response_id("a#b"), None);
    assert_eq!(usable_response_id(""), None);
}
