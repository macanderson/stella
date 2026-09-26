// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! JSON Schema for the two messages a wrapper plugin exchanges with its host.
//!
//! The upgrade [`wire_corpus`](crate::wire_corpus) declared and could not take
//! (#3532). That module publishes a *corpus* — every message serialized twice,
//! fullest and emptiest — because `schemars` derives a schema from a
//! `JsonSchema` impl, an impl only exists where the type is defined, and
//! `crate::wire` was held by another session when the corpus was written. The
//! derives are on those types now, so this publishes the artifact the corpus
//! was standing in for.
//!
//! # What this catches and the corpus does not
//!
//! The three drift shapes `wire_corpus`'s header names as its own blind spots,
//! two of them structurally:
//!
//! - **a widened or narrowed scalar** — `pub round: u32` → `u64` changes
//!   `"format": "uint32"` to `"uint64"` here and not one byte of the corpus;
//! - **a string field gaining or losing a format or pattern constraint** —
//!   likewise invisible to a corpus, which only ever shows one example value.
//!
//! The third — an enum whose case set grows — is caught by both, and by the
//! compiler first: `wire_corpus` enumerates every closed enum with a successor
//! `match`, so a new case is an `E0004` there before it can go unpublished
//! anywhere.
//!
//! # What the corpus catches and this does not, which is why both ship
//!
//! A schema describes shapes; the corpus pins **bytes**. `serde`'s rendering of
//! a value is not recoverable from a schema: a field that starts being emitted
//! as `null` rather than omitted, an enum whose `rename_all` changes the case
//! of a tag *and* whose schema is regenerated to match, a `skip_serializing_if`
//! that stops firing — a reader of the schema alone sees a legal document
//! either way. The corpus shows the two exact strings a plugin's parser will
//! meet. Neither artifact subsumes the other, so `docs/wire/` carries both and
//! `scripts/check-wire-schema.sh` diffs both.
//!
//! # What it does not describe
//!
//! **The envelope's refusal of unknown keys.** `WrapperRequest` and
//! `WrapperResponse` have hand-written `Deserialize` impls over a two-key
//! envelope that denies anything beside `point` and `body` (#3500), and
//! `schemars` derives from the *shape* — the `Serialize` framing — not from a
//! reader. The `deny_unknown_fields` this crate spells on the two enums is what
//! closes that gap, so the emitted `additionalProperties: false` describes the
//! reader that actually runs. Where a body struct carries the attribute the
//! same mechanism applies and always did.
//!
//! **The host-call channel.** [`crate::HostCallRequest`] and its answer cross
//! the same pipes as the wrapper socket. They are published in the corpus.
//! They are not published as a schema here. [`crate::HostCallOk`] is an
//! **untagged** union: its cases are told apart only by their required key
//! sets, a rule JSON Schema states as `oneOf` and does not check. A schema
//! that looks authoritative about a union nothing checks would be worse than
//! none. The corpus shows every arm's real bytes, which is what an
//! implementer of that channel needs.
//!
//! **The driver channel** carries no such union. Every type
//! [`crate::DriveRequest`] and [`crate::DriverCallResponse`] carry is a plain
//! struct or an externally tagged enum. So it is not excluded here for that
//! reason, and it is not excluded at all. [`driver_schema`](crate::wire_schema::driver_schema)
//! and [`driver_artifacts`](crate::wire_schema::driver_artifacts) publish its own
//! `driver.schema.json` and `driver.d.ts` beside these, over the same four
//! rules stated above.
//!
//! # The `.d.ts` beside it, and why the container is flattened first
//!
//! `wrapper.d.ts` is printed by the same `stella_protocol::schema_export` that
//! prints `agentevent.d.ts` and `serveframe.d.ts` — one subset of JSON Schema
//! to keep in step rather than three. It could not be, until #4535: that
//! printer assumed a union tagged on `type`, and this socket's envelope is
//! `#[serde(tag = "point", content = "body")]`, so it refused with
//! `variant carries no "type" const` and the one contract whose audience writes
//! TypeScript was the one contract with no declarations to import
//! (`doc:pipeline-as-plugins` §5 names TypeScript as first-class here).
//!
//! Printing it forced the composite document to be *flattened*, and that fixed
//! a second defect nobody had asked about. `schemars` numbers every reference
//! from the root of the document it is generating, so
//! `schema_for!(WrapperRequest)` emits `"$ref": "#/$defs/BeforeTurnRequest"`.
//! Nesting that whole schema under `#/$defs/WrapperRequest` — which is what a
//! container of two `schema_for!` results is — leaves every one of those
//! references pointing at a `#/$defs/BeforeTurnRequest` the composite root does
//! not have. The document validated nothing and dangled seventeen times.
//! [`wrapper_schema`](crate::wire_schema::wrapper_schema) now hoists both
//! roots' definitions into one `$defs`, so the references resolve and each
//! payload type is declared once rather than twice. A name defined on both
//! sides with two different bodies is a
//! [`WrapperSchemaError::Conflict`](crate::wire_schema::WrapperSchemaError::Conflict)
//! rather than a silent overwrite.
//!
//! # Determinism
//!
//! `serde_json::Map` is a `BTreeMap` here (the workspace does not enable
//! `preserve_order`), so object keys sort, and every array `schemars` emits is
//! in declaration order. Running the exporter twice produces no diff the second
//! time — `scripts/check-wire-schema.sh` depends on exactly that.

