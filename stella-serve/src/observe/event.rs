// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The typed vocabulary of everything this server does at a boundary.
//!
//! Modelled on `stella-protocol`'s [`AgentEvent`](stella_protocol::AgentEvent),
//! deliberately: one variant per boundary, internally tagged, serde-first, and —
//! the part that matters most — **work the server throws away gets a name**
//! rather than vanishing into a `let _ =`. `AgentEvent::SpeculationDiscarded` is
//! the pattern; [`ServeEvent::ReverseTimedOut`] and its neighbours are this
//! crate's version of it.
//!
//! # Every field here is structurally content-free
//!
//! Records are written to stderr, and operators ship stderr. So no variant
//! carries prompt text, tool arguments, tool results, model output, a
//! filesystem path, a raw request path, or a full turn id. The only `String`s
//! are `addr` (operator-supplied), the two `error` fields (our own
//! [`ServeError`](crate::ServeError) / `io::Error` renderings), and the
//! sanitized [`RequestId`]. Everything else is an enum, an integer, or a
//! truncated [`TurnRef`].
//!
//! That is a property, not a convention: `observe::tests::no_content_reaches_a
//! _record` poisons a turn with sentinels and sweeps every emitted record for
//! them, in the shape `stella-store::content_free` uses for egress.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How loud a record is. Ordered so filtering is a comparison: an event passes
/// when `event.level() <= filter`.
///
/// Derived from the variant rather than stored on it, so a record's severity
/// can never disagree with what happened, and adding a variant forces the
/// author to classify it (the match in [`ServeEvent::level`] is exhaustive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

/// The configured verbosity, from `STELLA_SERVE_LOG`. [`LevelFilter::Off`]
/// silences the sink entirely; it does **not** stop events being emitted, so
/// counters stay correct at any verbosity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum LevelFilter {
    Off,
    Error,
    Warn,
    #[default]
    Info,
    Debug,
}

impl LevelFilter {
    /// Parse `STELLA_SERVE_LOG`. Unknown values fall back to the default rather
    /// than failing startup: a typo in a log knob must not take a service down.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "silent" => Self::Off,
            "error" => Self::Error,
            "warn" | "warning" => Self::Warn,
            "debug" | "trace" => Self::Debug,
            _ => Self::Info,
        }
    }

    /// Whether a record at `level` should be written.
    #[must_use]
    pub fn admits(self, level: Level) -> bool {
        match self {
            Self::Off => false,
            Self::Error => level <= Level::Error,
            Self::Warn => level <= Level::Warn,
            Self::Info => level <= Level::Info,
            Self::Debug => true,
        }
    }
}

/// Which endpoint a request reached, as a **template**.
///
/// The raw path is never recorded, because for five of these it contains the
/// turn id. Serde renames each variant to its template so a record reads like
/// an access log without any rendering step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Route {
    #[serde(rename = "/healthz")]
    Healthz,
    #[serde(rename = "/v1/metrics")]
    Metrics,
    #[serde(rename = "/v1/turns")]
    TurnsCreate,
    #[serde(rename = "/v1/turns/{id}/events")]
    TurnEvents,
    #[serde(rename = "/v1/turns/{id}/tool-result")]
    TurnToolResult,
    #[serde(rename = "/v1/turns/{id}/provider-result")]
    TurnProviderResult,
    #[serde(rename = "/v1/turns/{id}/cancel")]
    TurnCancel,
    #[serde(rename = "/v1/turns/{id}/steer")]
    TurnSteer,
    #[serde(rename = "/v1/turns/{id}/pause")]
    TurnPause,
    #[serde(rename = "/v1/turns/{id}/resume")]
    TurnResume,
    #[serde(rename = "/v1/sessions")]
    SessionsCreate,
    /// `GET` (read) and `DELETE` (end) share the member path — one resource,
    /// two verbs, one template in the records.
    #[serde(rename = "/v1/sessions/{id}")]
    Session,
    #[serde(rename = "/v1/sessions/{id}/turns")]
    SessionTurns,
    /// A path that matched no route. The path itself is deliberately dropped:
    /// a 404 probe is attacker-controlled text, and this is a log.
    #[serde(rename = "<unrouted>")]
    Unrouted,
}

