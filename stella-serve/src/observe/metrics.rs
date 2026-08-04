// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Counters, folded from the same event stream the log sees.
//!
//! [`Metrics`] is an [`Observer`], not a set of instrumentation points. It has
//! no call sites of its own: it observes [`ServeEvent`]s and derives every
//! number from them. That is not a stylistic choice — it makes the usual
//! hand-rolled-metrics failure *unrepresentable*. A code path that increments a
//! counter but forgets to log, or logs but forgets to increment, cannot exist
//! when the counter is strictly downstream of the single emission.
//!
//! Exposed at `GET /v1/metrics` behind the same bearer token as everything else
//! (#930 is explicit: authenticated, not open). Pull, never push — nothing here
//! dials out, per AGENTS.md invariant 3.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use serde::Serialize;

use super::Observer;
use super::event::{MisrouteFault, RefusalReason, ReverseKind, ServeEvent, SettledOutcome};

/// Relaxed throughout: these are independent counters read for reporting, never
/// used to order other memory, so the cost of stronger ordering buys nothing.
const ORDER: Ordering = Ordering::Relaxed;

/// Every counter this server keeps.
#[derive(Default)]
pub struct Metrics {
    requests_total: AtomicU64,
    requests_2xx: AtomicU64,
    requests_4xx: AtomicU64,
    requests_5xx: AtomicU64,
    /// Requests the peer abandoned before we answered — the `status: None`
    /// terminal state, which without the fold would be no record at all.
    requests_unanswered: AtomicU64,
    request_duration_ms_total: AtomicU64,
    bytes_in_total: AtomicU64,
    bytes_out_total: AtomicU64,

    unauthorized_total: AtomicU64,
    /// 401s that were actually delayed by the throttle. A non-zero value here
    /// is the signature of a sustained guess, not a misconfigured client.
    unauthorized_throttled_total: AtomicU64,
    /// Requests refused by the `Host` guard (#1130). A non-zero value on a
    /// freshly-deployed container almost always means a missing
    /// `STELLA_SERVE_ALLOWED_HOSTS` entry; a non-zero value on a loopback
    /// bind means something is addressing this server by a name it does not
    /// answer to, which is the rebinding signature.
    host_rejected_total: AtomicU64,
    /// Turns a shutdown drain had to cancel because they did not reach a step
    /// boundary inside the grace period (#1131).
    drain_cancelled_total: AtomicU64,

    turns_created_total: AtomicU64,
    turns_refused_capacity_total: AtomicU64,
    turns_refused_collision_total: AtomicU64,
    turns_completed_total: AtomicU64,
    turns_aborted_total: AtomicU64,
    turns_reclaimed_total: AtomicU64,
    turns_live: AtomicI64,

    sessions_created_total: AtomicU64,
    sessions_refused_total: AtomicU64,
    sessions_deleted_total: AtomicU64,
    sessions_reclaimed_total: AtomicU64,
    sessions_live: AtomicI64,

    turn_duration_ms_total: AtomicU64,
    model_calls_total: AtomicU64,
    model_retries_total: AtomicU64,
    tool_calls_total: AtomicU64,
    tools_failed_total: AtomicU64,
    speculation_discarded_total: AtomicU64,
    loop_detections_total: AtomicU64,

    reverse_dispatched_provider_total: AtomicU64,
    reverse_dispatched_tool_total: AtomicU64,
    /// A pipeline-driven turn's scope-review gate, dispatched over the wire
    /// exactly like a tool or provider request (#1288).
    reverse_dispatched_scope_review_total: AtomicU64,
    reverse_answered_total: AtomicU64,
    reverse_wait_ms_total: AtomicU64,
    reverse_timed_out_total: AtomicU64,
    reverse_misrouted_unknown_total: AtomicU64,
    reverse_misrouted_kind_total: AtomicU64,
    reverse_abandoned_total: AtomicU64,
    reverse_in_flight: AtomicI64,

    streams_ended_total: AtomicU64,
    frames_sent_total: AtomicU64,
    frames_unencodable_total: AtomicU64,
    connections_failed_total: AtomicU64,
    accept_backoff_total: AtomicU64,
    accept_gave_up_total: AtomicU64,
    checkpoints_failed_total: AtomicU64,
}

