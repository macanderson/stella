// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `GET /v1/calibration` — the cost-drift report over the wire (#1298).
//!
//! The engine has always compared its own pre-call token estimate against what
//! the provider reported; before this the comparison was unreachable from the
//! API, so a host embedding Stella believed its own cost numbers with nothing
//! able to contradict them. These tests drive a real turn over a real socket
//! and then read the gap back out.

mod common;

use common::*;
use serde_json::json;
use stella_protocol::CompletionMessage;

/// A model answer carrying provider-reported input usage — the "actual" half
/// of the drift comparison. `common::model_result` reports zero, which the
/// estimator ignores as carrying no signal, so this test needs its own.
fn billed_answer(text: &str, input_tokens: u64) -> serde_json::Value {
    json!({
        "text": text,
        "usage": {
            "reported": true,
            "input_tokens": input_tokens,
            "output_tokens": 4,
        },
        "model": "mock-1",
        "cost_usd": 0.0,
    })
}

/// Run one turn to completion, answering its single provider request with a
/// result that reports `input_tokens` billed.
async fn run_billed_turn(addr: std::net::SocketAddr, provider_id: &str, input_tokens: u64) {
    let create = json!({
        "provider_id": provider_id,
        "messages": [serde_json::to_value(CompletionMessage::user("hi")).unwrap()],
        "reverse_request_timeout_ms": 20_000,
    })
    .to_string();
    let (status, body) = post_json(addr, "/v1/turns", Some(TOKEN), &create).await;
    assert!(status.contains("200"), "create: {status} {body}");
    let turn_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["turn_id"]
        .as_str()
        .unwrap()
        .to_string();

    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;
    while let Some(event) = next_event(&mut sse).await {
        match event["type"].as_str().unwrap_or_default() {
            "provider_request" => {
                let answer = json!({
                    "request_id": event["request_id"],
                    "status": "ok",
                    "result": billed_answer("done", input_tokens),
                })
                .to_string();
                let (status, resp) = post_json(
                    addr,
                    &format!("/v1/turns/{turn_id}/provider-result"),
                    Some(TOKEN),
                    &answer,
                )
                .await;
                assert!(status.contains("200"), "provider-result: {status} {resp}");
            }
            "turn_complete" => break,
            _ => {}
        }
    }
}

/// The witness for `calibration.drift` on the API surface: a turn's
/// `(estimated, actual)` pair reaches the process's calibration state, and the
/// gap is readable at `GET /v1/calibration`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_drift_report_is_readable_through_the_api() {
    let addr = start_server().await;

    // Before any turn: an empty list, which is an honest "nothing measured"
    // rather than a zero that would read as "measured, and perfect".
    // `untracked_turns` rides along at zero — nothing has been refused a map,
    // so the empty list above is the whole picture and says so.
    let (status, body) = get_json_authed(addr, "/v1/calibration", TOKEN).await;
    assert!(status.contains("200"), "calibration: {status} {body}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        json!({ "models": [], "untracked_turns": 0 })
    );

    run_billed_turn(addr, "mock", 4_000).await;

    let (status, body) = get_json_authed(addr, "/v1/calibration", TOKEN).await;
    assert!(status.contains("200"), "calibration: {status} {body}");
    let report: serde_json::Value = serde_json::from_str(&body).unwrap();
    let rows = report["models"].as_array().expect("models is a list");
    assert_eq!(rows.len(), 1, "one provider, one model: {report}");

    let row = &rows[0];
    assert_eq!(row["provider_id"], "mock");
    assert_eq!(row["model"], "mock-1");
    assert_eq!(row["samples"], 1);
    assert_eq!(
        row["actual_input_tokens"], 4_000,
        "the provider's own number, verbatim: {row}"
    );
    let estimated = row["estimated_input_tokens"].as_u64().unwrap();
    assert!(
        estimated > 0,
        "the engine's pre-call estimate must be recorded, not just the bill: {row}"
    );
    let ratio = row["drift_ratio"].as_f64().unwrap();
    assert!(
        (ratio - 4_000.0 / estimated as f64).abs() < 1e-9,
        "drift_ratio must be actual/estimated: {row}"
    );
    assert_eq!(
        row["applied_factor"], 1.0,
        "one sample is below the floor the correction needs, so nothing is \
         applied yet — the gap is reported all the same: {row}"
    );
}

/// Two providers must not blend. Keying only by model would average the
/// tokenizers of anything a host chose to call the same thing, and the average
/// would describe neither.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drift_is_reported_per_provider_not_just_per_model() {
    let addr = start_server().await;
    run_billed_turn(addr, "vendor-a", 4_000).await;
    run_billed_turn(addr, "vendor-b", 40).await;

    let (_, body) = get_json_authed(addr, "/v1/calibration", TOKEN).await;
    let report: serde_json::Value = serde_json::from_str(&body).unwrap();
    let rows = report["models"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "one row per (provider, model): {report}");
    assert_eq!(rows[0]["provider_id"], "vendor-a");
    assert_eq!(rows[1]["provider_id"], "vendor-b");
    assert!(
        rows[0]["drift_ratio"].as_f64().unwrap() > rows[1]["drift_ratio"].as_f64().unwrap(),
        "the same model name under two providers must keep its own gap: {report}"
    );
}

/// Read-only and authenticated, like `/v1/metrics` and for the same reason:
/// token volumes and model ids describe the host's traffic, so an open
/// endpoint would be free reconnaissance.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_drift_report_is_authenticated_and_read_only() {
    let addr = start_server().await;

    let (status, _) = get_json(addr, "/v1/calibration").await;
    assert!(status.contains("401"), "unauthenticated read: {status}");

    let (status, body) = post_json(addr, "/v1/calibration", Some(TOKEN), "{}").await;
    assert!(status.contains("405"), "write attempt: {status} {body}");
}
