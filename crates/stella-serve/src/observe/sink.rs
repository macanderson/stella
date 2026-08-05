// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Where [`ServeEvent`]s go: the sinks behind the [`Observer`] port.
//!
//! Three of them, none needing a dependency the crate did not already have:
//! [`JsonlSink`] (one JSON object per line on stderr), [`Fanout`] (several
//! observers as one), and [`Capture`] (test-only, the thing that makes the
//! acceptance criteria assertable on typed values instead of scraped text).
//!
//! The port is the whole point. Emission is typed and every site talks to
//! [`Observer`], so adopting `tracing` later is one more impl in this file and
//! **no emit site changes** — see `docs/spec/serve-observability.md` §9 for
//! why that deferral is a decision with an expiry rather than an omission.

use std::io::Write as _;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::Observer;
use super::event::{Level, LevelFilter, ServeEvent};

/// The environment variable that sets the sink's verbosity.
pub const LOG_LEVEL_ENV: &str = "STELLA_SERVE_LOG";

/// One line of the log, as it is actually written.
///
/// `ts` is epoch milliseconds — the workspace's existing convention
/// (`stella-store::sessions`), so this needs no time crate. `flatten` merges
/// the event's own internally-tagged map, which is why a record reads as one
/// flat object rather than a nested `{"event":{...}}`.
#[derive(Serialize)]
struct Line<'a> {
    ts: u128,
    level: Level,
    #[serde(flatten)]
    event: &'a ServeEvent,
}

/// One JSON object per line, on stderr.
///
/// stderr, not a file, on purpose: rotation, retention and shipping belong to
/// the supervisor (systemd, Docker, Oxagen's runner), and reimplementing them
/// inside the process is the first thing a real logging framework does better.
pub struct JsonlSink {
    filter: LevelFilter,
    /// Serializes whole lines. Two connections writing concurrently would
    /// otherwise interleave mid-record and produce unparseable output.
    out: Mutex<std::io::Stderr>,
}

impl JsonlSink {
    #[must_use]
    pub fn new(filter: LevelFilter) -> Self {
        Self {
            filter,
            out: Mutex::new(std::io::stderr()),
        }
    }

    /// Build from `STELLA_SERVE_LOG`, defaulting to `info`.
    #[must_use]
    pub fn from_env() -> Self {
        let filter = std::env::var(LOG_LEVEL_ENV)
            .map_or(LevelFilter::default(), |value| LevelFilter::parse(&value));
        Self::new(filter)
    }
}

impl Observer for JsonlSink {
    fn emit(&self, event: &ServeEvent) {
        let level = event.level();
        if !self.filter.admits(level) {
            return;
        }
        let line = Line {
            ts: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_millis()),
            level,
            event,
        };
        let Ok(mut json) = serde_json::to_vec(&line) else {
            // Unreachable: every field is serde-clean. Silence beats a panic
            // here — invariant 5, and losing a log line must never lose a turn.
            return;
        };
        json.push(b'\n');
        // A failed write is dropped deliberately: stderr being closed or full
        // is not a reason to fail a request that otherwise succeeded.
        if let Ok(mut out) = self.out.lock() {
            let _ = out.write_all(&json);
        }
    }
}

/// Several observers as one. The server holds exactly one of these, so adding a
/// sink never touches an emit site.
pub struct Fanout {
    observers: Vec<std::sync::Arc<dyn Observer>>,
}

impl Fanout {
    #[must_use]
    pub fn new(observers: Vec<std::sync::Arc<dyn Observer>>) -> Self {
        Self { observers }
    }
}

impl Observer for Fanout {
    fn emit(&self, event: &ServeEvent) {
        for observer in &self.observers {
            observer.emit(event);
        }
    }
}

/// Discards everything. The default when a caller does not care, and what keeps
/// `Session` usable as a library type without wiring a sink.
pub struct NullSink;

impl Observer for NullSink {
    fn emit(&self, _event: &ServeEvent) {}
}

