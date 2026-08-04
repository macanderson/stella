// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The request fold: one record per connection, produced where the connection
//! ends rather than at the twenty-odd places it can exit.
//!
//! `route` has twenty `return`s and a tail match. Emitting a record
//! before each is the obvious implementation and it is the wrong one — this
//! project has already been burned by that shape and written it down. The PROOF
//! rail (#901, "the proof rail resolves on every path, not just the happy one")
//! was a surface that reported only on the paths someone remembered, and hung
//! on `pending` forever on the ones they did not.
//!
//! So the record lives here, in four moves:
//!
//! 1. **Declare before anything can fail.** [`RequestRecord::opening`] runs
//!    before the first byte is parsed, so a request that dies inside
//!    `read_head` still has an identity and a start time.
//! 2. **Make "unreported" a terminal state.** [`RequestRecord::close`] turns
//!    "no response was written" into `status: None` and an `Err` return into a
//!    named [`ServeEvent::ConnectionFailed`] — the `let _ = handle_conn(..)` in
//!    the accept loop has nothing left to discard.
//! 3. **One construction site.** [`ServeEvent::RequestCompleted`] is built
//!    here and nowhere else in the crate, and `close` consumes `self`.
//! 4. **Prove it with a property.** `exactly_one_record_per_connection_for_any_input` in
//!    `tests/http.rs` drives arbitrary bytes at a live server and asserts
//!    the count, because the original bug is always a path nobody enumerated.

use std::sync::Arc;
use std::time::Instant;

use tokio::net::TcpStream;

use super::Observer;
use super::event::{Method, RequestId, Route, ServeEvent, TurnRef, millis};
use crate::http;

/// What a response actually turned out to be. `None` on a [`Responder`] that
/// was never written to is itself meaningful — see [`RequestRecord::close`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct Wrote {
    pub status: u16,
    pub bytes: u64,
}

/// The write half of one connection, and the only thing that writes to it.
///
/// Every response goes through here, so `status` and `bytes_out` are captured
/// **by construction** rather than by each handler remembering to report them.
/// It also owns the `X-Request-Id` echo, so no route can forget that either.
pub(crate) struct Responder<'a> {
    stream: &'a mut TcpStream,
    request_id: RequestId,
    wrote: Option<Wrote>,
}

impl<'a> Responder<'a> {
    pub(crate) fn new(stream: &'a mut TcpStream, request_id: RequestId) -> Self {
        Self {
            stream,
            request_id,
            wrote: None,
        }
    }

    /// A one-shot JSON response.
    pub(crate) async fn json(&mut self, status: &str, body: &[u8]) -> std::io::Result<()> {
        self.json_with_headers(status, &[], body).await
    }

    /// [`Responder::json`] plus caller-supplied headers (`Retry-After` on a 429,
    /// `Allow` on a 405). `X-Request-Id` is appended here, so every response
    /// carries it whether or not the route thought about it.
    pub(crate) async fn json_with_headers(
        &mut self,
        status: &str,
        extra: &[(&str, &str)],
        body: &[u8],
    ) -> std::io::Result<()> {
        let mut headers: Vec<(&str, &str)> = Vec::with_capacity(extra.len() + 1);
        headers.extend_from_slice(extra);
        headers.push(("X-Request-Id", self.request_id.as_str()));
        let written = http::write_json_with_headers(self.stream, status, &headers, body).await;
        // Record what we *attempted*, even when the write failed: a peer that
        // vanished mid-response still received a decision from this server, and
        // that decision is what a reader of the log needs.
        self.record(status, written.as_ref().copied().unwrap_or(0));
        written.map(|_| ())
    }

    /// Open an SSE stream. The head is a `200`; body bytes accrue through
    /// [`Responder::add_bytes`] as frames are written.
    pub(crate) async fn sse_head(&mut self) -> std::io::Result<()> {
        let written = http::write_sse_head(self.stream, self.request_id.as_str()).await;
        self.record("200 OK", written.as_ref().copied().unwrap_or(0));
        written.map(|_| ())
    }

