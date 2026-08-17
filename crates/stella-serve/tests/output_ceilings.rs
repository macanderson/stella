// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A served session carries what it learned about what its account can fund
//! (#3568).
//!
//! #3307 gave the output-budget clamp a session lifetime, and `stella-cli`
//! attaches the handle that carries it. `stella-serve` did not: every served
//! turn re-sent the configured ceiling, ate the affordability 402, and only
//! then retried reduced — pure latency, once per turn, for as long as the
//! balance stayed low.
//!
//! The shape of the witness is the counting one, because that is the only
//! thing that can tell a handle that carries from a handle that is merely
//! attached: a `Some(Default::default())` minted per turn would satisfy every
//! `is_some()` assertion and still spend the wasted round-trip. Two turns of
//! one session against a host that refuses any ask above what it can afford
//! spend **three** provider calls, not four.

mod common;

use std::time::Duration;

use common::{TOKEN, next_event, open_sse, post_json};
use serde_json::json;

/// What the scripted host can fund, and the ceiling the turns ask for.
const AFFORDABLE: u32 = 117_676;
const CONFIGURED_CEILING: u32 = 128_000;

async fn create_session(addr: std::net::SocketAddr) -> String {
    let (status, body) = post_json(
        addr,
        "/v1/sessions",
        Some(TOKEN),
        &json!({ "system_prompt": "You are stella." }).to_string(),
    )
    .await;
    assert!(status.contains("200"), "create session: {status} {body}");
    serde_json::from_str::<serde_json::Value>(&body).unwrap()["session_id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Start one turn of `session_id` asking for [`CONFIGURED_CEILING`] output
/// tokens.
async fn create_turn(addr: std::net::SocketAddr, session_id: &str, prompt: &str) -> String {
    let create = json!({
        "provider_id": "mock",
        "tools": [],
        "input": [{ "role": "user", "content": prompt }],
        "reverse_request_timeout_ms": 20_000,
        "engine": { "max_output_tokens": CONFIGURED_CEILING },
    })
    .to_string();
    let (status, body) = post_json(
        addr,
        &format!("/v1/sessions/{session_id}/turns"),
        Some(TOKEN),
        &create,
    )
    .await;
    assert!(status.contains("200"), "create turn: {status} {body}");
    serde_json::from_str::<serde_json::Value>(&body).unwrap()["turn_id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Drive one turn to completion playing a gateway that prices the *ask*:
/// anything above [`AFFORDABLE`] is refused with the 402 the engine's output
/// budget recovery keys on, and the engine is left to repair it.
///
/// Returns every ceiling the turn asked for, in order.
async fn run_turn_answering_affordably(
    addr: std::net::SocketAddr,
    turn_id: &str,
) -> Vec<Option<u64>> {
    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;
    let mut asked = Vec::new();
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), next_event(&mut sse))
            .await
            .expect("the turn must make progress")
            .expect("the stream must not end before turn_complete");
        match frame["type"].as_str() {
            Some("provider_request") => {
                let ask = frame["request"]["max_output_tokens"].as_u64();
                asked.push(ask);
                let outcome = if ask.is_none_or(|ask| ask > u64::from(AFFORDABLE)) {
                    json!({
                        "status": "error",
                        "error": {
                            "kind": "output_budget_exceeded",
                            "message": "This request requires more credits, or fewer \
                                        max_tokens.",
                            "affordable_output_tokens": AFFORDABLE,
                        },
                    })
                } else {
                    json!({
                        "status": "ok",
                        "result": common::model_result("recovered"),
                    })
                };
                let mut body = outcome;
                body["request_id"] = frame["request_id"].clone();
                let (status, body) = post_json(
                    addr,
                    &format!("/v1/turns/{turn_id}/provider-result"),
                    Some(TOKEN),
                    &body.to_string(),
                )
                .await;
                assert!(status.contains("200"), "provider-result: {status} {body}");
            }
            Some("turn_complete") => {
                assert_eq!(
                    frame["outcome"]["status"], "completed",
                    "the refusal must be repaired, not fatal: {frame}"
                );
                break;
            }
            _ => {}
        }
    }
    asked
}

/// The carry, counted. Turn 1 learns the hard way — two calls, the second at
/// the clamp. Turn 2 must open *at* the clamp, so the session spends three
/// provider calls across two turns rather than four.
///
/// On `main` this fails with four: `session_output_ceilings` is left `None`
/// for a served session, so turn 2 re-asks for the configured ceiling and
/// re-pays the 402.
#[tokio::test]
async fn a_served_session_carries_its_learned_output_ceiling_between_turns() {
    let addr = common::start_server().await;
    let session_id = create_session(addr).await;

    let turn1 = create_turn(addr, &session_id, "hi").await;
    let first = run_turn_answering_affordably(addr, &turn1).await;
    assert_eq!(
        first.len(),
        2,
        "turn 1 asks the configured ceiling, is refused, and retries reduced: {first:?}"
    );
    assert_eq!(
        first[0],
        Some(u64::from(CONFIGURED_CEILING)),
        "turn 1's first ask is the configured ceiling: {first:?}"
    );
    let clamp = first[1].expect("the retry names a ceiling");
    assert!(
        clamp <= u64::from(AFFORDABLE),
        "the retry must ask for something the account can fund: {first:?}"
    );

    let turn2 = create_turn(addr, &session_id, "and again").await;
    let second = run_turn_answering_affordably(addr, &turn2).await;
    assert_eq!(
        second,
        vec![Some(clamp)],
        "turn 2 must open at the ceiling turn 1 learned — one call, no second 402"
    );

    assert_eq!(
        first.len() + second.len(),
        3,
        "three provider calls across two turns; four is the pre-#3568 behaviour"
    );
}
