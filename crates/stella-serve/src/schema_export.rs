// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A machine-checked description of the **serve transport** wire format.
//!
//! `stella-protocol`'s `schema_export` describes [`AgentEvent`] — the payload.
//! This module describes the envelope around it: the [`ServerFrame`] union a
//! host reads off the SSE stream, and the two result bodies it POSTs back.
//! Together they are everything a client of `stella-serve` parses or produces.
//!
//! Split from the protocol's exporter rather than merged into it because the
//! two contracts have different owners and different blast radii. `AgentEvent`
//! is consumed by the TUI, `--output-format stream-json`, *and* this server; a
//! `ServerFrame` exists only between this server and its host. Publishing them
//! as one artifact would imply a coupling that is not there, and would make a
//! transport change look like a change to the CLI's output format.
//!
//! The TypeScript printer is *not* duplicated: this calls
//! [`stella_protocol::schema_export::typescript_declarations_with_header`], so
//! there is one subset of JSON Schema to keep in step rather than two.
//!
//! # What the schema cannot say
//!
//! Two properties of the live stream are outside JSON Schema and are stated in
//! the generated banner instead, because a client that misses them breaks:
//!
//! - **Every frame carries a `seq`** that is not on [`ServerFrame`] itself. It
//!   is added by the transport at delivery time (`crate::history`), so the
//!   object on the wire is the frame's own shape *plus* `seq`. The `.d.ts`
//!   models that as an intersection, and `envelope_shape_is_frame_plus_seq` in
//!   this crate's tests pins it against a really-serialized frame.
//! - **`replay_truncated` is a transport frame, not an engine one.** It is
//!   emitted when a resume point has aged out of the retained ring, and it
//!   deliberately has no `seq`: it describes what the server can no longer
//!   supply rather than something that happened in the turn.

use serde_json::Value;
use stella_protocol::schema_export::{UnsupportedSchema, typescript_declarations_with_header};

use crate::engine_overrides::EngineOverrides;
use crate::frame::{
    ProviderDeltaIn, ProviderResultIn, ScopeReviewResultIn, ServerFrame, ToolResultIn,
};

/// The JSON Schema (2020-12) for one outbound frame.
#[must_use]
pub fn server_frame_schema() -> Value {
    schemars::schema_for!(ServerFrame).to_value()
}

/// The JSON Schema for the tool-result body a host POSTs back.
#[must_use]
pub fn tool_result_schema() -> Value {
    schemars::schema_for!(ToolResultIn).to_value()
}

/// The JSON Schema for the provider-result body a host POSTs back.
#[must_use]
pub fn provider_result_schema() -> Value {
    schemars::schema_for!(ProviderResultIn).to_value()
}

/// The JSON Schema for the provider-delta body a streaming host POSTs (#1165).
#[must_use]
pub fn provider_delta_schema() -> Value {
    schemars::schema_for!(ProviderDeltaIn).to_value()
}

/// The JSON Schema for the scope-review decision a host POSTs back.
///
/// The answer to a [`ServerFrame::ScopeReviewRequest`], sent to
/// `POST /v1/turns/{id}/approve`. It was missing from the published contract
/// until #1497: the outbound schema described the *request* frame in six
/// places while the inbound document offered no type for the reply, so a host
/// generating a client from these artifacts could parse the question and had
/// nothing to construct the answer from.
#[must_use]
pub fn scope_review_result_schema() -> Value {
    schemars::schema_for!(ScopeReviewResultIn).to_value()
}

/// The JSON Schema for the optional `engine` object on `POST /v1/turns` and
/// `POST /v1/sessions/{id}/turns` (#1167).
#[must_use]
pub fn engine_overrides_schema() -> Value {
    schemars::schema_for!(EngineOverrides).to_value()
}

/// Every inbound body in one document, as a `$defs` container.
///
/// A container rather than a `oneOf`: these are the bodies of **different
/// endpoints**, not arms of one tagged union, and none carries a
/// discriminant. Publishing them as a union would tell a client it may send
/// any shape to any route, which is false — and the shared TypeScript
/// printer rejected exactly that framing when it was tried, because a `oneOf`
/// with no `type` const is not something it can print an honest discriminated
/// union from. The printer was right; the modeling was wrong.
#[must_use]
pub fn inbound_schema() -> Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "StellaServeInbound",
        "description":
            "Bodies a host POSTs back to answer a reverse request. ToolResultIn \
             answers POST /v1/turns/{id}/tool-result; ProviderResultIn answers \
             POST /v1/turns/{id}/provider-result; ProviderDeltaIn optionally \
             streams fragments of an in-flight provider answer to \
             POST /v1/turns/{id}/provider-delta ahead of its ProviderResultIn; \
             ScopeReviewResultIn answers POST /v1/turns/{id}/approve, carrying \
             the human's decision on a scope_review_request frame. \
             Each is keyed by the request_id carried on the frame it answers. \
             EngineOverrides is not a reverse-request answer at all: it is the \
             optional `engine` object on POST /v1/turns and \
             POST /v1/sessions/{id}/turns, published here because it is wire \
             contract. This document is a definitions container, not a union: \
             the bodies are not interchangeable and carry no discriminant.",
        "$defs": {
            "ToolResultIn": tool_result_schema(),
            "ProviderResultIn": provider_result_schema(),
            "ProviderDeltaIn": provider_delta_schema(),
            "ScopeReviewResultIn": scope_review_result_schema(),
            "EngineOverrides": engine_overrides_schema(),
        },
    })
}

