// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A machine-checked description of the **serve transport** wire format.
//!
//! `stella-protocol`'s `schema_export` describes
//! [`AgentEvent`](stella_protocol::AgentEvent) — the payload.
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
//! # Why the definitions are hoisted before anything is printed
//!
//! `schemars` numbers every reference from the root of the document it is
//! generating, and it re-emits a shared payload type into *each* root that
//! reaches it. Five roots printed one after another therefore produced a
//! `.d.ts` declaring `ToolCall`, `CompletionUsage`, `FinishReason` and seven
//! more twice each — `TS2300: Duplicate identifier`, so the artifact did not
//! compile for the TypeScript consumer it exists for (#4583). The same
//! numbering left [`inbound_schema`] dangling: four derived schemas nested
//! whole under `#/$defs/…` kept fifteen `#/$defs/…` references pointing at
//! definitions the composite root did not have.
//!
//! Both are the one defect, and one `hoist` is the one fix: every root's `$defs`
//! is lifted into a single map, so each payload type is defined once and every
//! reference resolves against the document that carries it. A name defined two
//! different ways is a [`ServeSchemaError::Conflict`] rather than a silent
//! overwrite — publishing one Rust type's shape under another's name is a
//! failure no consumer could detect. This is the pattern `stella-plugin`'s
//! `wrapper_schema` uses (#4535); the printer is shared, so the flattening
//! discipline is too.
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

use serde_json::{Map, Value};
use stella_protocol::schema_export::{
    Discriminant, UnsupportedSchema, typescript_declarations_with_header,
};

use crate::engine_overrides::EngineOverrides;
use crate::frame::{ProviderDeltaIn, ProviderResultIn, ServerFrame, ToolResultIn};

/// Why the transport's wire contract could not be published.
///
/// Every arm is a defect in this crate rather than in a caller's input — the
/// exporter reads nothing but this workspace's own types — so each one stops
/// the export rather than degrading it. A `.d.ts` that lies is worse than an
/// exporter that refused to write one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServeSchemaError {
    /// `schemars` produced something other than an object for a named root.
    #[error("the derived schema for `{0}` is not a JSON object")]
    NotAnObject(&'static str),
    /// Two roots define the same `$defs` name with different bodies, so
    /// hoisting them into one map would have to discard one.
    #[error(
        "two roots define `{0}` with different bodies — one of them would be lost when the \
         documents are flattened into one `$defs`"
    )]
    Conflict(String),
    /// The shared TypeScript printer met a construct it does not model.
    #[error(transparent)]
    Unsupported(#[from] UnsupportedSchema),
}

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

/// The JSON Schema for the optional `engine` object on `POST /v1/turns` and
/// `POST /v1/sessions/{id}/turns` (#1167).
#[must_use]
pub fn engine_overrides_schema() -> Value {
    schemars::schema_for!(EngineOverrides).to_value()
}

/// One body a host POSTs to the transport, as both published artifacts need it.
struct InboundBody {
    /// The name it is published under in `$defs`, and declared under in the
    /// `.d.ts`.
    name: &'static str,
    /// Its derived schema.
    schema: fn() -> Value,
    /// The banner it rides under in the printed `.d.ts`. Empty for a body that
    /// belongs to the section above it.
    banner: &'static str,
}

/// Every inbound body a host can POST.
///
/// One table rather than two ordered lists. Both artifacts enumerate these
/// bodies, and when the enumeration lived at eight call sites the omission this
/// module's exact-set test guards against — a body added to the transport and
/// published in neither — had two places to hide instead of one.
static INBOUND_BODIES: &[InboundBody] = &[
    InboundBody {
        name: "ToolResultIn",
        schema: tool_result_schema,
        banner: INBOUND_HEADER,
    },
    InboundBody {
        name: "ProviderResultIn",
        schema: provider_result_schema,
        banner: "",
    },
    InboundBody {
        name: "ProviderDeltaIn",
        schema: provider_delta_schema,
        banner: DELTA_HEADER,
    },
    InboundBody {
        name: "EngineOverrides",
        schema: engine_overrides_schema,
        banner: ENGINE_HEADER,
    },
];

