// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The live plane — Server-Sent Events over the workspace's own telemetry.
//!
//! # Why a stream at all
//!
//! The dashboard used to fire twelve full-history aggregates every five
//! seconds, unconditionally, whether or not anything had happened. That is
//! expensive when the workspace is busy and pure waste when it is idle, and
//! it still leaves the operator up to five seconds behind — which is the
//! wrong order of magnitude for the thing this dashboard exists to catch. An
//! agent that has started looping, or begun burning tokens on a tool that
//! keeps erroring, should be visible as it happens, not on the next tick.
//!
//! # Why the server polls even though this is a push stream
//!
//! There is no cross-process notification to subscribe to. The writing
//! session and the observatory are different processes sharing a SQLite file,
//! and SQLite offers no "tell me when someone else commits" channel across
//! process boundaries. So something must ask.
//!
//! The move is to make *asking* cheap and *answering* rare.
//! [`Observatory::cursor`] is five small probes — no join, and only one scan
//! (`count(*) FROM tool_calls`) —
//! so the server can run it several times a second for little. It pushes a
//! frame only when that fingerprint actually moves. Net effect: the browser
//! stops polling entirely, total query work drops (idle workspaces cost a
//! handful of probes instead of twelve aggregates), and latency improves from
//! five seconds to [`POLL_INTERVAL`].
//!
//! # Why this needs its own connection budget
//!
//! `lib.rs` bounds concurrent connections with a semaphore, and the comment
//! there is an explicit warning: it is only safe because "nothing here is a
//! long-lived stream — every response is one shot and closes". An SSE
//! connection is exactly the long-lived stream that warning is about. Left on
//! the same semaphore, a handful of open dashboard tabs would hold permits
//! indefinitely and starve every one-shot route behind them, and the page
//! would hang while its own stream was healthy.
//!
//! So streams get a separate, smaller budget ([`MAX_LIVE_STREAMS`]) and the
//! one-shot semaphore is left exactly as it was. A stream that cannot get a
//! slot is **refused** rather than queued — the opposite of the one-shot
//! policy, and deliberately: a queued one-shot request arrives a moment late,
//! but a queued stream would hang forever behind connections that never
//! close, and a client told "no" can fall back to polling immediately.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

use crate::db::Observatory;

/// How often the server re-reads the cursor.
///
/// The floor on how stale the dashboard can be, and the knob that trades
/// latency against idle cost. A quarter second reads as instant to a human
/// watching a tool call land, while costing a few index probes per second
/// against a local file — far less than the twelve full-history aggregates
/// per five seconds it replaces.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How often a frame is sent even when nothing changed.
///
/// Required, not cosmetic: an idle SSE connection is indistinguishable from a
/// wedged one, and intermediaries and browsers alike will eventually drop a
/// silent socket. The heartbeat is also how the server discovers the client
/// is gone — the write fails, and the task ends.
const HEARTBEAT: Duration = Duration::from_secs(15);

/// Concurrent live streams served at once.
///
/// Small on purpose. Each is a task holding a socket and waking four times a
/// second, and the legitimate population is "dashboard tabs a person has
/// open" — single digits. See this module's header for why this budget is
/// separate from the one-shot connection semaphore rather than shared with
/// it.
const MAX_LIVE_STREAMS: usize = 8;

/// Live streams currently open, against [`MAX_LIVE_STREAMS`].
///
/// A counter rather than a `Semaphore` because the policy here is refusal,
/// not queueing: the check and the answer are one atomic step, and there is
/// no waiting to represent.
static OPEN_STREAMS: AtomicUsize = AtomicUsize::new(0);

/// Decrements [`OPEN_STREAMS`] on drop, so a slot is returned however the
/// stream ended — client hangup, write error, or the task being dropped
/// mid-await. Releasing by hand at the end of the loop would leak a slot on
/// every path that leaves early, and a leaked slot is permanent: the budget
/// is process-lifetime.
struct StreamSlot;

impl StreamSlot {
    /// Claim a slot, or `None` when the budget is full.
    fn claim() -> Option<Self> {
        // `fetch_update` rather than load-then-store: two tabs opening at
        // once must not both observe MAX - 1 and both proceed.
        OPEN_STREAMS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |open| {
                (open < MAX_LIVE_STREAMS).then_some(open + 1)
            })
            .ok()
            .map(|_| Self)
    }
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        OPEN_STREAMS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The response head for a live stream.
///
/// `no-store` and `X-Accel-Buffering: no` both say the same thing to
/// different listeners: do not buffer this. A proxy or browser that holds
/// frames back to fill a buffer turns a real-time stream into a delayed one,
/// which is the single failure mode this whole module exists to avoid.
/// `Connection: keep-alive` overrides the one-shot routes' `close`.
fn stream_head(csp: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
         Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\
         Content-Security-Policy: {csp}\r\nX-Accel-Buffering: no\r\n\
         Connection: keep-alive\r\n\r\n"
    )
}

