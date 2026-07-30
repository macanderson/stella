//! Resumable SSE streams (#971 phase 2).
//!
//! A dropped connection used to lose a turn outright: the frames existed once,
//! were written to a socket, and were forgotten, and `Drop for Session`
//! cancelled the turn behind them. For a host that has already paid for the
//! model call those frames describe, a recycled proxy connection meant paying
//! twice. These tests pin the three properties that fix costs:
//! every frame is numbered, a reconnect gets what it missed, and a host that
//! does not want the trade can switch it off.

mod common;

use std::time::{Duration, Instant};

use common::*;
use serde_json::json;

/// Without a per-frame sequence number, "resume after X" cannot be expressed at
/// all, so this is the foundation the rest of the resume behavior stands on. It
/// must appear both in the JSON payload (for a client that reads `?after=`) and
/// as the SSE `id:` (for one that lets the platform reconnect for it).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_frame_carries_a_monotonic_seq_in_the_payload_and_the_sse_id() {
    let addr = start_server().await;
    let turn_id = create_turn(addr).await;

    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;
    let mut expected = 0_u64;
    let mut seen = 0_usize;
    while let Some((id, event)) = next_event_with_id(&mut sse).await {
        expected += 1;
        seen += 1;
        assert_eq!(
            event["seq"].as_u64(),
            Some(expected),
            "payload seq must be gapless in delivery order: {event}"
        );
        assert_eq!(
            id,
            Some(expected),
            "the SSE id must match the payload seq, or a browser resumes from a \
             different point than an explicit client would"
        );
        // The seq is additive: the frame's own discriminant is untouched.
        assert!(
            event["type"].is_string(),
            "seq must not displace the frame's type tag: {event}"
        );
        if event["type"].as_str() == Some("provider_request") {
            break;
        }
    }
    assert!(seen > 0, "the turn produced frames");
}

/// The reliability property this whole mechanism exists for: a subscriber whose
/// connection drops mid-turn reconnects and receives the frames it missed,
/// rather than losing a turn the host has already paid a model call for.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reconnect_replays_the_frames_it_missed_and_the_turn_survives() {
    // A window long enough that the reconnect below is comfortably inside it.
    let (addr, _capture) = start_observed_server_with(Duration::from_secs(10)).await;
    let turn_id = create_turn(addr).await;
    let events_path = format!("/v1/turns/{turn_id}/events");

    // Read exactly one frame, then hang up — deliberately dropping the rest.
    let mut sse = open_sse(addr, &events_path, TOKEN).await;
    let (first_id, _) = next_event_with_id(&mut sse)
        .await
        .expect("the turn produces a first frame");
    let resume_from = first_id.expect("every frame carries an id");
    drop(sse);

    // Reconnect asking for the WHOLE retained stream (`after=0`), not merely
    // what followed the frame already seen.
    //
    // That is not incidental to the test — it is the contract. A resumed
    // stream carries frames *after* the named point, and an outstanding
    // reverse request is never re-announced: a client saying "after N" is
    // asserting it received everything through N, obligations included. A
    // client that persisted its seq but not its in-flight `tool_request` /
    // `provider_request` ids would therefore wait forever for a frame the
    // server considers delivered. Replaying from 0 is how such a client
    // recovers, and this test drives exactly that path.
    let _ = resume_from;
    let mut sse = open_sse(addr, &format!("{events_path}?after=0"), TOKEN).await;

    let mut answered = false;
    let mut outcome = None;
    let mut first_replayed_seq = None;
    while let Some((_, event)) = next_event_with_id(&mut sse).await {
        let seq = event["seq"]
            .as_u64()
            .expect("frames stay sequenced on resume");
        first_replayed_seq.get_or_insert(seq);
        match event["type"].as_str().unwrap_or_default() {
            "provider_request" if !answered => {
                let request_id = event["request_id"].as_str().unwrap();
                let body = json!({
                    "request_id": request_id,
                    "status": "ok",
                    "result": model_result("done"),
                })
                .to_string();
                let (status, resp) = post_json(
                    addr,
                    &format!("/v1/turns/{turn_id}/provider-result"),
                    Some(TOKEN),
                    &body,
                )
                .await;
                assert!(status.contains("200"), "provider-result: {status} {resp}");
                answered = true;
            }
            "turn_complete" => outcome = Some(event["outcome"].clone()),
            _ => {}
        }
    }

    assert_eq!(
        first_replayed_seq,
        Some(1),
        "a resume from 0 must replay the retained stream from its very first \
         frame, or the client cannot rebuild the turn it lost"
    );
    assert!(
        answered,
        "the turn was still live after the disconnect — a parked turn keeps \
         making progress, which is what makes resuming worth anything"
    );
    let outcome = outcome.expect("the resumed stream carries the terminal frame");
    assert_eq!(
        outcome["status"].as_str(),
        Some("completed"),
        "a turn that survived a reconnect completes normally: {outcome}"
    );
}

/// A browser `EventSource` never sends `?after=`; it replays the last `id:` it
/// saw in a `Last-Event-ID` header, automatically. Honoring that is what makes
/// the web host — the one this server exists for — resume with no client code.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn last_event_id_resumes_the_same_way_the_after_query_does() {
    let (addr, _capture) = start_observed_server_with(Duration::from_secs(10)).await;
    let turn_id = create_turn(addr).await;
    let events_path = format!("/v1/turns/{turn_id}/events");

    let mut sse = open_sse(addr, &events_path, TOKEN).await;
    let (first_id, _) = next_event_with_id(&mut sse)
        .await
        .expect("the turn produces a first frame");
    let resume_from = first_id.expect("every frame carries an id");
    drop(sse);

    let mut sse = open_sse_resuming(addr, &events_path, TOKEN, resume_from).await;
    let (_, event) = next_event_with_id(&mut sse)
        .await
        .expect("the resumed stream delivers the next frame");
    assert_eq!(
        event["seq"].as_u64(),
        Some(resume_from + 1),
        "Last-Event-ID must name the same resume point `?after=` does: {event}"
    );
}

/// The resume window is a trade — a parked turn holds an OS thread and any
/// in-flight model call — so a host must be able to decline it. Zero restores
/// the original contract: a disconnect ends the turn immediately.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_zero_resume_window_cancels_on_disconnect() {
    let (addr, _capture) = start_observed_server_with(Duration::ZERO).await;
    let turn_id = create_turn(addr).await;
    let events_path = format!("/v1/turns/{turn_id}/events");

    let mut sse = open_sse(addr, &events_path, TOKEN).await;
    next_event(&mut sse)
        .await
        .expect("the turn produces a frame");
    drop(sse);

    // Probed with a malformed `tool-result` POST, which reads the registry
    // without taking the session — see the disconnect test above for why a
    // second `/events` GET is no longer a non-destructive probe.
    let probe_path = format!("/v1/turns/{turn_id}/tool-result");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (status, _) = post_json(addr, &probe_path, Some(TOKEN), "not json").await;
        if status.contains("404") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "with the resume window off, a disconnect must reclaim the turn at \
             once: {status}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