// Two `expect`s below, and they are the one place the lint is wrong, for
// `stella_protocol::schema_export`'s reason: both serialize a
// `serde_json::Value`, and the only two ways `serde_json` can fail to
// serialize — a map key that is not a string, a non-finite float — are
// unrepresentable in `Value`. The failure is excluded by the type, so plumbing
// a `Result` no caller could match on would trade a proof for ceremony.
#![expect(
    clippy::expect_used,
    reason = "the two `expect`s below serialize a `serde_json::Value`, whose only serialization failures are unrepresentable in the type; see the paragraph above"
)]

use serde_json::{Map, Value};
use stella_protocol::schema_export::{Discriminant, UnsupportedSchema};

use crate::wire::{WrapperRequest, WrapperResponse};

/// The committed schema's filename.
pub const WRAPPER_SCHEMA: &str = "wrapper.schema.json";

/// The committed TypeScript declarations' filename.
pub const WRAPPER_DTS: &str = "wrapper.d.ts";

/// The driver channel's committed schema filename.
pub const DRIVER_SCHEMA: &str = "driver.schema.json";

/// The driver channel's committed TypeScript declarations filename.
pub const DRIVER_DTS: &str = "driver.d.ts";

/// Why the socket's wire contract could not be published.
///
/// Every arm is a defect in this crate rather than in a caller's input — the
/// exporter reads nothing but this workspace's own types — so each one stops
/// the export rather than degrading it. A `.d.ts` that lies about a socket
/// spoken in four languages is worse than an exporter that refused to write it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WrapperSchemaError {
    /// `schemars` produced something other than an object for a named root.
    #[error("the derived schema for `{0}` is not a JSON object")]
    NotAnObject(&'static str),
    /// The two directions define the same `$defs` name with different bodies,
    /// so hoisting them into one map would have to discard one.
    #[error(
        "the request and the response both define `{0}`, with different bodies — one of them \
         would be lost when the two documents are flattened into one `$defs`"
    )]
    Conflict(String),
    /// The shared TypeScript printer met a construct it does not model.
    #[error(transparent)]
    Unsupported(#[from] UnsupportedSchema),
}

/// How the socket's envelopes are tagged, and what the union of those tags is
/// called in the published declarations.
///
/// Named `WrapperPoint` after the Rust enum it is derived from
/// ([`crate::WrapperPoint`]), which is the name a plugin author reading
/// `doc:wrapper-socket` alongside these declarations will already have met.
const SOCKET_POINT: Discriminant = Discriminant {
    key: "point",
    vocabulary: "WrapperPoint",
    doc: "Every point this build's socket speaks. Both directions are tagged with it\n\
          and both carry the same two values: the tag says which point a message\n\
          belongs to, never whether it is a request or a response.",
};