    /// The raw stream, for the SSE loop's read/write split. Callers that write
    /// through this must report what they wrote with [`Responder::add_bytes`].
    pub(crate) fn stream_mut(&mut self) -> &mut TcpStream {
        self.stream
    }

    /// Add bytes written outside [`Responder::json`] — i.e. SSE frames.
    pub(crate) fn add_bytes(&mut self, bytes: u64) {
        if let Some(wrote) = self.wrote.as_mut() {
            wrote.bytes = wrote.bytes.saturating_add(bytes);
        }
    }

    /// What this connection was answered with, if anything.
    pub(crate) fn wrote(&self) -> Option<Wrote> {
        self.wrote
    }

    /// Parse the numeric status out of a `"404 Not Found"` literal.
    ///
    /// The statuses are all crate-internal literals, so an unparseable one is a
    /// bug in this crate rather than host input; `0` records it as "we answered
    /// with something unrecognisable" instead of dropping the record.
    fn record(&mut self, status: &str, bytes: u64) {
        let code = status
            .split_whitespace()
            .next()
            .and_then(|code| code.parse::<u16>().ok())
            .unwrap_or(0);
        match self.wrote.as_mut() {
            // A second write on one connection only happens on the SSE path
            // (head, then frames); keep the first status and accumulate bytes.
            Some(wrote) => wrote.bytes = wrote.bytes.saturating_add(bytes),
            None => {
                self.wrote = Some(Wrote {
                    status: code,
                    bytes,
                })
            }
        }
    }
}

/// The accumulating identity of one in-flight request.
///
/// Opened before parsing, filled in as routing learns more, and closed exactly
/// once. Nothing here is optional-by-accident: a field that is still `None` at
/// close time means the request died before that fact was known, and the record
/// says so.
pub(crate) struct RequestRecord {
    request_id: RequestId,
    started: Instant,
    method: Option<Method>,
    route: Option<Route>,
    turn: Option<TurnRef>,
    bytes_in: u64,
}

impl RequestRecord {
    /// Open a record. Runs before the request head is read, so a peer that
    /// never completes one still produces a record with an identity, a start
    /// time, and — after `close` — a `status: None`.
    pub(crate) fn opening(request_id: RequestId) -> Self {
        Self {
            request_id,
            started: Instant::now(),
            method: None,
            route: None,
            turn: None,
            bytes_in: 0,
        }
    }

    pub(crate) fn request_id(&self) -> &RequestId {
        &self.request_id
    }

    /// Replace the generated id with the peer's, once a head has parsed and
    /// carried a usable `X-Request-Id`.
    ///
    /// The record opens with a generated id rather than waiting for the header,
    /// because a peer that never completes a head must still be recorded — so
    /// adoption is a second step rather than a precondition.
    pub(crate) fn adopt_request_id(&mut self, request_id: RequestId) {
        self.request_id = request_id;
    }

    pub(crate) fn set_method(&mut self, method: &str) {
        self.method = Some(Method::new(method));
    }

    pub(crate) fn set_route(&mut self, route: Route) {
        self.route = Some(route);
    }

    /// Attach the turn this request concerns, truncated for recording.
    pub(crate) fn set_turn(&mut self, turn_id: &str) {
        self.turn = Some(TurnRef::new(turn_id));
    }

    pub(crate) fn set_bytes_in(&mut self, bytes: u64) {
        self.bytes_in = bytes;
    }