impl Route {
    /// The path template, as it appears in a record.
    #[must_use]
    pub fn template(self) -> &'static str {
        match self {
            Self::Healthz => "/healthz",
            Self::Metrics => "/v1/metrics",
            Self::TurnsCreate => "/v1/turns",
            Self::TurnEvents => "/v1/turns/{id}/events",
            Self::TurnToolResult => "/v1/turns/{id}/tool-result",
            Self::TurnProviderResult => "/v1/turns/{id}/provider-result",
            Self::TurnCancel => "/v1/turns/{id}/cancel",
            Self::TurnSteer => "/v1/turns/{id}/steer",
            Self::TurnPause => "/v1/turns/{id}/pause",
            Self::TurnResume => "/v1/turns/{id}/resume",
            Self::SessionsCreate => "/v1/sessions",
            Self::Session => "/v1/sessions/{id}",
            Self::SessionTurns => "/v1/sessions/{id}/turns",
            Self::Unrouted => "<unrouted>",
        }
    }
}

/// How many characters of a turn id reach a record.
///
/// A turn id is a **second factor**: acting on a turn needs the bearer token
/// *and* the id, and `new_turn_id` spends 128 random bits precisely so the id
/// cannot be guessed from another. Writing it verbatim into a file that gets
/// shipped to a log aggregator spends that factor for nothing. Eight hex
/// characters is 32 bits — ample to correlate records inside one process's
/// lifetime, useless for forging a request against a live turn.
const TURN_REF_CHARS: usize = 8;

/// A turn id as it appears in a record: truncated, never whole.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TurnRef(String);

impl TurnRef {
    /// Truncate a full turn id (`turn-<32 hex>`) for recording.
    #[must_use]
    pub fn new(turn_id: &str) -> Self {
        let hex = turn_id.strip_prefix("turn-").unwrap_or(turn_id);
        Self(hex.chars().take(TURN_REF_CHARS).collect())
    }
}

impl std::fmt::Display for TurnRef {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A session id as it appears in a record: truncated, never whole.
///
/// Same reasoning as [`TurnRef`] — a session id is a second factor (it names a
/// retained conversation, and `DELETE` destroys one), minted from the same 128
/// random bits, so a record carries enough to correlate and not enough to act.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionRef(String);

impl SessionRef {
    /// Truncate a full session id (`session-<32 hex>`) for recording.
    #[must_use]
    pub fn new(session_id: &str) -> Self {
        let hex = session_id.strip_prefix("session-").unwrap_or(session_id);
        Self(hex.chars().take(TURN_REF_CHARS).collect())
    }
}

impl std::fmt::Display for SessionRef {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Longest host-supplied `X-Request-Id` we will echo and record.
const MAX_REQUEST_ID_LEN: usize = 64;

/// A request correlation id — echoed on the response as `X-Request-Id`.
///
/// A host-supplied value is **sanitized before it is stored**, not on the way
/// out. An unsanitized header echoed into a line-oriented log is a log-forging
/// vector: `serde_json` escaping stops a newline from splitting a record, but
/// nothing stops a caller from making every line it touches a kilobyte wide, or
/// from embedding text that reads like a different record to a human scanning
/// the file. Anything failing the charset or the cap is replaced outright.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestId(String);

impl RequestId {
    /// Mint an unguessable id. Not security-critical — nothing is authorized by
    /// a request id — but sequential ids would let a caller infer the server's
    /// total request volume from two samples.
    #[must_use]
    pub fn generate() -> Self {
        use rand::Rng as _;
        let mut bytes = [0_u8; 8];
        rand::rng().fill_bytes(&mut bytes);
        let mut id = String::with_capacity(16);
        for byte in bytes {
            use std::fmt::Write as _;
            let _ = write!(id, "{byte:02x}");
        }
        Self(id)
    }