/// Every committed artifact, as `(filename, contents)`.
///
/// # Errors
///
/// [`UnsupportedSchema`] when a generated schema uses a construct the shared
/// TypeScript printer does not model. Loud rather than approximate: a `.d.ts`
/// that lies is worse than no `.d.ts`.
pub fn artifacts() -> Result<Vec<(&'static str, String)>, UnsupportedSchema> {
    let outbound = server_frame_schema();

    let mut ts = typescript_declarations_with_header(&outbound, OUTBOUND_HEADER)?;
    ts.push_str(ENVELOPE_SUFFIX);
    // Printed one root at a time, for the reason `inbound_schema` documents:
    // they are two endpoint bodies, so each prints as its own interface rather
    // than as an arm of a union nothing discriminates.
    ts.push_str(&typescript_declarations_with_header(
        &tool_result_schema(),
        INBOUND_HEADER,
    )?);
    ts.push_str(&typescript_declarations_with_header(
        &provider_result_schema(),
        "",
    )?);
    ts.push_str(&typescript_declarations_with_header(
        &provider_delta_schema(),
        DELTA_HEADER,
    )?);
    ts.push_str(&typescript_declarations_with_header(
        &scope_review_result_schema(),
        SCOPE_REVIEW_HEADER,
    )?);
    ts.push_str(&typescript_declarations_with_header(
        &engine_overrides_schema(),
        ENGINE_HEADER,
    )?);

    let mut json =
        serde_json::to_string_pretty(&outbound).expect("a JSON Schema is always serializable");
    json.push('\n');
    let mut inbound_json = serde_json::to_string_pretty(&inbound_schema())
        .expect("a JSON Schema is always serializable");
    inbound_json.push('\n');

    Ok(vec![
        ("serveframe.schema.json", json),
        ("serveinbound.schema.json", inbound_json),
        ("serveframe.d.ts", ts),
    ])
}

const OUTBOUND_HEADER: &str = "\
// GENERATED FILE — DO NOT EDIT.
//
// Regenerate with:  bash scripts/export-agentevent-schema.sh
// Source of truth:  stella-serve/src/frame.rs
// Guarded by:       scripts/check-wire-schema.sh (`make wire-schema`)
//
// The `stella-serve` transport contract: what a host reads off
// `GET /v1/turns/{id}/events` and what it POSTs back.
//
// Frames arrive as SSE events. Each has an `id:` line carrying the frame's
// `seq` and a `data:` line carrying the JSON below.
";

/// The one piece of the contract the schema cannot express, written once here
/// rather than left for each consumer to rediscover.
const ENVELOPE_SUFFIX: &str = "
// ── the envelope ────────────────────────────────────────────────────────────
//
// `seq` is added by the transport at delivery time, not by the engine, so it
// is not a field on any ServerFrame variant above. On the wire it sits
// alongside them: `{\"seq\":12,\"type\":\"event\",\"event\":{…}}`.
//
// It is monotonic and gapless in DELIVERY order, starting at 1, and is also
// emitted as the SSE `id:` line — which is what lets a browser EventSource
// resume automatically via `Last-Event-ID`.

/** One frame as it appears on the wire: the engine's frame, plus its seq. */
export type StellaWireFrame = ServerFrame & { seq: number };

/**
 * Sent instead of a replay when the requested resume point has already been
 * evicted from the server's retained ring.
 *
 * Deliberately has no `seq`: it describes what the transport can no longer
 * supply, not something that happened in the turn. Receiving it means the
 * frames between `requested_after` and `oldest_retained` are unrecoverable —
 * reconnect with `?after=` one less than `oldest_retained` to replay what is
 * still held (an `?after=0` resume just re-answers `replay_truncated` unless
 * the ring still holds seq 1), or abandon the turn.
 */
export interface ReplayTruncated {
  type: \"replay_truncated\";
  /** The seq the client asked to resume after. */
  requested_after: number;
  /** The oldest seq the server still holds. */
  oldest_retained: number;
}

/** Anything a `data:` line can carry. */
export type StellaSseFrame = StellaWireFrame | ReplayTruncated;

// ── holds ARE re-learned by replay ──────────────────────────────────────────
//
// `turn_held` / `turn_released` are ordinary numbered frames, so a resumed
// stream replays them like any other and a client reconnecting mid-hold
// rediscovers that the turn is waiting on it. This is the OPPOSITE of the
// reverse-request rule stated below, and the difference is deliberate: an
// obligation is not re-announced because `?after=N` asserts you already have
// it, whereas a hold is a *state* the turn is still in. Read the tail from a
// `turn_held` with no matching `turn_released` after it and the turn is still
// held; post /resume to release it.
";

