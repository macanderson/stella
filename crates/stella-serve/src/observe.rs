// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Observability for the serve layer: a typed event at every boundary.
//!
//! Before this module the crate's entire observable surface was its startup
//! banner and its exit code — ten print statements, all in `main.rs`, and
//! nothing at all from the library. Four situations produced identical output
//! (none): a turn wedged on a reverse request nobody will answer, a host
//! answering with the wrong `request_id`, a 429 storm, and a bearer-token brute
//! force. See #930 and `docs/design/serve-observability.md`.
//!
//! # Emission and sink are separate decisions
//!
//! The audit dimension this closes contrasts `stella-serve` against
//! `stella-core`, which scores well with **no logging framework at all** — it
//! scores on `AgentEvent`: a typed event at every boundary, with discarded work
//! named rather than dropped. That is what is copied here.
//!
//! - **What is emitted** — [`event::ServeEvent`], serde-first.
//!   Testable, because a test asserts on a typed value instead of scraping text.
//! - **Where it goes** — the [`Observer`] port. This is the part that would cost
//!   a dependency, and keeping it a trait is what makes that decision
//!   reversible: adopting `tracing` later is one more impl in [`sink`], with no
//!   emit site touched.
//!
//! # Nothing here egresses
//!
//! AGENTS.md invariant 3. Records go to stderr; counters are exposed at
//! `GET /v1/metrics` behind the same bearer token as every other route — pull,
//! never push. No sink opens a connection. And no record carries prompt text,
//! tool payloads, model output, paths, raw request paths, or a whole turn id;
//! `tests/observe.rs` proves that with a sentinel sweep in the shape
//! `stella-store::content_free` uses for egress.

pub mod event;
pub mod metrics;
pub(crate) mod record;
pub mod sink;
pub(crate) mod tally;

pub use event::{
    AbandonReason, AcceptKind, Level, LevelFilter, Method, MisrouteFault, RefusalReason, RequestId,
    ReverseKind, Route, ServeEvent, SettledOutcome, ShutdownReason, StreamEndReason, TurnRef,
    TurnTally,
};
pub use metrics::{Metrics, Snapshot};
pub use sink::{Capture, Fanout, JsonlSink, LOG_LEVEL_ENV, NullSink};

/// Where [`ServeEvent`]s go.
///
/// Deliberately one method taking a reference: an observer must be cheap enough
/// to call from the turn's hot path and from every request, must never block the
/// caller on anything it cannot control, and must never fail *upwards* — losing
/// a record is not a reason to lose a turn (invariant 5). Implementations
/// swallow their own I/O errors.
pub trait Observer: Send + Sync + 'static {
    fn emit(&self, event: &ServeEvent);
}

/// The shared handle every emit site holds.
pub type SharedObserver = std::sync::Arc<dyn Observer>;

/// An observer that discards everything — the default for callers that use
/// [`Session`](crate::Session) as a library type without wiring a sink.
#[must_use]
pub fn null_observer() -> SharedObserver {
    std::sync::Arc::new(NullSink)
}

/// The standard production wiring: counters plus a JSONL sink on stderr, as one
/// observer.
///
/// Returns the [`Metrics`] separately because `GET /v1/metrics` has to read it.
/// The counters are *inside* the fanout, not alongside it, which is what makes a
/// counter incapable of disagreeing with the log — see [`metrics`].
#[must_use]
pub fn standard() -> (SharedObserver, std::sync::Arc<Metrics>) {
    let metrics = std::sync::Arc::new(Metrics::new());
    let observer: SharedObserver = std::sync::Arc::new(Fanout::new(vec![
        metrics.clone(),
        std::sync::Arc::new(JsonlSink::from_env()),
    ]));
    (observer, metrics)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// The standard wiring must route to the counters, or `/v1/metrics` reports
    /// zeros forever while the log looks healthy — the exact disagreement this
    /// module's shape exists to prevent.
    #[test]
    fn the_standard_wiring_feeds_the_counters() {
        let (observer, metrics) = standard();
        observer.emit(&ServeEvent::TurnCreated {
            turn: TurnRef::new("turn-0123456789abcdef"),
            live_turns: 1,
        });
        assert_eq!(metrics.snapshot().turns_created_total, 1);
    }

    /// A null observer is a real observer: emit sites must not branch on
    /// whether anyone is listening.
    #[test]
    fn the_null_observer_accepts_everything_silently() {
        let observer = null_observer();
        observer.emit(&ServeEvent::Listening {
            addr: "127.0.0.1:0".to_string(),
            host_guard: crate::HostMode::Loopback,
        });
    }

    /// `Observer` has to be usable as a shared trait object from both the
    /// server runtime and a session thread, so this must compile.
    #[test]
    fn an_observer_is_shareable_across_threads() {
        let capture = Arc::new(Capture::new());
        let observer: SharedObserver = capture.clone();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                observer.emit(&ServeEvent::Listening {
                    addr: "elsewhere".to_string(),
                    host_guard: crate::HostMode::Loopback,
                });
            });
        });
        assert_eq!(capture.events().len(), 1);
    }
}
