// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Hand-written `JsonSchema` impls for two driver-channel types.
//! A plain `#[derive]` would get both wrong.
//!
//! [`DriveResponse`] writes its own [`Serialize`](serde::Serialize) by hand.
//! Its struct holds only `next`. Its wire shape is
//! `{"point": "drive", "body": {"next": …}}`. A derive would read the struct
//! and publish the wrong shape, because it never sees the hand-written impl.
//!
//! [`DriverCallResponse`] derives `Serialize` over a flattened field whose
//! type is a two-case enum. `schemars` merges that badly. It puts the
//! enum's `oneOf` next to the struct's `properties` instead of inside it.
//! The printer in `stella_protocol::schema_export` then reads that `oneOf`
//! as the whole type and never looks at `properties`. The derived schema
//! would drop `result` and print `{ ok: DriverOk } | { err: HostCallFailure }`.
//! This file writes the correct schema by hand instead, as two full arms.
//!
//! Both impls compile only with the `schema` feature, like every other
//! `schemars` derive in this crate.

use super::{DriveNext, DrivePoint, DriveResponse, DriverCallResponse, DriverOk};
use crate::host_call::HostCallFailure;

impl schemars::JsonSchema for DriveResponse {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "DriveResponse".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let point = generator.subschema_for::<DrivePoint>();
        let next = generator.subschema_for::<DriveNext>();
        schemars::json_schema!({
            "type": "object",
            "description": "The response that ends a driver session: which point is open — \
                           always \"drive\" — and what the driver wants to happen next.",
            "properties": {
                "point": point,
                "body": {
                    "type": "object",
                    "properties": { "next": next },
                    "required": ["next"],
                    "additionalProperties": false
                }
            },
            "required": ["point", "body"],
            "additionalProperties": false
        })
    }
}

impl schemars::JsonSchema for DriverCallResponse {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "DriverCallResponse".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let result = generator.subschema_for::<u32>();
        let ok = generator.subschema_for::<DriverOk>();
        let err = generator.subschema_for::<HostCallFailure>();
        schemars::json_schema!({
            "description": "The host's answer to one capability ask: `result`, the id the ask \
                           carried, plus exactly one of `ok` (what the host did) or `err` \
                           (why it would not or could not).",
            "oneOf": [
                {
                    "type": "object",
                    "properties": { "result": result.clone(), "ok": ok },
                    "required": ["result", "ok"],
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "properties": { "result": result, "err": err },
                    "required": ["result", "err"],
                    "additionalProperties": false
                }
            ]
        })
    }
}