/// The JSON Schema (2020-12) for one request on the socket.
#[must_use]
pub fn wrapper_request_schema() -> Value {
    schemars::schema_for!(WrapperRequest).to_value()
}

/// The JSON Schema (2020-12) for one response on the socket.
#[must_use]
pub fn wrapper_response_schema() -> Value {
    schemars::schema_for!(WrapperResponse).to_value()
}

/// Both directions in one document, as a `$defs` container.
///
/// A container rather than a `oneOf`, for `stella-serve`'s `inbound_schema`
/// reason: a request and a response are not interchangeable, and while both do
/// carry a `point` discriminant, the discriminant does not tell them apart —
/// `{"point": "before_turn"}` is a legal tag on either. A union would tell a
/// plugin author that anything matching either arm is a legal thing to write,
/// which is false in the one direction that matters: a plugin writes responses.
///
/// Both directions' payload definitions are hoisted into the container's own
/// `$defs`, for the reason the module header states: `schemars` writes every
/// reference from *its* document's root, so nesting two derived schemas whole
/// leaves every `#/$defs/…` inside them pointing at nothing.
///
/// # Errors
///
/// [`WrapperSchemaError::NotAnObject`] if a derived root is not an object, and
/// [`WrapperSchemaError::Conflict`] if the two directions define one name two
/// different ways.
pub fn wrapper_schema() -> Result<Value, WrapperSchemaError> {
    let mut defs = Map::new();
    let request = hoist("WrapperRequest", wrapper_request_schema(), &mut defs)?;
    let response = hoist("WrapperResponse", wrapper_response_schema(), &mut defs)?;
    defs.insert("WrapperRequest".to_string(), request);
    defs.insert("WrapperResponse".to_string(), response);

    Ok(serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "StellaWrapperSocket",
        "description":
            "The two messages a wrapper plugin exchanges with its host at a \
             point, and every payload they carry. WrapperRequest is what the \
             host writes to the plugin's stdin; WrapperResponse is what the \
             plugin writes back to stdout to end the point. Both are adjacently \
             framed as {\"point\": …, \"body\": {…}} and both refuse a key \
             beside those two. This document is a definitions container, not a \
             union: the two are not interchangeable, and `point` does not \
             discriminate between them. The host-call channel crosses the same \
             pipes and is published in wrapper.wire.json instead, because one \
             of its answers is an untagged union — see \
             crates/stella-plugin/src/wire_schema.rs for why. The driver \
             channel crosses those pipes too and carries no such union, so it \
             publishes its own driver.schema.json beside this one.",
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
/// legal and meaningless — the container states the dialect once — and leaving
/// it would put two `$schema` keys in one document for a reader to reconcile.
fn hoist(
    name: &'static str,
    schema: Value,
    defs: &mut Map<String, Value>,
) -> Result<Value, WrapperSchemaError> {
    let Value::Object(mut root) = schema else {
        return Err(WrapperSchemaError::NotAnObject(name));
    };
    root.remove("$schema");
    let Some(Value::Object(nested)) = root.remove("$defs") else {
        return Ok(Value::Object(root));
    };
    for (defined, body) in nested {
        match defs.get(&defined) {
            Some(existing) if existing != &body => {
                return Err(WrapperSchemaError::Conflict(defined));
            }
            Some(_) => {}
            None => {
                defs.insert(defined, body);
            }
        }
    }
    Ok(Value::Object(root))
}

/// Every committed artifact this module owns, as `(filename, contents)`.
///
/// # Errors
///
/// [`WrapperSchemaError`], as [`wrapper_schema`], plus
/// [`WrapperSchemaError::Unsupported`] when the shared TypeScript printer meets
/// a construct it does not model.
pub fn artifacts() -> Result<Vec<(&'static str, String)>, WrapperSchemaError> {
    let schema = wrapper_schema()?;
    let declarations = stella_protocol::schema_export::typescript_declarations_with_header(
        &schema,
        DTS_HEADER,
        SOCKET_POINT,
    )?;
    let mut json =
        serde_json::to_string_pretty(&schema).expect("a JSON Schema is always serializable");
    json.push('\n');
    Ok(vec![(WRAPPER_SCHEMA, json), (WRAPPER_DTS, declarations)])
}

const DTS_HEADER: &str = "\
// GENERATED FILE — DO NOT EDIT.
//
// Regenerate with:  bash scripts/export-agentevent-schema.sh
// Source of truth:  stella-plugin/src/wire.rs
// Guarded by:       scripts/check-wire-schema.sh (`make wire-schema`)
//
// The wrapper socket: the two messages an out-of-process plugin exchanges with
// its host at a point. The host writes a WrapperRequest to the plugin's stdin,
// one JSON object per line; the plugin writes a WrapperResponse back to stdout
// to end the point.
//
// A request and a response are NOT alternatives. `point` is a legal tag on
// both, and says which point a message belongs to — never which direction it
// travels. Which direction you are reading is decided by which pipe it came
// off, and a plugin only ever writes responses.
//
// The host-call channel crosses the same pipes and is not described here: one
// of its answers is an untagged union whose contract is that its arms are
// discriminable by their required keys, which JSON Schema can state and
// cannot check. docs/wire/wrapper.wire.json shows every one of its messages
// as the exact bytes a parser meets. The driver channel crosses those pipes
// too and carries no such union — see driver.schema.json and driver.d.ts.
//
// Every message carries protocol_version, and the contract is additive-only.
";

/// Every driver-channel root type, hoisted into one `$defs` container. This
/// follows the [`wrapper_schema`] pattern, over four shapes instead of two.
/// [`crate::DriveRequest`] opens a session. [`crate::DriverCallRequest`] is a
/// capability ask mid-session. [`crate::DriverCallResponse`] is the host's
/// answer to one. [`crate::DriveResponse`] ends the session. This is a
/// container, not a union, on [`wrapper_schema`]'s own terms: the four are
/// not interchangeable, and nothing on the wire tells them apart by shape
/// alone. A reader knows which one arrived from which pipe carried it, and
/// where in the exchange it stands.
///
/// # Errors
///
/// As [`wrapper_schema`].
pub fn driver_schema() -> Result<Value, WrapperSchemaError> {
    let mut defs = Map::new();
    for (name, root) in [
        (
            "DriveRequest",
            schemars::schema_for!(crate::DriveRequest).to_value(),
        ),
        (
            "DriverCallRequest",
            schemars::schema_for!(crate::DriverCallRequest).to_value(),
        ),
        (
            "DriverCallResponse",
            schemars::schema_for!(crate::DriverCallResponse).to_value(),
        ),
        (
            "DriveResponse",
            schemars::schema_for!(crate::DriveResponse).to_value(),
        ),
    ] {
        let hoisted = hoist(name, root, &mut defs)?;
        defs.insert(name.to_string(), hoisted);
    }

    Ok(serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "StellaDriverChannel",
        "description":
            "The four messages the driver channel carries, and every payload they carry. \
             DriveRequest opens a session; DriverCallRequest is a capability ask a driver \
             makes mid-session; DriverCallResponse is the host's answer to one; DriveResponse \
             ends the session, naming what should happen next. This document is a definitions \
             container, not a union: the four are not interchangeable. The host-call channel \
             crosses the same pipes and stays corpus-only in wrapper.wire.json, because one of \
             its answers is an untagged union — see crates/stella-plugin/src/wire_schema.rs.",
        "$defs": Value::Object(defs),
    }))
}