/// One SSE frame: `event:` name, `data:` payload, blank line to dispatch.
///
/// The payload is serialized compactly and on one line because a literal
/// newline inside `data:` would split the frame into two fields — SSE is a
/// line-oriented format, and `serde_json`'s compact writer is what keeps that
/// safe without escaping by hand.
fn frame(event: &str, data: &Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

/// Serve one live stream until the client goes away.
///
/// Sends a `tick` frame immediately (so a fresh tab renders without waiting
/// for the first change), then one per cursor movement, plus a heartbeat
/// whenever [`HEARTBEAT`] passes with nothing to say.
///
/// Errors are the exit condition, not something to report: every one of them
/// means the socket is gone, and there is nobody left to tell.
pub(crate) async fn serve_stream(
    mut socket: TcpStream,
    observatory: Arc<Observatory>,
    csp: &str,
) -> std::io::Result<()> {
    let Some(_slot) = StreamSlot::claim() else {
        // Refused, with a body the page can act on — it falls back to
        // polling rather than showing an empty dashboard.
        let body = json!({ "error": "too many live streams open" }).to_string();
        socket
            .write_all(
                format!(
                    "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await?;
        return socket.shutdown().await;
    };
    socket.write_all(stream_head(csp).as_bytes()).await?;

    let mut last_cursor: Option<Value> = None;
    let mut since_frame = Duration::ZERO;
    loop {
        // The queries are blocking SQLite work. Off the reactor, or four
        // wakeups a second per stream would sit on reactor workers that the
        // one-shot routes also need.
        let obs = Arc::clone(&observatory);
        let Ok(snapshot) = tokio::task::spawn_blocking(move || {
            let cursor = obs.cursor().ok()?;
            Some((cursor, obs.live().ok()?))
        })
        .await
        else {
            return Ok(());
        };
        let Some((cursor, live)) = snapshot else {
            // A transient read failure (a migration's exclusive lock) is not
            // a reason to drop a healthy connection — skip this tick and let
            // the heartbeat keep the socket alive until it clears.
            tokio::time::sleep(POLL_INTERVAL).await;
            since_frame += POLL_INTERVAL;
            continue;
        };
        let changed = last_cursor.as_ref() != Some(&cursor);
        if changed || since_frame >= HEARTBEAT {
            let payload = json!({ "cursor": cursor, "live": live, "changed": changed });
            socket.write_all(frame("tick", &payload).as_bytes()).await?;
            since_frame = Duration::ZERO;
            last_cursor = Some(cursor);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
        since_frame += POLL_INTERVAL;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_is_one_line_of_data_terminated_by_a_blank_line() {
        let f = frame("tick", &json!({ "a": 1, "b": "x" }));
        assert!(f.starts_with("event: tick\n"));
        assert!(f.ends_with("\n\n"), "a frame must dispatch: {f:?}");
        let data: Vec<&str> = f.lines().filter(|l| l.starts_with("data: ")).collect();
        assert_eq!(data.len(), 1, "the payload must not split across fields");
    }

    /// A payload holding a newline must still produce exactly one `data:`
    /// line — otherwise SSE would read the tail as a separate field and the
    /// client would parse a truncated frame.
    #[test]
    fn an_embedded_newline_cannot_split_a_frame() {
        let f = frame("tick", &json!({ "prompt": "line one\nline two" }));
        let data: Vec<&str> = f.lines().filter(|l| l.starts_with("data: ")).collect();
        assert_eq!(data.len(), 1);
        assert!(f.contains("\\n"), "the newline is escaped, not literal");
    }

    #[test]
    fn the_stream_budget_refuses_past_its_cap_and_recovers_on_drop() {
        let slots: Vec<StreamSlot> = (0..MAX_LIVE_STREAMS)
            .map(|_| StreamSlot::claim().expect("within budget"))
            .collect();
        assert!(
            StreamSlot::claim().is_none(),
            "the cap refuses rather than over-committing"
        );
        drop(slots);
        assert!(
            StreamSlot::claim().is_some(),
            "slots return when streams end"
        );
    }

    #[test]
    fn the_stream_head_forbids_buffering_and_keeps_the_connection() {
        let head = stream_head("default-src 'self'");
        assert!(head.contains("Content-Type: text/event-stream"));
        assert!(head.contains("X-Accel-Buffering: no"));
        assert!(head.contains("Connection: keep-alive"));
        assert!(!head.contains("Content-Length"), "a stream has no length");
    }
}