    /// Accept a host-supplied `X-Request-Id`, or `None` if it is unusable.
    ///
    /// Conservative on purpose: `[A-Za-z0-9._-]` covers every id format in
    /// practice (UUIDs, hex, ULIDs, `trace-span` pairs) and admits nothing that
    /// could be mistaken for structure in a JSON line or a terminal.
    #[must_use]
    pub fn from_header(value: &str) -> Option<Self> {
        let trimmed = value.trim();
        let ok = !trimmed.is_empty()
            && trimmed.len() <= MAX_REQUEST_ID_LEN
            && trimmed
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        ok.then(|| Self(trimmed.to_string()))
    }

    /// Accept a host-supplied id for *recording only*, replacing an unusable one
    /// with a fixed marker rather than a generated id.
    ///
    /// Used where the id names something the host chose (a reverse-request id it
    /// answered with) rather than something we minted. A marker beats a fresh
    /// random id there: the point of the record is "the host sent us this", and
    /// a plausible-looking generated id would suggest the host sent something
    /// legitimate when it did not.
    #[must_use]
    pub fn sanitized(value: &str) -> Self {
        Self::from_header(value).unwrap_or_else(|| Self("<unusable>".to_string()))
    }

    /// The value to echo in the `X-Request-Id` response header.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Longest HTTP method we will record.
const MAX_METHOD_LEN: usize = 16;

/// A request method, sanitized for recording.
///
/// The method is host-supplied and reaches a record on the 405/404 paths, so it
/// gets the same treatment as [`RequestId`]: ASCII letters only, capped, and
/// replaced wholesale when it fails. RFC 9110 §9 methods are all short and
/// alphabetic; anything else was never going to route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Method(String);

impl Method {
    #[must_use]
    pub fn new(raw: &str) -> Self {
        let usable = !raw.is_empty()
            && raw.len() <= MAX_METHOD_LEN
            && raw.bytes().all(|b| b.is_ascii_alphabetic());
        if usable {
            Self(raw.to_ascii_uppercase())
        } else {
            Self("<invalid>".to_string())
        }
    }
}

/// Why `POST /v1/turns` was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    /// The registry is at `MAX_LIVE_TURNS` and nothing could be reclaimed.
    AtCapacity,
    /// A freshly minted id was already registered. Unreachable in practice at
    /// 128 bits — recorded because if it ever fires, the RNG is the story.
    IdCollision,
}

/// How a turn ended, as the registry saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettledOutcome {
    Completed,
    Aborted,
}

/// Why an SSE stream stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamEndReason {
    /// The terminal `turn_complete` frame was written. The healthy ending.
    TurnComplete,
    /// The subscriber hung up. The turn is parked for the resume window rather
    /// than cancelled outright — see `ServerState::park_for_resume`.
    PeerDisconnected,
    /// A parked turn's resume window elapsed with no reconnect, so the server
    /// gave up on it and cancelled. Distinct from [`Self::PeerDisconnected`]
    /// on purpose: the first says a client *may* come back, this one says the
    /// work was definitively thrown away, which is the one worth alerting on.
    ResumeWindowExpired,
    /// A write to the subscriber failed mid-stream.
    WriteFailed,
    /// The peer sent more than the tolerated inbound bytes on a stream that
    /// owes it nothing.
    StrayBytes,
    /// The session's frame channel closed without a terminal frame.
    SessionEnded,
}