/// Every committed driver-channel artifact, as `(filename, contents)`.
///
/// # Errors
///
/// [`WrapperSchemaError`], as [`driver_schema`], plus
/// [`WrapperSchemaError::Unsupported`] when the shared TypeScript printer meets
/// a construct it does not model.
pub fn driver_artifacts() -> Result<Vec<(&'static str, String)>, WrapperSchemaError> {
    let schema = driver_schema()?;
    let declarations = stella_protocol::schema_export::typescript_declarations_with_header(
        &schema,
        DRIVER_DTS_HEADER,
        DRIVER_POINT,
    )?;
    let mut json =
        serde_json::to_string_pretty(&schema).expect("a JSON Schema is always serializable");
    json.push('\n');
    Ok(vec![(DRIVER_SCHEMA, json), (DRIVER_DTS, declarations)])
}

/// How the driver channel's session messages are tagged.
///
/// None of the four published roots is itself a `oneOf` tagged on `point`.
/// Only [`crate::DrivePoint`] carries the key, and it has one legal value.
/// So this adds no tag vocabulary to the printed declarations — the rule in
/// `stella_protocol::schema_export::typescript_declarations_with_header` for
/// a container, where an untagged `$defs` entry "contributes nothing and is
/// not an error". Named for the same reason as [`SOCKET_POINT`]: it is the
/// vocabulary a plugin author already meets in `doc:backlog-self-driving`.
const DRIVER_POINT: Discriminant = Discriminant {
    key: "point",
    vocabulary: "DrivePoint",
    doc: "The one point a driver session dispatches.",
};