    /// Close the record and emit it. **The only** construction site of
    /// [`ServeEvent::RequestCompleted`] in this crate.
    ///
    /// Consumes `self`, takes the response outcome from the [`Responder`] that
    /// owns the write half, and additionally emits a
    /// [`ServeEvent::ConnectionFailed`] when the connection ended in an I/O
    /// error — the thing the accept loop's `let _ =` used to swallow.
    pub(crate) fn close(
        self,
        observer: &Arc<dyn Observer>,
        wrote: Option<Wrote>,
        outcome: &std::io::Result<()>,
    ) {
        if let Err(err) = outcome {
            observer.emit(&ServeEvent::ConnectionFailed {
                request_id: self.request_id.clone(),
                error: err.to_string(),
            });
        }
        observer.emit(&ServeEvent::RequestCompleted {
            request_id: self.request_id,
            method: self.method.unwrap_or_else(|| Method::new("")),
            route: self.route.unwrap_or(Route::Unrouted),
            status: wrote.map(|wrote| wrote.status),
            duration_ms: millis(self.started.elapsed()),
            bytes_in: self.bytes_in,
            bytes_out: wrote.map_or(0, |wrote| wrote.bytes),
            turn: self.turn,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observe::sink::Capture;

    fn observer(capture: &Arc<Capture>) -> Arc<dyn Observer> {
        capture.clone()
    }

    /// A request that died before we answered must still produce a record —
    /// with `status: None`, which is a *terminal state*, not a missing field.
    /// This is the case the accept loop's `let _ =` used to erase entirely.
    #[test]
    fn an_unanswered_connection_still_reports() {
        let capture = Arc::new(Capture::new());
        let record = RequestRecord::opening(RequestId::generate());
        record.close(&observer(&capture), None, &Ok(()));

        let events = capture.events();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ServeEvent::RequestCompleted { status, route, .. } => {
                assert_eq!(*status, None, "silence must be recorded as silence");
                assert_eq!(*route, Route::Unrouted);
            }
            other => panic!("expected a request record, got {other:?}"),
        }
    }

    /// An I/O failure produces *both* the named failure and the terminal
    /// record — the failure is no longer discarded, and the record still
    /// accounts for the connection.
    #[test]
    fn a_failed_connection_is_named_and_still_accounted() {
        let capture = Arc::new(Capture::new());
        let mut record = RequestRecord::opening(RequestId::generate());
        record.set_method("POST");
        record.set_route(Route::TurnsCreate);
        let err = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "peer reset");
        record.close(
            &observer(&capture),
            Some(Wrote {
                status: 200,
                bytes: 12,
            }),
            &Err(err),
        );

        let events = capture.events();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], ServeEvent::ConnectionFailed { .. }));
        match &events[1] {
            ServeEvent::RequestCompleted {
                status,
                bytes_out,
                route,
                ..
            } => {
                assert_eq!(*status, Some(200));
                assert_eq!(*bytes_out, 12);
                assert_eq!(*route, Route::TurnsCreate);
            }
            other => panic!("expected a request record, got {other:?}"),
        }
    }

    /// The turn a request concerns is correlatable but not actionable.
    #[test]
    fn a_record_carries_a_truncated_turn_not_the_id() {
        let capture = Arc::new(Capture::new());
        let full = "turn-0123456789abcdef0123456789abcdef";
        let mut record = RequestRecord::opening(RequestId::generate());
        record.set_route(Route::TurnCancel);
        record.set_turn(full);
        record.close(&observer(&capture), None, &Ok(()));

        let json = serde_json::to_string(&capture.events()[0]).expect("serialize");
        assert!(!json.contains(full), "the whole turn id reached a record");
        assert!(json.contains("01234567"));
        assert!(
            json.contains("/v1/turns/{id}/cancel"),
            "the route must be a template: {json}"
        );
    }

    /// The status literals used across the crate must all parse, or the
    /// recorded status silently becomes 0 for a whole class of responses.
    #[test]
    fn every_status_literal_this_crate_writes_parses() {
        for status in [
            "200 OK",
            "400 Bad Request",
            "401 Unauthorized",
            "404 Not Found",
            "405 Method Not Allowed",
            "408 Request Timeout",
            "409 Conflict",
            "413 Payload Too Large",
            "429 Too Many Requests",
            "501 Not Implemented",
        ] {
            let code = status
                .split_whitespace()
                .next()
                .and_then(|code| code.parse::<u16>().ok());
            assert!(code.is_some(), "`{status}` does not parse");
            assert!(matches!(code, Some(200..=599)));
        }
    }
}