/// The serializable shape of `GET /v1/metrics`.
///
/// A flat object of integers on purpose: it is a scrape target, not an API, and
/// a flat shape is what both `jq` and a Prometheus text translator want. Nothing
/// here is host content — every value is a count this process derived.
#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub requests_total: u64,
    pub requests_2xx: u64,
    pub requests_4xx: u64,
    pub requests_5xx: u64,
    pub requests_unanswered: u64,
    pub request_duration_ms_total: u64,
    pub bytes_in_total: u64,
    pub bytes_out_total: u64,

    pub unauthorized_total: u64,
    pub unauthorized_throttled_total: u64,
    pub host_rejected_total: u64,
    pub drain_cancelled_total: u64,

    pub turns_created_total: u64,
    pub turns_refused_capacity_total: u64,
    pub turns_refused_collision_total: u64,
    pub turns_completed_total: u64,
    pub turns_aborted_total: u64,
    pub turns_reclaimed_total: u64,
    pub turns_live: u64,

    pub sessions_created_total: u64,
    pub sessions_refused_total: u64,
    pub sessions_deleted_total: u64,
    pub sessions_reclaimed_total: u64,
    pub sessions_live: u64,

    pub turn_duration_ms_total: u64,
    pub model_calls_total: u64,
    pub model_retries_total: u64,
    pub tool_calls_total: u64,
    pub tools_failed_total: u64,
    pub speculation_discarded_total: u64,
    pub loop_detections_total: u64,

    pub reverse_dispatched_provider_total: u64,
    pub reverse_dispatched_tool_total: u64,
    pub reverse_dispatched_scope_review_total: u64,
    pub reverse_answered_total: u64,
    pub reverse_wait_ms_total: u64,
    pub reverse_timed_out_total: u64,
    pub reverse_misrouted_unknown_total: u64,
    pub reverse_misrouted_kind_total: u64,
    pub reverse_abandoned_total: u64,
    pub reverse_in_flight: u64,

    pub streams_ended_total: u64,
    pub frames_sent_total: u64,
    pub frames_unencodable_total: u64,
    pub connections_failed_total: u64,
    pub accept_backoff_total: u64,
    pub accept_gave_up_total: u64,
    /// Turns whose resume point could not be written or cleared. One per
    /// *turn*, matching `ServeEvent::CheckpointFailed`'s own budget — so a
    /// non-zero value counts undurable turns, not failed syscalls.
    pub checkpoints_failed_total: u64,
}

impl Metrics {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read every counter. Not a consistent snapshot across counters — each is
    /// read independently — which is the right trade for a scrape endpoint:
    /// locking the whole server to make thirty integers agree would cost more
    /// than the momentary skew is worth.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            requests_total: self.requests_total.load(ORDER),
            requests_2xx: self.requests_2xx.load(ORDER),
            requests_4xx: self.requests_4xx.load(ORDER),
            requests_5xx: self.requests_5xx.load(ORDER),
            requests_unanswered: self.requests_unanswered.load(ORDER),
            request_duration_ms_total: self.request_duration_ms_total.load(ORDER),
            bytes_in_total: self.bytes_in_total.load(ORDER),
            bytes_out_total: self.bytes_out_total.load(ORDER),

            unauthorized_total: self.unauthorized_total.load(ORDER),
            unauthorized_throttled_total: self.unauthorized_throttled_total.load(ORDER),
            host_rejected_total: self.host_rejected_total.load(ORDER),
            drain_cancelled_total: self.drain_cancelled_total.load(ORDER),

            turns_created_total: self.turns_created_total.load(ORDER),
            turns_refused_capacity_total: self.turns_refused_capacity_total.load(ORDER),
            turns_refused_collision_total: self.turns_refused_collision_total.load(ORDER),
            turns_completed_total: self.turns_completed_total.load(ORDER),
            turns_aborted_total: self.turns_aborted_total.load(ORDER),
            turns_reclaimed_total: self.turns_reclaimed_total.load(ORDER),
            turns_live: gauge(&self.turns_live),

            sessions_created_total: self.sessions_created_total.load(ORDER),
            sessions_refused_total: self.sessions_refused_total.load(ORDER),
            sessions_deleted_total: self.sessions_deleted_total.load(ORDER),
            sessions_reclaimed_total: self.sessions_reclaimed_total.load(ORDER),
            sessions_live: gauge(&self.sessions_live),