/// Every inbound body in one document, as a `$defs` container.
///
/// A container rather than a `oneOf`: these are the bodies of **different
/// endpoints**, not arms of one tagged union, and none carries a
/// discriminant. Publishing them as a union would tell a client it may send
/// any shape to any route, which is false — and the shared TypeScript
/// printer rejected exactly that framing when it was tried, because a `oneOf`
/// with no `type` const is not something it can print an honest discriminated
/// union from. The printer was right; the modeling was wrong.
///
/// Each body's own payload definitions are hoisted into the container's `$defs`
/// beside it, for the reason the module header states: `schemars` writes every
/// reference from *its* document's root, so nesting four derived schemas whole
/// leaves every `#/$defs/…` inside them pointing at nothing.
///
/// # Errors
///
/// [`ServeSchemaError::NotAnObject`] if a derived root is not an object, and
/// [`ServeSchemaError::Conflict`] if two bodies define one name two different
/// ways.
pub fn inbound_schema() -> Result<Value, ServeSchemaError> {
    let mut defs = Map::new();
    let mut bodies = Vec::with_capacity(INBOUND_BODIES.len());
    for body in INBOUND_BODIES {
        bodies.push((body.name, hoist(body.name, (body.schema)(), &mut defs)?));
    }
    // The bodies go in after every payload type, and through the same refusal:
    // a body sharing a payload type's name would otherwise publish one shape
    // under the other's name, whichever way the two happened to be ordered.
    for (name, body) in bodies {
        if defs.contains_key(name) {
            return Err(ServeSchemaError::Conflict(name.to_string()));
        }
        defs.insert(name.to_string(), body);
    }

    Ok(serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "StellaServeInbound",
        "description":
            "Bodies a host POSTs back to answer a reverse request. ToolResultIn \
             answers POST /v1/turns/{id}/tool-result; ProviderResultIn answers \
             POST /v1/turns/{id}/provider-result; ProviderDeltaIn optionally \
             streams fragments of an in-flight provider answer to \
             POST /v1/turns/{id}/provider-delta ahead of its ProviderResultIn. \
             Each is keyed by the request_id carried on the frame it answers. \
             EngineOverrides is not a reverse-request answer at all: it is the \
             optional `engine` object on POST /v1/turns and \
             POST /v1/sessions/{id}/turns, published here because it is wire \
             contract. This document is a definitions container, not a union: \
             the bodies are not interchangeable and carry no discriminant. \
             Every payload type they reference is defined here beside them, \
             once each.",
        "$defs": Value::Object(defs),
    }))
}

/// Move `schema`'s own `$defs` into `defs` and return what is left of it.
///
/// Add-only and conflict-refusing: a name already present keeps its first
/// definition when the two agree byte-for-byte, and stops the export when they
/// do not. Silently overwriting would publish one Rust type's shape under
/// another's name, which no consumer could detect and no test here would see.
///
/// `$schema` goes with it. A nested subschema declaring its own dialect is
/// legal and meaningless — the document that carries it states the dialect
/// once — and leaving it would put two `$schema` keys in one document for a
/// reader to reconcile.
fn hoist(
    name: &'static str,
    schema: Value,
    defs: &mut Map<String, Value>,
) -> Result<Value, ServeSchemaError> {
    let Value::Object(mut root) = schema else {
        return Err(ServeSchemaError::NotAnObject(name));
    };
    root.remove("$schema");
    let Some(Value::Object(nested)) = root.remove("$defs") else {
        return Ok(Value::Object(root));
    };
    for (defined, body) in nested {
        match defs.get(&defined) {
            Some(existing) if existing != &body => {
                return Err(ServeSchemaError::Conflict(defined));
            }
            Some(_) => {}
            None => {
                defs.insert(defined, body);
            }
        }
    }
    Ok(Value::Object(root))
}