const DRIVER_DTS_HEADER: &str = "\
// GENERATED FILE — DO NOT EDIT.
//
// Regenerate with:  bash scripts/export-agentevent-schema.sh
// Source of truth:  stella-plugin/src/driver.rs
// Guarded by:       scripts/check-wire-schema.sh (`make wire-schema`)
//
// The driver channel: the session a plugin opens when it drives turns,
// instead of sitting inside one. The host opens the session with a
// DriveRequest. The driver asks for a capability with a DriverCallRequest,
// and the host answers with a DriverCallResponse. The driver ends the
// session with a DriveResponse naming what should happen next.
//
// The host-call channel crosses the same pipes and is not described here.
// One of its answers is an untagged union. Its arms are told apart only by
// their required keys, a rule JSON Schema can state and cannot check.
// docs/wire/wrapper.wire.json shows every one of its messages as the exact
// bytes a parser meets.
//
// Every capability ask names a call. The host's grant either permits or
// refuses it. A refusal is a value the driver branches on, never a death.
";

#[cfg(test)]
mod tests {
    use super::*;

    /// Reach a definition inside a root's own `$defs`.
    fn def<'a>(root: &'a Value, name: &str) -> &'a Value {
        root.get("$defs")
            .and_then(|defs| defs.get(name))
            .unwrap_or_else(|| panic!("the schema defines {name}"))
    }

    /// **The witness.** The schema states a scalar's width, which is the first
    /// of the two drift shapes the corpus is structurally blind to: `round: u32`
    /// and `round: u64` serialize to the same `0` and produce no corpus diff at
    /// all.
    ///
    /// Asserted on the property rather than on a golden string, so widening the
    /// field fails *here* with a sentence, and separately shows up as a
    /// `wrapper.schema.json` diff at the gate.
    #[test]
    fn the_schema_states_a_scalar_width_the_corpus_cannot_show() {
        let request = wrapper_request_schema();
        let before = def(&request, "BeforeTurnRequest");
        let round = before
            .get("properties")
            .and_then(|properties| properties.get("round"))
            .expect("BeforeTurnRequest declares `round`");
        assert_eq!(
            round.get("format").and_then(Value::as_str),
            Some("uint32"),
            "a widened round is a wire change; the corpus would not have shown it"
        );
        assert_eq!(round.get("type").and_then(Value::as_str), Some("integer"));
    }

    /// The other half: what the derived schema says about the envelope has to
    /// match the hand-written reader beside it.
    ///
    /// `WrapperRequest`/`WrapperResponse` decode through a two-key envelope that
    /// refuses anything beside `point` and `body` (#3500), and `schemars`
    /// derives from the `Serialize` framing, which says nothing about a reader.
    /// Without the `schemars(deny_unknown_fields)` this pins, the published
    /// contract would tell a plugin author the socket accepts keys it refuses —
    /// a schema that over-permits is how a plugin ships something the host
    /// rejects in the field.
    #[test]
    fn every_envelope_arm_refuses_a_key_beside_point_and_body() {
        for root in [wrapper_request_schema(), wrapper_response_schema()] {
            let arms = root
                .get("oneOf")
                .and_then(Value::as_array)
                .expect("the envelope is a union of its points");
            assert_eq!(arms.len(), 2, "before_turn and after_turn");
            for arm in arms {
                assert_eq!(
                    arm.get("additionalProperties"),
                    Some(&Value::Bool(false)),
                    "the reader refuses an unknown envelope key and the schema must say so"
                );
                let required: Vec<&str> = arm
                    .get("required")
                    .and_then(Value::as_array)
                    .expect("both keys are required")
                    .iter()
                    .filter_map(Value::as_str)
                    .collect();
                assert_eq!(required, vec!["point", "body"]);
            }
        }
    }

    /// The container is a container. A plugin author reading it must not be
    /// told that a request and a response are alternatives: `point` is a legal
    /// tag on both, so a union here would be a claim nothing validates.
    #[test]
    fn the_document_is_a_definitions_container_and_not_a_union() {
        let document = wrapper_schema().expect("the container assembles");
        assert!(document.get("oneOf").is_none() && document.get("anyOf").is_none());
        let defs = document
            .get("$defs")
            .and_then(Value::as_object)
            .expect("a container");
        assert!(defs.contains_key("WrapperRequest") && defs.contains_key("WrapperResponse"));
        for direction in ["WrapperRequest", "WrapperResponse"] {
            assert!(
                def(&document, direction).get("oneOf").is_some(),
                "{direction} is the union of the points it is spoken at"
            );
        }
    }

    /// **The witness for #4535's other half.** Every `$ref` in the published
    /// document resolves against the published document.
    ///
    /// It did not, and nothing said so: `schemars` numbers references from the
    /// root of the document it generates, so nesting two derived schemas whole
    /// under `#/$defs/Wrapper{Request,Response}` left every one of their
    /// `#/$defs/…` pointing at a definition the composite root did not have.
    /// A plugin author who ran the document through a validator got seventeen
    /// unresolvable references; a reader who did not, got a document that looked
    /// authoritative.
    #[test]
    fn every_reference_in_the_published_document_resolves_within_it() {
        let document = wrapper_schema().expect("the container assembles");
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

    /// The definitions are hoisted, not duplicated: a payload type both
    /// directions carry is declared once, which is also what keeps the printed
    /// `.d.ts` free of a duplicate `export interface`.
    #[test]
    fn a_type_both_directions_carry_is_defined_once() {
        let document = wrapper_schema().expect("the container assembles");
        let defs = document
            .get("$defs")
            .and_then(Value::as_object)
            .expect("a container");
        // `PublishedSignal` rides on a request (what the host decided) and on a
        // response (what the plugin published), so it is in both derived roots.
        assert!(defs.contains_key("PublishedSignal"));
        assert_eq!(
            def(&document, "PublishedSignal"),
            def(&wrapper_request_schema(), "PublishedSignal"),
            "hoisting must not reshape a definition"
        );
    }

    /// A name defined two different ways stops the export rather than losing
    /// one of the two. Nothing in this crate produces one today, which is
    /// exactly why the refusal needs a test: the arm is unreachable from the
    /// shipped types and would otherwise be unexercised until the day it fires.
    #[test]
    fn two_definitions_of_one_name_refuse_rather_than_overwrite() {
        let mut defs = Map::new();
        defs.insert("Signal".to_string(), serde_json::json!({"type": "string"}));
        let clash = serde_json::json!({
            "$defs": { "Signal": { "type": "integer" } },
            "oneOf": [],
        });
        let err = hoist("WrapperResponse", clash, &mut defs).unwrap_err();
        assert_eq!(err, WrapperSchemaError::Conflict("Signal".to_string()));
    }

    /// The declarations print, name every payload type, and carry the socket's
    /// own tag vocabulary rather than `AgentEvent`'s.
    #[test]
    fn the_declarations_name_every_payload_type_and_the_points() {
        let document = wrapper_schema().expect("the container assembles");
        let artifacts = artifacts().expect("both artifacts print");
        let (name, declarations) = artifacts
            .iter()
            .find(|(name, _)| *name == WRAPPER_DTS)
            .expect("the .d.ts is committed beside the schema");
        assert_eq!(*name, "wrapper.d.ts");

        for defined in document["$defs"].as_object().expect("a container").keys() {
            assert!(
                declarations.contains(&format!("export interface {defined} "))
                    || declarations.contains(&format!("export type {defined} =")),
                "no declaration emitted for {defined}"
            );
        }
        assert!(
            declarations
                .contains("export type WrapperPoint =\n  | \"before_turn\"\n  | \"after_turn\";"),
            "{declarations}"
        );
        // A container is not a type, and the vocabulary is this socket's.
        assert!(!declarations.contains("export type StellaWrapperSocket"));
        assert!(!declarations.contains("KnownTypeTag"));
    }

    /// One `export` per name. A `.d.ts` that declares an identifier twice is a
    /// `tsc` error (TS2300), so a duplicate would make the published artifact
    /// unusable by the audience it exists for.
    #[test]
    fn no_identifier_is_exported_twice() {
        let artifacts = artifacts().expect("both artifacts print");
        let (_, declarations) = artifacts
            .iter()
            .find(|(name, _)| *name == WRAPPER_DTS)
            .expect("the .d.ts is committed beside the schema");
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

    /// Running the exporter twice writes the same bytes, which is what lets
    /// `scripts/check-wire-schema.sh` treat any diff as real drift.
    #[test]
    fn export_is_deterministic() {
        assert_eq!(
            artifacts().expect("both artifacts print"),
            artifacts().expect("both artifacts print")
        );
    }

    /// The driver channel publishes a schema and TypeScript declarations,
    /// the way the wrapper socket does — on the header's old reason, which
    /// stopped being true of this channel: nothing in `stella_plugin::driver`
    /// is an untagged union.
    #[test]
    fn the_driver_channel_publishes_a_schema_and_declarations() {
        let artifacts = driver_artifacts().expect("the driver channel's artifacts print");
        let names: Vec<&str> = artifacts.iter().map(|(name, _)| *name).collect();
        assert_eq!(names, vec![DRIVER_SCHEMA, DRIVER_DTS]);
    }

    /// Every `$ref` in the driver document resolves against the driver
    /// document, on [`every_reference_in_the_published_document_resolves_within_it`]'s
    /// terms.
    #[test]
    fn every_reference_in_the_driver_document_resolves_within_it() {
        let document = driver_schema().expect("the driver container assembles");
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
            "the driver document references its payload types"
        );
        for reference in found {
            let name = reference
                .strip_prefix("#/$defs/")
                .unwrap_or_else(|| panic!("{reference} is not a local definition reference"));
            assert!(defs.contains_key(name), "{reference} resolves to nothing");
        }
    }

    /// `DriverCallResponse` is hand-written, not derived (this module's
    /// header explains why): a two-arm union, `result` and `ok` required
    /// together or `result` and `err` required together, and no third key
    /// legal in either arm.
    #[test]
    fn driver_call_response_is_a_closed_union_of_ok_or_err() {
        let document = driver_schema().expect("the driver container assembles");
        let response = def(&document, "DriverCallResponse");
        let arms = response
            .get("oneOf")
            .and_then(Value::as_array)
            .expect("a union of the two answers a capability ask gets");
        assert_eq!(arms.len(), 2);
        let mut required_sets: Vec<Vec<&str>> = arms
            .iter()
            .map(|arm| {
                assert_eq!(
                    arm.get("additionalProperties"),
                    Some(&Value::Bool(false)),
                    "each arm refuses a key beside its own"
                );
                arm.get("required")
                    .and_then(Value::as_array)
                    .expect("each arm requires its own keys")
                    .iter()
                    .filter_map(Value::as_str)
                    .collect()
            })
            .collect();
        required_sets.sort_unstable();
        assert_eq!(
            required_sets,
            vec![vec!["result", "err"], vec!["result", "ok"]]
        );
    }

    /// `DriveResponse` is hand-written too: the envelope `Serialize` actually
    /// writes, `{"point": "drive", "body": {"next": …}}`, not this struct's
    /// own field (`next` alone).
    #[test]
    fn drive_response_requires_the_envelope_a_driver_actually_writes() {
        let document = driver_schema().expect("the driver container assembles");
        let response = def(&document, "DriveResponse");
        assert_eq!(
            response.get("required").and_then(Value::as_array),
            Some(&vec![
                Value::String("point".to_string()),
                Value::String("body".to_string())
            ])
        );
        let body = response
            .pointer("/properties/body")
            .expect("DriveResponse carries a body");
        assert_eq!(
            body.get("required").and_then(Value::as_array),
            Some(&vec![Value::String("next".to_string())])
        );
        assert_eq!(body.get("additionalProperties"), Some(&Value::Bool(false)));
    }

    /// The declarations print, and name every payload type the schema defines.
    #[test]
    fn the_driver_declarations_name_every_payload_type() {
        let document = driver_schema().expect("the driver container assembles");
        let artifacts = driver_artifacts().expect("both driver artifacts print");
        let (name, declarations) = artifacts
            .iter()
            .find(|(name, _)| *name == DRIVER_DTS)
            .expect("the .d.ts is committed beside the schema");
        assert_eq!(*name, "driver.d.ts");

        for defined in document["$defs"].as_object().expect("a container").keys() {
            assert!(
                declarations.contains(&format!("export interface {defined} "))
                    || declarations.contains(&format!("export type {defined} =")),
                "no declaration emitted for {defined}"
            );
        }
        // A container is not a type of its own.
        assert!(!declarations.contains("export type StellaDriverChannel"));
    }

    /// One `export` per name, on [`no_identifier_is_exported_twice`]'s terms.
    #[test]
    fn no_identifier_is_exported_twice_in_the_driver_declarations() {
        let artifacts = driver_artifacts().expect("both driver artifacts print");
        let (_, declarations) = artifacts
            .iter()
            .find(|(name, _)| *name == DRIVER_DTS)
            .expect("the .d.ts is committed beside the schema");
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

    /// Running the driver exporter twice writes the same bytes, on
    /// [`export_is_deterministic`]'s terms.
    #[test]
    fn driver_export_is_deterministic() {
        assert_eq!(
            driver_artifacts().expect("both driver artifacts print"),
            driver_artifacts().expect("both driver artifacts print")
        );
    }

    /// `docs/wire/` is read by a consumer with no workspace in front of them,
    /// so nothing published names a Rust path — `scripts/check-wire-schema.sh`
    /// enforces this over the committed files; this is the same check over the
    /// driver channel's artifacts in the one-crate test run.
    #[test]
    fn the_driver_contract_names_no_rust_path() {
        let document = driver_schema().expect("the driver container assembles");
        let json = serde_json::to_string(&document).expect("the document serializes");
        let artifacts = driver_artifacts().expect("both driver artifacts print");
        let (_, declarations) = artifacts
            .iter()
            .find(|(name, _)| *name == DRIVER_DTS)
            .expect("the .d.ts is committed beside the schema");
        for leaked in ["crate::", "super::"] {
            assert!(!json.contains(leaked), "driver.schema.json names {leaked}");
            assert!(!declarations.contains(leaked), "driver.d.ts names {leaked}");
        }
    }
}