            turn_duration_ms_total: self.turn_duration_ms_total.load(ORDER),
            model_calls_total: self.model_calls_total.load(ORDER),
            model_retries_total: self.model_retries_total.load(ORDER),
            tool_calls_total: self.tool_calls_total.load(ORDER),
            tools_failed_total: self.tools_failed_total.load(ORDER),
            speculation_discarded_total: self.speculation_discarded_total.load(ORDER),
            loop_detections_total: self.loop_detections_total.load(ORDER),

            reverse_dispatched_provider_total: self.reverse_dispatched_provider_total.load(ORDER),
            reverse_dispatched_tool_total: self.reverse_dispatched_tool_total.load(ORDER),
            reverse_dispatched_scope_review_total: self
                .reverse_dispatched_scope_review_total
                .load(ORDER),
            reverse_answered_total: self.reverse_answered_total.load(ORDER),
            reverse_wait_ms_total: self.reverse_wait_ms_total.load(ORDER),
            reverse_timed_out_total: self.reverse_timed_out_total.load(ORDER),
            reverse_misrouted_unknown_total: self.reverse_misrouted_unknown_total.load(ORDER),
            reverse_misrouted_kind_total: self.reverse_misrouted_kind_total.load(ORDER),
            reverse_abandoned_total: self.reverse_abandoned_total.load(ORDER),
            reverse_in_flight: gauge(&self.reverse_in_flight),

            streams_ended_total: self.streams_ended_total.load(ORDER),
            frames_sent_total: self.frames_sent_total.load(ORDER),
            frames_unencodable_total: self.frames_unencodable_total.load(ORDER),
            connections_failed_total: self.connections_failed_total.load(ORDER),
            accept_backoff_total: self.accept_backoff_total.load(ORDER),
            accept_gave_up_total: self.accept_gave_up_total.load(ORDER),
            checkpoints_failed_total: self.checkpoints_failed_total.load(ORDER),
        }
    }
}

/// Read a gauge, clamping a transiently-negative value to zero.
///
/// The in-flight gauges are incremented and decremented from different threads,
/// so a reader can catch a decrement that landed before its matching increment
/// was observed. Reporting `0` beats reporting a negative count of things.
fn gauge(value: &AtomicI64) -> u64 {
    u64::try_from(value.load(ORDER).max(0)).unwrap_or(0)
}

