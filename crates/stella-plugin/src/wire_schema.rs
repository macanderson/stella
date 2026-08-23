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
//! The third — an enum whose variant set grows — is caught by both, and by the
//! compiler first: `wire_corpus` enumerates every closed enum with a successor
//! `match`, so a new variant is an `E0004` there before it can go unpublished
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
//! # What it deliberately does not describe
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
//! **The host-call and driver channels.** [`crate::HostCallRequest`],
//! [`crate::DriveRequest`] and their answers cross the same pipes and are
//! published in the corpus; they are not published here, because
//! [`crate::HostCallOk`] is an **untagged** union whose whole contract is that
//! its variants are discriminable by their required key sets — a property
//! JSON Schema states as `oneOf` and does not check. Publishing a schema that
//! looks authoritative about a union nothing validates would be worse than
//! publishing none. The corpus shows every arm's real bytes, which is what an
//! implementer of that channel needs.
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
#![allow(clippy::expect_used)]

use serde_json::{Map, Value};
use stella_protocol::schema_export::{Discriminant, UnsupportedSchema};

use crate::wire::{WrapperRequest, WrapperResponse};

/// The committed schema's filename.
pub const WRAPPER_SCHEMA: &str = "wrapper.schema.json";

/// The committed TypeScript declarations' filename.
pub const WRAPPER_DTS: &str = "wrapper.d.ts";

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
             discriminate between them. The host-call and driver channels cross \
             the same pipes and are published in wrapper.wire.json instead — \
             see crates/stella-plugin/src/wire_schema.rs for why.",
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
// The host-call and driver channels cross the same pipes and are not described
// here: HostCallOk is an untagged union whose contract is that its arms are
// discriminable by their required keys, which JSON Schema can state and cannot
// check. docs/wire/wrapper.wire.json shows every one of their messages as the
// exact bytes a parser meets.
//
// Every message carries protocol_version, and the contract is additive-only.
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
}