impl StreamEndReason {
    /// Whether this ending means *the subscriber* went away while the turn
    /// itself is still worth keeping — the condition for parking it rather than
    /// cancelling it outright.
    ///
    /// Two reasons qualify, and they are two names for one event. A stream
    /// reads the peer's half while it writes frames, so a subscriber that hangs
    /// up is discovered by whichever of the two notices first: the read, as EOF
    /// ([`Self::PeerDisconnected`]), or the next write, as a broken pipe
    /// ([`Self::WriteFailed`]). Those races are decided by `tokio::select!`,
    /// which polls its branches in a deliberately randomized order — so without
    /// this, a coin flip inside the runtime decided whether a host that dropped
    /// its connection could resume the turn it had already paid a model call
    /// for, or got `404 unknown turn` on reconnect. The distinction is still
    /// worth *recording* (a failed write says the socket died mid-frame, an EOF
    /// says it closed cleanly); it is not worth a different lifecycle.
    ///
    /// Everything else is final. [`Self::StrayBytes`] is a peer that is very
    /// much still there and speaking the protocol wrongly; the rest describe
    /// the *turn* ending, and a finished turn has nothing left to resume.
    #[must_use]
    pub fn leaves_the_turn_resumable(self) -> bool {
        matches!(self, Self::PeerDisconnected | Self::WriteFailed)
    }
}

/// Which port raised a reverse request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReverseKind {
    Provider,
    Tool,
}

/// Why a host answer matched nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MisrouteFault {
    /// No in-flight request under that id: already answered, already timed out,
    /// or fabricated.
    UnknownRequest,
    /// A real in-flight request, answered with the wrong kind of result.
    KindMismatch,
}

/// Why in-flight reverse requests were dropped wholesale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbandonReason {
    /// `POST /v1/turns/{id}/cancel`, or a dropped `Session`.
    Cancelled,
    /// The turn settled normally and the registry was swept.
    Teardown,
}

/// The accept-loop condition that triggered a backoff, mirroring
/// `accept::AcceptAction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptKind {
    /// Descriptor or memory exhaustion — the listener is fine, the process is
    /// under pressure.
    Exhaustion,
    /// An `io::ErrorKind` this platform does not name.
    Unnameable,
}

/// Why the server is stopping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownReason {
    /// The listener returned a fatal error, or backoff gave up.
    ListenerFailed,
}

/// What one turn actually did, folded from the engine's own `AgentEvent`
/// stream as it passes through.
///
/// Counted, never logged per event: `AgentEvent::TextDelta` fires per token, so
/// a record per frame would make this server slower than the model it waits on.
/// The whole tally rides one [`ServeEvent::TurnSettled`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnTally {
    /// Pipeline stage boundaries — the progress axis. A turn whose `stages`
    /// stopped advancing while a reverse request's wait climbs is *wedged*; one
    /// that keeps advancing is merely slow.
    pub stages: u32,
    pub model_calls: u32,
    /// Model-call retries the engine performed (`AgentEvent::Retry`).
    pub retries: u32,
    pub tool_calls: u32,
    pub tools_failed: u32,
    /// Speculative work the engine discarded — `stella-core` already names it,
    /// so this server counts it rather than re-deriving it.
    pub speculation_discarded: u32,
    pub loop_detections: u32,
}

