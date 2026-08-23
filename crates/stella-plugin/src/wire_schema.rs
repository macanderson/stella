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
//! # No `.d.ts` beside it, and the reason is a measurement rather than a
//! preference
//!
//! `agentevent.schema.json` and `serveframe.schema.json` each ship TypeScript
//! declarations printed by `stella_protocol::schema_export`. This one does not,
//! because that printer refuses it and is right to:
//!
//! ```text
//! unsupported JSON Schema construct at #/oneOf/0: variant carries no `type` const
//! ```
//!
//! The printer models a discriminated union keyed on `type`, which is what
//! `AgentEvent` and `ServerFrame` both use. This socket's envelope is keyed on
//! `point` (`#[serde(tag = "point", content = "body")]`), so the arms carry no
//! `type` const and the printer stops rather than guessing — the loud-failure
//! contract that module was built with. Teaching it a caller-supplied tag key
//! is a small, additive change to a printer two other committed artifacts
//! depend on, and it belongs in its own change with its own diff of those
//! artifacts rather than riding in here. It matters: TypeScript is one of the
//! two languages `doc:pipeline-as-plugins` §5 promises are first-class on this
//! socket, and an author writing one has no declarations to import.
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

use serde_json::Value;

use crate::wire::{WrapperRequest, WrapperResponse};

/// The committed schema's filename.
pub const WRAPPER_SCHEMA: &str = "wrapper.schema.json";

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
#[must_use]
pub fn wrapper_schema() -> Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "StellaWrapperSocket",
        "description":
            "The two messages a wrapper plugin exchanges with its host at a \
             point. WrapperRequest is what the host writes to the plugin's \
             stdin; WrapperResponse is what the plugin writes back to stdout to \
             end the point. Both are adjacently framed as {\"point\": …, \
             \"body\": {…}} and both refuse a key beside those two. This \
             document is a definitions container, not a union: the two are not \
             interchangeable, and `point` does not discriminate between them. \
             The host-call and driver channels cross the same pipes and are \
             published in wrapper.wire.json instead — see \
             crates/stella-plugin/src/wire_schema.rs for why.",
        "$defs": {
            "WrapperRequest": wrapper_request_schema(),
            "WrapperResponse": wrapper_response_schema(),
        },
    })
}

/// Every committed artifact this module owns, as `(filename, contents)`.
#[must_use]
pub fn artifacts() -> Vec<(&'static str, String)> {
    let mut json = serde_json::to_string_pretty(&wrapper_schema())
        .expect("a JSON Schema is always serializable");
    json.push('\n');
    vec![(WRAPPER_SCHEMA, json)]
}

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
        let document = wrapper_schema();
        assert!(document.get("oneOf").is_none() && document.get("anyOf").is_none());
        let defs = document
            .get("$defs")
            .and_then(Value::as_object)
            .expect("a container");
        assert_eq!(defs.len(), 2);
        assert!(defs.contains_key("WrapperRequest") && defs.contains_key("WrapperResponse"));
    }
}