/// Every committed artifact, as `(filename, contents)`.
///
/// # Errors
///
/// [`ServeSchemaError`], as [`inbound_schema`], plus
/// [`ServeSchemaError::Unsupported`] when a generated schema uses a construct
/// the shared TypeScript printer does not model. Loud rather than approximate:
/// a `.d.ts` that lies is worse than no `.d.ts`.
pub fn artifacts() -> Result<Vec<(&'static str, String)>, ServeSchemaError> {
    let outbound = server_frame_schema();

    // One definition set across every root the `.d.ts` prints. Each root
    // carries its own copy of every payload type it reaches, so printing five
    // of them back to back declared ten identifiers twice — `TS2300` (#4583).
    let mut defs = Map::new();
    let mut frame = hoist("ServerFrame", outbound.clone(), &mut defs)?;
    let mut sections = Vec::with_capacity(INBOUND_BODIES.len());
    for body in INBOUND_BODIES {
        sections.push((hoist(body.name, (body.schema)(), &mut defs)?, body.banner));
    }

    // The whole definition set rides with the first document printed, and the
    // four that follow declare only their own root. Nothing is lost by that:
    // TypeScript declarations are order-independent within a file, so an
    // interface referring to one printed above it resolves exactly as it did
    // when every root carried its own copy.
    frame["$defs"] = Value::Object(defs);
    let mut ts =
        typescript_declarations_with_header(&frame, OUTBOUND_HEADER, Discriminant::EVENT_TYPE)?;
    ts.push_str(ENVELOPE_SUFFIX);
    // Still one root at a time, for the reason `inbound_schema` documents: they
    // are the bodies of different endpoints, so each prints as its own
    // interface under its own banner rather than as an arm of a union nothing
    // discriminates.
    for (root, header) in &sections {
        ts.push_str(&typescript_declarations_with_header(
            root,
            header,
            Discriminant::EVENT_TYPE,
        )?);
    }

    let mut json =
        serde_json::to_string_pretty(&outbound).expect("a JSON Schema is always serializable");
    json.push('\n');
    let mut inbound_json = serde_json::to_string_pretty(&inbound_schema()?)
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
    /// one — the generated contract could tell a host to expect a request and
    /// give it no type to build the answer from. Adding an inbound body means
    /// adding it here, which is the point: an exact set turns "I forgot to
    /// export it" into a failing test rather than a silently incomplete
    /// contract.
    ///
    /// The exact half is asserted on [`INBOUND_BODIES`] and the reaches-the-
    /// document half on the generated `$defs`, because since #4583 that map
    /// also carries every payload type the bodies reference. Asserting the
    /// whole map exactly would pin the payload graph of four Rust types, which
    /// changes for reasons that have nothing to do with an endpoint gaining a
    /// body.
    #[test]
    fn every_inbound_body_is_published_in_the_contract() {
        let published: Vec<&str> = INBOUND_BODIES.iter().map(|body| body.name).collect();
        assert_eq!(
            published,
            [
                "ToolResultIn",
                "ProviderResultIn",
                "ProviderDeltaIn",
                "EngineOverrides"
            ],
            "the published inbound bodies drifted from the ones a host can POST"
        );

        let schema = inbound_schema().expect("the container assembles");
        let defs = schema["$defs"]
            .as_object()
            .expect("the inbound document is a $defs container");
        for name in published {
            assert!(defs.contains_key(name), "{name} never reached the document");
        }
    }

    /// **The witness for #4583.** Every `$ref` in the published inbound
    /// document resolves against the published inbound document.
    ///
    /// It did not, and nothing said so: `schemars` numbers references from the
    /// root of the document it generates, so nesting four derived schemas whole
    /// under `#/$defs/…` left fifteen of their own `#/$defs/…` pointing at
    /// definitions the composite root did not have. A host that ran the
    /// document through a validator got fifteen unresolvable references; one
    /// who did not, got a document that looked authoritative.
    #[test]
    fn every_reference_in_the_published_inbound_document_resolves_within_it() {
        let document = inbound_schema().expect("the container assembles");
        let defs = document
            .get("$defs")
            .and_then(Value::as_object)
            .expect("a container");

        fn references(node: &Value, found: &mut Vec<String>) {
            match node {
                Value::Object(map) => {
                    for (key, value) in map {
                        if key == "$ref" {
                            if let Some(reference) = value.as_str() {
                                found.push(reference.to_string());
                            }
                        } else {
                            references(value, found);
                        }
                    }
                }
                Value::Array(items) => items.iter().for_each(|item| references(item, found)),
                _ => {}
            }
        }

        let mut found = Vec::new();
        references(&document, &mut found);
        assert!(
            !found.is_empty(),
            "the document references its payload types"
        );
        for reference in found {
            let name = reference
                .strip_prefix("#/$defs/")
                .unwrap_or_else(|| panic!("{reference} is not a local definition reference"));
            assert!(defs.contains_key(name), "{reference} resolves to nothing");
        }
    }

    /// The definitions are hoisted, not duplicated: a payload type two bodies
    /// carry is declared once, and hoisting does not reshape it.
    #[test]
    fn a_type_two_documents_carry_is_defined_once() {
        let document = inbound_schema().expect("the container assembles");
        let defs = document
            .get("$defs")
            .and_then(Value::as_object)
            .expect("a container");
        // `GenerationParams` rides inside `EngineOverrides` and inside the
        // frame's own `CompletionRequest`, so it is in two derived roots.
        assert!(defs.contains_key("GenerationParams"));
        assert_eq!(
            defs.get("GenerationParams"),
            server_frame_schema()
                .get("$defs")
                .and_then(|defs| defs.get("GenerationParams")),
            "hoisting must not reshape a definition"
        );
    }

    /// **The witness for #4583's stated repro.** One `export` per name. A
    /// `.d.ts` that declares an identifier twice is a `tsc` error (TS2300), so
    /// a duplicate makes the published artifact unusable by the audience it
    /// exists for — and ten of them shipped, because five roots each carried
    /// their own copy of every payload type they reach.
    #[test]
    fn no_identifier_is_exported_twice() {
        let artifacts = artifacts().expect("every artifact prints");
        let (_, declarations) = artifacts
            .iter()
            .find(|(name, _)| *name == "serveframe.d.ts")
            .expect("the .d.ts is committed beside the schemas");
        let mut exported: Vec<&str> = declarations
            .lines()
            .filter_map(|line| line.strip_prefix("export "))
            .filter_map(|rest| {
                rest.strip_prefix("interface ")
                    .or(rest.strip_prefix("type "))
            })
            .map(|rest| rest.split_whitespace().next().unwrap_or(rest))
            .collect();
        let count = exported.len();
        exported.sort_unstable();
        exported.dedup();
        assert_eq!(count, exported.len(), "an identifier is declared twice");
    }

    /// Every root the `.d.ts` prints still names its own type, so dedup did not
    /// silently drop a declaration along with the duplicates.
    #[test]
    fn every_root_and_every_payload_type_is_still_declared() {
        let artifacts = artifacts().expect("every artifact prints");
        let (_, declarations) = artifacts
            .iter()
            .find(|(name, _)| *name == "serveframe.d.ts")
            .expect("the .d.ts is committed beside the schemas");

        let mut expected: Vec<String> = vec!["ServerFrame".to_string()];
        expected.extend(INBOUND_BODIES.iter().map(|body| body.name.to_string()));
        expected.extend(
            server_frame_schema()["$defs"]
                .as_object()
                .expect("the frame defines its payload types")
                .keys()
                .cloned(),
        );
        for body in INBOUND_BODIES {
            if let Some(defs) = (body.schema)()["$defs"].as_object() {
                expected.extend(defs.keys().cloned());
            }
        }

        for name in expected {
            assert!(
                declarations.contains(&format!("export interface {name} "))
                    || declarations.contains(&format!("export type {name} =")),
                "no declaration emitted for {name}"
            );
        }
    }

    /// A name defined two different ways stops the export rather than losing
    /// one of the two. Nothing in this crate produces one today, which is
    /// exactly why the refusal needs a test: the arm is unreachable from the
    /// shipped types and would otherwise be unexercised until the day it fires.
    #[test]
    fn two_definitions_of_one_name_refuse_rather_than_overwrite() {
        let mut defs = Map::new();
        defs.insert(
            "ToolCall".to_string(),
            serde_json::json!({"type": "string"}),
        );
        let clash = serde_json::json!({
            "$defs": { "ToolCall": { "type": "integer" } },
            "type": "object",
            "properties": {},
        });
        let err = hoist("ToolResultIn", clash, &mut defs).unwrap_err();
        assert_eq!(err, ServeSchemaError::Conflict("ToolCall".to_string()));
    }

    /// Running the exporter twice writes the same bytes, which is what lets
    /// `scripts/check-wire-schema.sh` treat any diff as real drift. Hoisting
    /// walks two maps and inserts into a third, and an order-dependent walk
    /// there would make the guard report drift on every other run.
    #[test]
    fn export_is_deterministic() {
        assert_eq!(
            artifacts().expect("every artifact prints"),
            artifacts().expect("every artifact prints")
        );
    }
}