/// One thing the server did, at a boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ServeEvent {
    // ---- process lifecycle ----
    /// The listener is bound and accepting. Replaces the startup `println!`.
    ///
    /// Carries the compiled `Host`-guard mode because a guard that is
    /// [`crate::HostMode::Inert`] — the honest answer for a `0.0.0.0` bind
    /// with no allow-list — must be visible to an operator at startup rather
    /// than mistaken for one that is enforcing (#1130).
    Listening {
        addr: String,
        host_guard: crate::HostMode,
    },
    ShuttingDown {
        reason: ShutdownReason,
        live_turns: usize,
    },

    // ---- the request fold: exactly one per accepted connection ----
    /// The terminal record for one connection. Constructed **only** by
    /// `RequestRecord::close`, so no request can exit unrecorded.
    RequestCompleted {
        request_id: RequestId,
        method: Method,
        route: Route,
        /// `None` when the peer hung up before we answered. Absence is a
        /// terminal state here, not a missing field.
        status: Option<u16>,
        duration_ms: u64,
        bytes_in: u64,
        bytes_out: u64,
        turn: Option<TurnRef>,
    },
    /// Split out of `RequestCompleted` so a brute force is one grep. `held_ms`
    /// is the throttle's own delay — visible to us, invisible to the guesser,
    /// whose response is byte-identical either way.
    Unauthorized {
        request_id: RequestId,
        route: Route,
        held_ms: u64,
    },
    /// A request was refused by the `Host` guard before any routing happened
    /// (#1130). Split out of `RequestCompleted` for the same reason
    /// `Unauthorized` is: the two things an operator wants to find here — a
    /// rebinding attempt, and their own missing allow-list entry — are one
    /// grep rather than a scan of every 403.
    ///
    /// `host` is attacker-controlled, so it is truncated by
    /// [`HOST_LOG_LIMIT`]: it is worth recording because it is exactly what
    /// tells a misconfiguration apart from an attack, and worth bounding
    /// because a 64 KiB header would otherwise become a log-flooding lever.
    HostRejected {
        request_id: RequestId,
        route: Route,
        host: String,
    },

    // ---- turn registry ----
    TurnCreated {
        turn: TurnRef,
        live_turns: usize,
    },
    TurnRefused {
        reason: RefusalReason,
        live_turns: usize,
    },
    TurnSettled {
        turn: TurnRef,
        outcome: SettledOutcome,
        duration_ms: u64,
        tally: TurnTally,
    },
    StreamEnded {
        turn: TurnRef,
        frames_sent: u64,
        reason: StreamEndReason,
    },

    // ---- session registry (#931) ----
    SessionCreated {
        session: SessionRef,
        live_sessions: usize,
    },
    /// The registry is at `MAX_SESSIONS` and nothing idle had aged out.
    SessionRefused {
        live_sessions: usize,
    },
    SessionDeleted {
        session: SessionRef,
        live_sessions: usize,
    },
    /// An idle-expired session evicted to admit a new one. Lossy — the
    /// conversation is gone and its id answers `404` — which is why it is
    /// only done under pressure and only past the idle deadline, and why it
    /// is reported here rather than being a silent `remove`.
    SessionReclaimed {
        session: SessionRef,
        idle_ms: u64,
    },

    // ---- reverse RPC: the wedge axis ----
    ReverseDispatched {
        turn: TurnRef,
        request_id: RequestId,
        kind: ReverseKind,
    },
    ReverseAnswered {
        turn: TurnRef,
        request_id: RequestId,
        kind: ReverseKind,
        waited_ms: u64,
    },

    // ---- work the server threw away ----
    /// The host never answered and the port gave up at its deadline. **The**
    /// wedge signal: before this, `Pending::abandon` was a silent `remove`.
    ReverseTimedOut {
        turn: TurnRef,
        request_id: RequestId,
        kind: ReverseKind,
        waited_ms: u64,
    },
    /// A host answer that matched nothing, or matched the wrong kind.
    ReverseMisrouted {
        request_id: RequestId,
        fault: MisrouteFault,
    },
    /// In-flight reverse requests dropped wholesale. Only emitted when
    /// `in_flight > 0` — a clean teardown discards nothing and says nothing.
    ReverseAbandoned {
        turn: TurnRef,
        in_flight: usize,
        reason: AbandonReason,
    },
    /// A finished-but-unstreamed turn evicted to admit a new one, with the
    /// frames nobody ever read.
    TurnReclaimed {
        turn: TurnRef,
        buffered_frames: usize,
    },
    /// `encode_or_abort` substituted a terminal frame: the turn died of a bug
    /// in our own serialization, and used to say nothing about it.
    FrameUnencodable {
        turn: TurnRef,
        error: String,
    },
    /// The `let _ = handle_conn(..)` at the accept loop, given a name. Routine
    /// (client hangups live here), hence `Debug`.
    ConnectionFailed {
        request_id: RequestId,
        error: String,
    },
    AcceptBackoff {
        kind: AcceptKind,
        delay_ms: u64,
        streak: u32,
    },
    /// The accept loop stopped retrying a condition that never cleared.
    AcceptGaveUp {
        kind: AcceptKind,
        streak: u32,
    },
}