/// Records events in memory for assertions.
///
/// This is what makes #930's acceptance criteria testable: a test matches on
/// `ServeEvent::ReverseTimedOut { .. }` rather than grepping stderr for a
/// phrase, so the assertion survives any change to how records are rendered.
#[derive(Default)]
pub struct Capture {
    events: Mutex<Vec<ServeEvent>>,
}

impl Capture {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every event emitted so far, in order.
    #[must_use]
    pub fn events(&self) -> Vec<ServeEvent> {
        self.events
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// How many events match `predicate`.
    pub fn count(&self, predicate: impl Fn(&ServeEvent) -> bool) -> usize {
        self.events().iter().filter(|e| predicate(e)).count()
    }

    /// The first event matching `predicate`, if any.
    pub fn find(&self, predicate: impl Fn(&ServeEvent) -> bool) -> Option<ServeEvent> {
        self.events().into_iter().find(|e| predicate(e))
    }
}

impl Observer for Capture {
    fn emit(&self, event: &ServeEvent) {
        self.events
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(event.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observe::event::{Method, RequestId, Route, ServeEvent, TurnRef};
    use std::sync::Arc;

    fn a_request() -> ServeEvent {
        ServeEvent::RequestCompleted {
            request_id: RequestId::generate(),
            method: Method::new("GET"),
            route: Route::Healthz,
            status: Some(200),
            duration_ms: 1,
            bytes_in: 0,
            bytes_out: 15,
            turn: None,
        }
    }

    /// The rendered line must be one object per line, flat, with the event's
    /// own fields alongside `ts`/`level` rather than nested under a key.
    #[test]
    fn a_record_renders_as_one_flat_json_line() {
        let event = a_request();
        let line = Line {
            ts: 1_753_876_543_210,
            level: event.level(),
            event: &event,
        };
        let json = serde_json::to_string(&line).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(value["ts"], 1_753_876_543_210_u64);
        assert_eq!(value["level"], "info");
        assert_eq!(value["event"], "request_completed");
        assert_eq!(value["route"], "/healthz");
        assert_eq!(value["status"], 200);
        assert!(!json.contains('\n'), "a record must not span lines");
    }

    /// Filtering is the sink's job, not the emit site's — so the counters
    /// downstream of the same stream stay correct at any verbosity.
    #[test]
    fn the_filter_drops_records_without_suppressing_emission() {
        let capture = Arc::new(Capture::new());
        let fanout = Fanout::new(vec![capture.clone()]);
        fanout.emit(&a_request());
        fanout.emit(&ServeEvent::ReverseMisrouted {
            request_id: RequestId::generate(),
            fault: crate::observe::event::MisrouteFault::UnknownRequest,
        });
        assert_eq!(capture.events().len(), 2);

        // The sink at `error` would print neither of those, but `Capture` — the
        // stand-in for the metrics fold — still saw both.
        assert!(!LevelFilter::Error.admits(Level::Info));
        assert!(!LevelFilter::Error.admits(Level::Warn));
    }

    /// A fanout with several sinks delivers to all of them, in order.
    #[test]
    fn a_fanout_reaches_every_observer() {
        let first = Arc::new(Capture::new());
        let second = Arc::new(Capture::new());
        let fanout = Fanout::new(vec![first.clone(), second.clone()]);
        fanout.emit(&a_request());
        fanout.emit(&a_request());
        assert_eq!(first.events().len(), 2);
        assert_eq!(second.events().len(), 2);
    }

    /// `TurnRef` is what reaches a record, so a full turn id must not appear in
    /// rendered output even when the emitting code holds one.
    #[test]
    fn a_rendered_record_never_carries_a_whole_turn_id() {
        let full = "turn-0123456789abcdef0123456789abcdef";
        let event = ServeEvent::TurnCreated {
            turn: TurnRef::new(full),
            live_turns: 1,
        };
        let line = Line {
            ts: 0,
            level: event.level(),
            event: &event,
        };
        let json = serde_json::to_string(&line).expect("serialize");
        assert!(
            !json.contains(full),
            "the whole turn id reached the record: {json}"
        );
        assert!(json.contains("01234567"), "no correlation handle: {json}");
    }
}