impl Observer for Metrics {
    fn emit(&self, event: &ServeEvent) {
        match event {
            ServeEvent::RequestCompleted {
                status,
                duration_ms,
                bytes_in,
                bytes_out,
                ..
            } => {
                self.requests_total.fetch_add(1, ORDER);
                self.request_duration_ms_total
                    .fetch_add(*duration_ms, ORDER);
                self.bytes_in_total.fetch_add(*bytes_in, ORDER);
                self.bytes_out_total.fetch_add(*bytes_out, ORDER);
                match status {
                    Some(200..=299) => &self.requests_2xx,
                    Some(400..=499) => &self.requests_4xx,
                    Some(500..=599) => &self.requests_5xx,
                    // A hung-up peer, or a status outside the classes above.
                    _ => &self.requests_unanswered,
                }
                .fetch_add(1, ORDER);
            }
            ServeEvent::Unauthorized { held_ms, .. } => {
                self.unauthorized_total.fetch_add(1, ORDER);
                if *held_ms > 0 {
                    self.unauthorized_throttled_total.fetch_add(1, ORDER);
                }
            }

            ServeEvent::HostRejected { .. } => {
                self.host_rejected_total.fetch_add(1, ORDER);
            }

            ServeEvent::TurnCreated { live_turns, .. } => {
                self.turns_created_total.fetch_add(1, ORDER);
                self.turns_live.store(as_i64(*live_turns), ORDER);
            }
            ServeEvent::TurnRefused { reason, live_turns } => {
                match reason {
                    RefusalReason::AtCapacity => &self.turns_refused_capacity_total,
                    RefusalReason::IdCollision => &self.turns_refused_collision_total,
                }
                .fetch_add(1, ORDER);
                self.turns_live.store(as_i64(*live_turns), ORDER);
            }
            ServeEvent::TurnSettled {
                outcome,
                duration_ms,
                tally,
                ..
            } => {
                match outcome {
                    SettledOutcome::Completed => &self.turns_completed_total,
                    SettledOutcome::Aborted => &self.turns_aborted_total,
                }
                .fetch_add(1, ORDER);
                self.turn_duration_ms_total.fetch_add(*duration_ms, ORDER);
                self.model_calls_total
                    .fetch_add(u64::from(tally.model_calls), ORDER);
                self.model_retries_total
                    .fetch_add(u64::from(tally.retries), ORDER);
                self.tool_calls_total
                    .fetch_add(u64::from(tally.tool_calls), ORDER);
                self.tools_failed_total
                    .fetch_add(u64::from(tally.tools_failed), ORDER);
                self.speculation_discarded_total
                    .fetch_add(u64::from(tally.speculation_discarded), ORDER);
                self.loop_detections_total
                    .fetch_add(u64::from(tally.loop_detections), ORDER);
            }
            ServeEvent::TurnReclaimed { .. } => {
                self.turns_reclaimed_total.fetch_add(1, ORDER);
            }

            ServeEvent::SessionCreated { live_sessions, .. } => {
                self.sessions_created_total.fetch_add(1, ORDER);
                self.sessions_live.store(as_i64(*live_sessions), ORDER);
            }
            ServeEvent::SessionRefused { live_sessions } => {
                self.sessions_refused_total.fetch_add(1, ORDER);
                self.sessions_live.store(as_i64(*live_sessions), ORDER);
            }
            ServeEvent::SessionDeleted { live_sessions, .. } => {
                self.sessions_deleted_total.fetch_add(1, ORDER);
                self.sessions_live.store(as_i64(*live_sessions), ORDER);
            }
            ServeEvent::SessionReclaimed { .. } => {
                self.sessions_reclaimed_total.fetch_add(1, ORDER);
            }

            ServeEvent::ReverseDispatched { kind, .. } => {
                match kind {
                    ReverseKind::Provider => &self.reverse_dispatched_provider_total,
                    ReverseKind::Tool => &self.reverse_dispatched_tool_total,
                    ReverseKind::ScopeReview => &self.reverse_dispatched_scope_review_total,
                }
                .fetch_add(1, ORDER);
                self.reverse_in_flight.fetch_add(1, ORDER);
            }
            ServeEvent::ReverseAnswered { waited_ms, .. } => {
                self.reverse_answered_total.fetch_add(1, ORDER);
                self.reverse_wait_ms_total.fetch_add(*waited_ms, ORDER);
                self.reverse_in_flight.fetch_sub(1, ORDER);
            }
            ServeEvent::ReverseTimedOut { waited_ms, .. } => {
                self.reverse_timed_out_total.fetch_add(1, ORDER);
                self.reverse_wait_ms_total.fetch_add(*waited_ms, ORDER);
                self.reverse_in_flight.fetch_sub(1, ORDER);
            }
            ServeEvent::ReverseMisrouted { fault, .. } => {
                match fault {
                    MisrouteFault::UnknownRequest => &self.reverse_misrouted_unknown_total,
                    MisrouteFault::KindMismatch => &self.reverse_misrouted_kind_total,
                }
                .fetch_add(1, ORDER);
            }
            ServeEvent::ReverseAbandoned { in_flight, .. } => {
                self.reverse_abandoned_total
                    .fetch_add(as_u64(*in_flight), ORDER);
                self.reverse_in_flight.fetch_sub(as_i64(*in_flight), ORDER);
            }

            ServeEvent::StreamEnded { frames_sent, .. } => {
                self.streams_ended_total.fetch_add(1, ORDER);
                self.frames_sent_total.fetch_add(*frames_sent, ORDER);
            }
            ServeEvent::FrameUnencodable { .. } => {
                self.frames_unencodable_total.fetch_add(1, ORDER);
            }
            ServeEvent::ConnectionFailed { .. } => {
                self.connections_failed_total.fetch_add(1, ORDER);
            }
            ServeEvent::AcceptBackoff { .. } => {
                self.accept_backoff_total.fetch_add(1, ORDER);
            }
            ServeEvent::AcceptGaveUp { .. } => {
                self.accept_gave_up_total.fetch_add(1, ORDER);
            }
            ServeEvent::CheckpointFailed { .. } => {
                self.checkpoints_failed_total.fetch_add(1, ORDER);
            }

            ServeEvent::DrainCompleted { cancelled, .. } => {
                // Only the forced half is counted. A turn that reached its
                // step boundary is a drain working as designed and needs no
                // counter; one that had to be cancelled is the signal that
                // the grace period is short for this workload, which is the
                // number worth watching across deploys.
                self.drain_cancelled_total
                    .fetch_add(as_u64(*cancelled), ORDER);
            }

            // Lifecycle markers carry no count.
            ServeEvent::Listening { .. } | ServeEvent::ShuttingDown { .. } => {}
        }
    }
}