impl ServeEvent {
    /// This event's severity. Exhaustive by construction: a new variant does
    /// not compile until it is classified.
    #[must_use]
    pub fn level(&self) -> Level {
        match self {
            // Routine progress.
            Self::Listening { .. }
            | Self::ShuttingDown { .. }
            | Self::RequestCompleted { .. }
            | Self::TurnCreated { .. }
            | Self::TurnSettled { .. }
            | Self::StreamEnded { .. }
            | Self::SessionCreated { .. }
            | Self::SessionDeleted { .. } => Level::Info,

            // Per model/tool call — a few dozen a turn, so off by default.
            Self::ReverseDispatched { .. } | Self::ReverseAnswered { .. } => Level::Debug,
            // Client hangups are ordinary and would drown the default level.
            Self::ConnectionFailed { .. } => Level::Debug,

            // Somebody is misbehaving, or the server is under pressure.
            Self::Unauthorized { .. }
            | Self::HostRejected { .. }
            | Self::TurnRefused { .. }
            | Self::TurnReclaimed { .. }
            | Self::SessionRefused { .. }
            | Self::SessionReclaimed { .. }
            | Self::ReverseMisrouted { .. }
            | Self::ReverseAbandoned { .. }
            | Self::AcceptBackoff { .. } => Level::Warn,

            // Something is broken and a human should look.
            Self::ReverseTimedOut { .. }
            | Self::FrameUnencodable { .. }
            | Self::AcceptGaveUp { .. } => Level::Error,
        }
    }
}

/// Round a duration to whole milliseconds for a record.
#[must_use]
pub fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// How many characters of a refused `Host` header reach a record.
///
/// Long enough for any real hostname (the DNS limit is 253) plus a port,
/// short enough that the 64 KiB a peer may spend on a request head cannot be
/// turned into 64 KiB of log line per connection.
const HOST_LOG_LIMIT: usize = 256;