/// Rides above `ProviderDeltaIn` in the printed `.d.ts`: the streaming half is
/// optional and advisory, which is a contract the schema cannot express.
const DELTA_HEADER: &str = "
// ── inbound, optional: streaming a provider answer ──────────────────────────
//
// A host that streams its model call MAY POST batches of fragments to
// `POST /v1/turns/{id}/provider-delta` while the provider_request is in
// flight, keyed by the same request_id its eventual ProviderResultIn answers.
// Fragments surface on /events as text_delta / reasoning frames (so second
// subscribers and resuming clients see them) and each batch resets the
// reverse-request deadline. Strictly advisory: the definitive text is the
// CompletionResult on the terminating provider-result POST — a retried call
// re-streams from the start with no reset marker. A host that cannot stream
// simply never uses this route.
";

/// Rides above `EngineOverrides` in the printed `.d.ts`: this is a request
/// sub-object, not a reverse-request answer, and its clamp semantics live
/// outside what the schema can say.
const SCOPE_REVIEW_HEADER: &str = "
// ── inbound: answering a scope review ───────────────────────────────────────
//
// The engine parks a turn on a `scope_review_request` frame when a plan needs
// a human decision before it executes. The host answers by POSTing this body
// to `POST /v1/turns/{id}/approve`, keyed by the frame's request_id.
//
// `decision` is an internally-tagged union: approve executes the plan as
// proposed, trim executes only the listed step indices, and the note-carrying
// arm re-plans with the reviewer's words folded in — an empty note reads as
// abort, the same collapse the stdio gate makes for a bare \"no\".
";

const ENGINE_HEADER: &str = "
// ── request-side: the optional `engine` object on POST /v1/turns ────────────
//
// Per-turn engine knobs (#1167), also accepted on
// POST /v1/sessions/{id}/turns. Lowered onto the server's defaults: an
// omitted field keeps the default, an empty object is a no-op. Unusable
// values (a zero cap, a NaN temperature) are refused with a 400 naming the
// knob; values past an operator ceiling are clamped, and every clamp is
// reported in the create response's `clamped` array as
// {knob, requested, effective} — a request is never silently honored at a
// value it did not get. retry_policy and loop_detection are operator policy
// and are deliberately not on this object.
";

const INBOUND_HEADER: &str = "
// ── inbound: answering a reverse request ────────────────────────────────────
//
// The engine never runs a model or a tool itself. When it needs one it emits a
// `provider_request` / `tool_request` frame carrying a `request_id`, and parks
// that step until the host POSTs the result back under the same id.
//
// An outstanding reverse request is NOT re-announced on resume: asking for
// `?after=N` asserts you received everything through N, obligations included.
// A client that persisted its seq but not its in-flight request ids must
// replay from the start — `?after=0`, or after a `replay_truncated`, one less
// than `oldest_retained` — to rediscover what it owes; obligations announced
// in frames the ring has already evicted cannot be re-learned this way.
";

#[cfg(test)]
mod tests {
    use super::*;

    /// Every inbound body a host can POST is published, by name.
    ///
    /// Asserted as an exact set rather than a contains-check, because the
    /// failure this guards is an **omission**, and a contains-check cannot see
    /// one. `ScopeReviewResultIn` was missing here while the outbound schema
    /// described its `scope_review_request` counterpart in six places (#1497):
    /// the generated contract told a host to expect the question and gave it no
    /// type to build the answer from. Nothing failed — the exporter ran, the
    /// artifacts committed, and the hole was only visible to someone diffing
    /// the two documents by hand.
    ///
    /// Adding an inbound body means adding it here, which is the point: an
    /// exact set turns "I forgot to export it" into a failing test rather than
    /// a silently incomplete contract.
    #[test]
    fn every_inbound_body_is_published_in_the_contract() {
        let schema = inbound_schema();
        let defs = schema["$defs"]
            .as_object()
            .expect("the inbound document is a $defs container");

        let mut published: Vec<&str> = defs.keys().map(String::as_str).collect();
        published.sort_unstable();

        assert_eq!(
            published,
            [
                "EngineOverrides",
                "ProviderDeltaIn",
                "ProviderResultIn",
                "ScopeReviewResultIn",
                "ToolResultIn",
            ],
            "the published inbound bodies drifted from the ones a host can POST"
        );
    }

    /// The scope-review body is a real schema, not an empty placeholder.
    ///
    /// A `$defs` key alone would satisfy the test above while publishing `{}`,
    /// which is a contract that accepts anything — worse than the omission it
    /// replaced, because it looks answered.
    #[test]
    fn the_scope_review_body_carries_its_fields() {
        let schema = scope_review_result_schema();
        let props = schema["properties"]
            .as_object()
            .expect("ScopeReviewResultIn is an object schema");

        assert!(
            props.contains_key("request_id"),
            "the answer must be keyed by the request_id of the frame it answers: {props:?}"
        );
        assert!(
            props.contains_key("decision"),
            "the answer must carry the human's decision: {props:?}"
        );
    }
}