fn as_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn as_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observe::event::{
        AbandonReason, Method, RequestId, Route, StreamEndReason, TurnRef, TurnTally,
    };

    fn request(status: Option<u16>) -> ServeEvent {
        ServeEvent::RequestCompleted {
            request_id: RequestId::generate(),
            method: Method::new("POST"),
            route: Route::TurnsCreate,
            status,
            duration_ms: 5,
            bytes_in: 10,
            bytes_out: 20,
            turn: None,
        }
    }

    fn turn() -> TurnRef {
        TurnRef::new("turn-abcdef0123456789")
    }

    /// Status classes are bucketed, and the hung-up peer — the case that had no
    /// record at all before the fold — lands in its own bucket rather than
    /// being miscounted as a success.
    #[test]
    fn request_status_classes_are_bucketed_including_no_answer() {
        let metrics = Metrics::new();
        metrics.emit(&request(Some(200)));
        metrics.emit(&request(Some(404)));
        metrics.emit(&request(Some(429)));
        metrics.emit(&request(Some(500)));
        metrics.emit(&request(None));

        let snap = metrics.snapshot();
        assert_eq!(snap.requests_total, 5);
        assert_eq!(snap.requests_2xx, 1);
        assert_eq!(snap.requests_4xx, 2);
        assert_eq!(snap.requests_5xx, 1);
        assert_eq!(snap.requests_unanswered, 1);
        assert_eq!(snap.request_duration_ms_total, 25);
        assert_eq!(snap.bytes_in_total, 50);
        assert_eq!(snap.bytes_out_total, 100);
    }

    /// A throttled 401 must be distinguishable from an ordinary one: the first
    /// is a brute force, the second is a misconfigured client.
    #[test]
    fn a_throttled_401_is_counted_apart_from_an_ordinary_one() {
        let metrics = Metrics::new();
        for held_ms in [0, 0, 250, 500] {
            metrics.emit(&ServeEvent::Unauthorized {
                request_id: RequestId::generate(),
                route: Route::TurnsCreate,
                held_ms,
            });
        }
        let snap = metrics.snapshot();
        assert_eq!(snap.unauthorized_total, 4);
        assert_eq!(snap.unauthorized_throttled_total, 2);
    }

    /// The in-flight gauge must return to zero across every way a reverse
    /// request can end — answered, timed out, or dropped wholesale. A gauge
    /// that only decrements on the happy path reads as a permanent leak.
    #[test]
    fn the_in_flight_gauge_balances_on_every_ending() {
        let metrics = Metrics::new();
        let dispatch = |kind| ServeEvent::ReverseDispatched {
            turn: turn(),
            request_id: RequestId::generate(),
            kind,
        };
        metrics.emit(&dispatch(ReverseKind::Provider));
        metrics.emit(&dispatch(ReverseKind::Tool));
        metrics.emit(&dispatch(ReverseKind::Tool));
        metrics.emit(&dispatch(ReverseKind::Tool));
        assert_eq!(metrics.snapshot().reverse_in_flight, 4);

        metrics.emit(&ServeEvent::ReverseAnswered {
            turn: turn(),
            request_id: RequestId::generate(),
            kind: ReverseKind::Provider,
            waited_ms: 40,
        });
        metrics.emit(&ServeEvent::ReverseTimedOut {
            turn: turn(),
            request_id: RequestId::generate(),
            kind: ReverseKind::Tool,
            waited_ms: 300_000,
        });
        metrics.emit(&ServeEvent::ReverseAbandoned {
            turn: turn(),
            in_flight: 2,
            reason: AbandonReason::Cancelled,
        });

        let snap = metrics.snapshot();
        assert_eq!(snap.reverse_in_flight, 0, "the gauge leaked");
        assert_eq!(snap.reverse_dispatched_provider_total, 1);
        assert_eq!(snap.reverse_dispatched_tool_total, 3);
        assert_eq!(snap.reverse_answered_total, 1);
        assert_eq!(snap.reverse_timed_out_total, 1);
        assert_eq!(snap.reverse_abandoned_total, 2);
        assert_eq!(snap.reverse_wait_ms_total, 300_040);
    }

    /// A decrement observed before its matching increment must read as zero,
    /// not as a negative count of live requests.
    #[test]
    fn a_gauge_never_reports_a_negative_count() {
        let metrics = Metrics::new();
        metrics.emit(&ServeEvent::ReverseAnswered {
            turn: turn(),
            request_id: RequestId::generate(),
            kind: ReverseKind::Tool,
            waited_ms: 1,
        });
        assert_eq!(metrics.snapshot().reverse_in_flight, 0);
    }

    /// The engine's own tally reaches the counters through `TurnSettled`, so
    /// retries and discarded speculation are visible without the server ever
    /// logging a per-frame record.
    #[test]
    fn the_turn_tally_folds_into_engine_counters() {
        let metrics = Metrics::new();
        metrics.emit(&ServeEvent::TurnSettled {
            turn: turn(),
            outcome: SettledOutcome::Completed,
            duration_ms: 900,
            tally: TurnTally {
                stages: 6,
                model_calls: 3,
                retries: 2,
                tool_calls: 5,
                tools_failed: 1,
                speculation_discarded: 4,
                loop_detections: 1,
                frames_dropped: 0,
            },
        });
        let snap = metrics.snapshot();
        assert_eq!(snap.turns_completed_total, 1);
        assert_eq!(snap.turns_aborted_total, 0);
        assert_eq!(snap.turn_duration_ms_total, 900);
        assert_eq!(snap.model_calls_total, 3);
        assert_eq!(snap.model_retries_total, 2);
        assert_eq!(snap.tool_calls_total, 5);
        assert_eq!(snap.tools_failed_total, 1);
        assert_eq!(snap.speculation_discarded_total, 4);
        assert_eq!(snap.loop_detections_total, 1);
    }

    /// The registry gauge tracks occupancy, and refusals are separated by
    /// cause: capacity is an operational signal, a collision is an RNG story.
    #[test]
    fn registry_occupancy_and_refusal_causes_are_distinct() {
        let metrics = Metrics::new();
        metrics.emit(&ServeEvent::TurnCreated {
            turn: turn(),
            live_turns: 7,
        });
        assert_eq!(metrics.snapshot().turns_live, 7);
        metrics.emit(&ServeEvent::TurnRefused {
            reason: RefusalReason::AtCapacity,
            live_turns: 32,
        });
        metrics.emit(&ServeEvent::TurnRefused {
            reason: RefusalReason::IdCollision,
            live_turns: 32,
        });
        let snap = metrics.snapshot();
        assert_eq!(snap.turns_live, 32);
        assert_eq!(snap.turns_refused_capacity_total, 1);
        assert_eq!(snap.turns_refused_collision_total, 1);
    }

    /// Discarded work reaches the counters too — that is the clause the audit
    /// contrasts `stella-serve` against `stella-core` on.
    #[test]
    fn discarded_work_is_counted_not_swallowed() {
        let metrics = Metrics::new();
        metrics.emit(&ServeEvent::TurnReclaimed {
            turn: turn(),
            buffered_frames: 12,
        });
        metrics.emit(&ServeEvent::FrameUnencodable {
            turn: turn(),
            error: "boom".to_string(),
        });
        metrics.emit(&ServeEvent::ConnectionFailed {
            request_id: RequestId::generate(),
            error: "reset".to_string(),
        });
        metrics.emit(&ServeEvent::StreamEnded {
            turn: turn(),
            frames_sent: 9,
            reason: StreamEndReason::PeerDisconnected,
        });
        let snap = metrics.snapshot();
        assert_eq!(snap.turns_reclaimed_total, 1);
        assert_eq!(snap.frames_unencodable_total, 1);
        assert_eq!(snap.connections_failed_total, 1);
        assert_eq!(snap.streams_ended_total, 1);
        assert_eq!(snap.frames_sent_total, 9);
    }

    /// The scrape body is a flat object of integers — no nesting, no strings,
    /// so nothing host-supplied can ride out through the metrics endpoint.
    #[test]
    fn the_snapshot_is_a_flat_object_of_numbers() {
        let metrics = Metrics::new();
        metrics.emit(&request(Some(200)));
        let json = serde_json::to_value(metrics.snapshot()).expect("serialize");
        let object = json.as_object().expect("an object");
        assert!(!object.is_empty());
        for (key, value) in object {
            assert!(
                value.is_number(),
                "`{key}` is not a number — only counts may leave this endpoint"
            );
        }
    }
}