/// A refused `Host` as it appears in a record: bounded, never verbatim.
///
/// Truncation is on a **character** boundary, not a byte one: a `Host` header
/// is `from_utf8_lossy`'d at parse time, so it can carry multi-byte
/// characters, and slicing those by byte index panics.
#[must_use]
pub fn host_for_record(host: &str) -> String {
    if host.chars().count() <= HOST_LOG_LIMIT {
        return host.to_string();
    }
    host.chars().take(HOST_LOG_LIMIT).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Invariant 4: every type crossing a crate boundary round-trips through
    /// `serde_json` byte-for-byte.
    #[test]
    fn events_round_trip_byte_for_byte() {
        let events = vec![
            ServeEvent::Listening {
                addr: "127.0.0.1:8080".to_string(),
                host_guard: crate::HostMode::Loopback,
            },
            ServeEvent::RequestCompleted {
                request_id: RequestId("abc123".to_string()),
                method: Method::new("POST"),
                route: Route::TurnsCreate,
                status: Some(200),
                duration_ms: 3,
                bytes_in: 512,
                bytes_out: 48,
                turn: Some(TurnRef::new("turn-0123456789abcdef")),
            },
            ServeEvent::RequestCompleted {
                request_id: RequestId::generate(),
                method: Method::new("GET"),
                route: Route::Unrouted,
                // The hung-up case: no status, and it must survive the trip.
                status: None,
                duration_ms: 0,
                bytes_in: 0,
                bytes_out: 0,
                turn: None,
            },
            ServeEvent::ReverseTimedOut {
                turn: TurnRef::new("turn-deadbeefcafe"),
                request_id: RequestId("prov-0".to_string()),
                kind: ReverseKind::Provider,
                waited_ms: 300_000,
            },
            ServeEvent::TurnSettled {
                turn: TurnRef::new("turn-deadbeefcafe"),
                outcome: SettledOutcome::Aborted,
                duration_ms: 12,
                tally: TurnTally {
                    stages: 4,
                    model_calls: 2,
                    retries: 1,
                    tool_calls: 3,
                    tools_failed: 1,
                    speculation_discarded: 1,
                    loop_detections: 0,
                },
            },
        ];
        for event in events {
            let json = serde_json::to_string(&event).expect("serialize");
            let back: ServeEvent = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(event, back, "round trip changed the value: {json}");
            let again = serde_json::to_string(&back).expect("re-serialize");
            assert_eq!(json, again, "round trip is not byte-stable");
        }
    }

    /// A subscriber hanging up is *one* event with two names, because the
    /// stream discovers it from whichever of its two halves touches the dead
    /// socket first — the read, as EOF, or the next write, as a broken pipe.
    /// Both must keep the turn resumable.
    ///
    /// This is the invariant, not a restatement of the implementation: the two
    /// are raced by a `tokio::select!` that randomizes which branch it polls,
    /// so when only `PeerDisconnected` parked the turn, a coin flip inside the
    /// runtime decided whether a host that dropped its connection could resume
    /// the turn it had already paid a model call for — the other side of the
    /// flip cancelled it and answered the reconnect `404 unknown turn`.
    #[test]
    fn both_spellings_of_a_hang_up_keep_the_turn_resumable() {
        assert!(StreamEndReason::PeerDisconnected.leaves_the_turn_resumable());
        assert!(
            StreamEndReason::WriteFailed.leaves_the_turn_resumable(),
            "a write that failed because the subscriber is gone is that \
             subscriber being gone, and must park the turn exactly as an EOF \
             does — otherwise which of two ready select! branches the runtime \
             happens to poll decides whether the turn survives"
        );
    }

    /// The other half of the same rule: an ending that is *not* the peer going
    /// away must stay final, or a settled turn would be parked — holding an OS
    /// thread and a registry slot for a reconnect that has nothing to resume.
    #[test]
    fn an_ending_that_is_not_a_hang_up_is_final() {
        for reason in [
            StreamEndReason::TurnComplete,
            StreamEndReason::ResumeWindowExpired,
            StreamEndReason::SessionEnded,
            // The peer is emphatically still there — it is talking, just
            // wrongly. Nothing to wait for.
            StreamEndReason::StrayBytes,
        ] {
            assert!(
                !reason.leaves_the_turn_resumable(),
                "{reason:?} does not describe a subscriber going away"
            );
        }
    }

    /// A turn id is a second factor. A record must carry enough to correlate
    /// and not enough to act.
    #[test]
    fn a_turn_ref_is_truncated_not_whole() {
        let full = "turn-0123456789abcdef0123456789abcdef";
        let reference = TurnRef::new(full);
        assert_eq!(reference.to_string(), "01234567");
        assert!(
            !full.contains(&format!("{reference}0")) || full.len() > reference.to_string().len(),
            "sanity: the reference is a strict prefix",
        );
        assert_eq!(reference.to_string().len(), TURN_REF_CHARS);
    }

    /// The route is a template, so no record can carry a turn id in its path.
    #[test]
    fn every_route_template_is_free_of_concrete_ids() {
        for route in [
            Route::Healthz,
            Route::Metrics,
            Route::TurnsCreate,
            Route::TurnEvents,
            Route::TurnToolResult,
            Route::TurnProviderResult,
            Route::TurnCancel,
            Route::TurnSteer,
            Route::TurnPause,
            Route::TurnResume,
            Route::SessionsCreate,
            Route::Session,
            Route::SessionTurns,
            Route::Unrouted,
        ] {
            let json = serde_json::to_string(&route).expect("serialize");
            assert_eq!(json, format!("\"{}\"", route.template()));
            assert!(
                !route.template().contains("turn-") && !route.template().contains("session-"),
                "{} embeds a concrete id",
                route.template()
            );
        }
    }

    /// Same second-factor posture as a turn id: a record correlates a
    /// session, it cannot address one.
    #[test]
    fn a_session_ref_is_truncated_not_whole() {
        let full = "session-0123456789abcdef0123456789abcdef";
        let reference = SessionRef::new(full);
        assert_eq!(reference.to_string(), "01234567");
        assert_eq!(reference.to_string().len(), TURN_REF_CHARS);
    }

    /// Log forging: a header that could split or bloat a record is refused
    /// outright rather than escaped and kept.
    #[test]
    fn a_hostile_request_id_header_is_refused() {
        assert!(RequestId::from_header("a1b2-c3d4.e5").is_some());
        assert!(RequestId::from_header("").is_none());
        assert!(RequestId::from_header("   ").is_none());
        assert!(
            RequestId::from_header("has space").is_none(),
            "whitespace could be read as structure by a human scanning the log"
        );
        assert!(RequestId::from_header("line\nbreak").is_none());
        assert!(RequestId::from_header("quote\"brace}").is_none());
        assert!(
            RequestId::from_header(&"x".repeat(MAX_REQUEST_ID_LEN + 1)).is_none(),
            "an unbounded id makes every line it touches unbounded"
        );
        assert!(RequestId::from_header(&"x".repeat(MAX_REQUEST_ID_LEN)).is_some());
    }

    /// A method reaches a record on the 404/405 paths, so it gets the same
    /// treatment as a request id.
    #[test]
    fn a_hostile_method_is_replaced_not_recorded() {
        assert_eq!(Method::new("post"), Method::new("POST"));
        assert_eq!(Method::new("GET\r\nX: y"), Method("<invalid>".to_string()));
        assert_eq!(Method::new(""), Method("<invalid>".to_string()));
        assert_eq!(
            Method::new(&"A".repeat(MAX_METHOD_LEN + 1)),
            Method("<invalid>".to_string())
        );
    }

    /// A typo in a log knob must not take the service down, and `off` must
    /// actually silence rather than merely quieten.
    #[test]
    fn the_level_filter_is_forgiving_and_off_is_total() {
        assert_eq!(LevelFilter::parse("nonsense"), LevelFilter::Info);
        assert_eq!(LevelFilter::parse(" DEBUG "), LevelFilter::Debug);
        assert_eq!(LevelFilter::parse("off"), LevelFilter::Off);
        for level in [Level::Error, Level::Warn, Level::Info, Level::Debug] {
            assert!(!LevelFilter::Off.admits(level));
            assert!(LevelFilter::Debug.admits(level));
        }
        assert!(LevelFilter::Error.admits(Level::Error));
        assert!(!LevelFilter::Error.admits(Level::Warn));
        assert!(LevelFilter::Info.admits(Level::Info));
        assert!(!LevelFilter::Info.admits(Level::Debug));
    }

    /// The wedge signal and its neighbours must be loud; the per-call chatter
    /// must not be, or the default level becomes unreadable and gets turned off.
    #[test]
    fn severity_matches_what_an_operator_needs_to_see() {
        let turn = TurnRef::new("turn-abcdef01");
        let id = RequestId("r".to_string());
        assert_eq!(
            ServeEvent::ReverseTimedOut {
                turn: turn.clone(),
                request_id: id.clone(),
                kind: ReverseKind::Tool,
                waited_ms: 1,
            }
            .level(),
            Level::Error
        );
        assert_eq!(
            ServeEvent::ReverseMisrouted {
                request_id: id.clone(),
                fault: MisrouteFault::UnknownRequest,
            }
            .level(),
            Level::Warn
        );
        assert_eq!(
            ServeEvent::ReverseDispatched {
                turn,
                request_id: id,
                kind: ReverseKind::Tool,
            }
            .level(),
            Level::Debug
        );
    }
}
