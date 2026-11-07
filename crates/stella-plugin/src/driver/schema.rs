// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Hand-written `JsonSchema` impls for three driver-channel types.
//! A plain `#[derive]` would get each one wrong. Each impl says why.
//!
//! The three share one rule. Their readers take an explicit `null` as a
//! missing key. So each schema lets a key the message does not use be `null`.
//!
//! All three compile only with the `schema` feature, like every other
//! `schemars` derive in this crate.

use serde_json::{Map, Value, json};

use super::{
    DriveNext, DrivePoint, DriveResponse, DriverArgs, DriverCall, DriverCallRequest,
    DriverCallResponse, DriverOk,
};
use crate::host_call::HostCallFailure;

/// The schema for a key this message does not use. The reader takes `null`
/// there as no key at all, so `null` is the one value it allows.
fn null_only() -> Value {
    json!({ "type": "null" })
}

/// [`DriveResponse`] writes its own `Serialize` by hand. Its struct holds
/// only `next`. Its wire shape is `{"point": "drive", "body": {"next": …}}`.
/// A derive reads the struct and never sees the hand-written impl, so it
/// would publish the wrong shape.
impl schemars::JsonSchema for DriveResponse {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "DriveResponse".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let point = generator.subschema_for::<DrivePoint>();
        let next = generator.subschema_for::<DriveNext>();
        schemars::json_schema!({
            "type": "object",
            "description": "The response that ends a driver session. `point` is always \
                           \"drive\", and `body.next` says what the driver wants to happen \
                           next. `call`, `id` and `args` belong to a capability ask, so a \
                           response carries them only as null.",
            "properties": {
                "point": point,
                "body": {
                    "type": "object",
                    "properties": { "next": next },
                    "required": ["next"],
                    "additionalProperties": false
                },
                "call": null_only(),
                "id": null_only(),
                "args": null_only()
            },
            "required": ["point", "body"],
            "additionalProperties": false
        })
    }
}

/// [`DriverCallResponse`] derives `Serialize` over a flattened field whose
/// type is a two-case enum. `schemars` merges that badly. It puts the enum's
/// `oneOf` next to the struct's `properties` instead of inside it. The
/// printer in `stella_protocol::schema_export` then reads that `oneOf` as the
/// whole type and drops `result`. So this writes the two arms in full.
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
                           (why it would not or could not). The other key of the two may \
                           appear only as null.",
            "oneOf": [
                {
                    "type": "object",
                    "properties": { "result": result.clone(), "ok": ok, "err": null_only() },
                    "required": ["result", "ok"],
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "properties": { "result": result, "err": err, "ok": null_only() },
                    "required": ["result", "err"],
                    "additionalProperties": false
                }
            ]
        })
    }
}

/// [`DriverCallRequest`] names a call in `call` and that call's own table in
/// `args`. A derive treats the two keys apart. Then an ask for `work_status`
/// that carries a `work_start` table reads as valid, and the host refuses it.
///
/// So each call that reads a table gets an arm of its own. That arm needs
/// `args`, and `args` sets that one member. One last arm covers every call
/// that reads none. Its `args` may be missing, `null`, or a table that sets
/// no member.
///
/// The arms come from the schema of [`DriverArgs`]. Each of its members is
/// named for the call that reads it, as [`DriverArgs::tables`] says. So a new
/// member gets its arm here with no edit.
impl schemars::JsonSchema for DriverCallRequest {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "DriverCallRequest".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let id = generator.subschema_for::<u32>().to_value();
        // No arm refers to `DriverCall` itself. This puts it in the document
        // anyway, so the TypeScript names every call a `[driver]` block lists.
        let _ = generator.subschema_for::<DriverCall>();
        let calls = <DriverCall as schemars::JsonSchema>::json_schema(generator).to_value();
        let table = <DriverArgs as schemars::JsonSchema>::json_schema(generator).to_value();
        let members = table
            .get("properties")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();

        let mut arms = Vec::new();
        let mut bare = Vec::new();
        for call in DriverCall::all() {
            let name = call.as_str();
            let case = one_call(&calls, name);
            if members.contains_key(name) {
                arms.push(ask(&id, case, table_of(&members, Some(name)), true));
            } else {
                bare.push(case);
            }
        }
        let no_table = json!({ "anyOf": [table_of(&members, None), null_only()] });
        arms.push(ask(&id, json!({ "oneOf": bare }), no_table, false));

        schemars::json_schema!({
            "description": "One capability ask a driver writes mid-session. `id` is the \
                           driver's own number for the ask. The host echoes it on the answer, \
                           so a driver that sends two asks at once can tell the answers apart. \
                           A call that reads arguments sends `args` with one member, named for \
                           that call, and any other member only as null. A call that reads \
                           none sends no member. `point` and `body` belong to a session \
                           response, so an ask carries them only as null.",
            "oneOf": arms
        })
    }
}

/// The case [`DriverCall`]'s own schema gives the call `name`, which keeps
/// its description. A bare constant if that schema has no such case.
fn one_call(calls: &Value, name: &str) -> Value {
    calls
        .get("oneOf")
        .and_then(Value::as_array)
        .and_then(|cases| {
            cases
                .iter()
                .find(|case| case.get("const").and_then(Value::as_str) == Some(name))
        })
        .cloned()
        .unwrap_or_else(|| json!({ "const": name, "type": "string" }))
}

/// One arm of the [`DriverCallRequest`] schema: the calls it covers, and the
/// `args` those calls take.
fn ask(id: &Value, call: Value, args: Value, needs_args: bool) -> Value {
    let required = if needs_args {
        json!(["id", "call", "args"])
    } else {
        json!(["id", "call"])
    };
    json!({
        "type": "object",
        "properties": {
            "id": id,
            "call": call,
            "args": args,
            "point": null_only(),
            "body": null_only()
        },
        "required": required,
        "additionalProperties": false
    })
}

/// An `args` table that sets the member `named` and no other. With no name,
/// a table that sets no member at all.
fn table_of(members: &Map<String, Value>, named: Option<&str>) -> Value {
    let mut properties = Map::new();
    for (member, schema) in members {
        let allowed = if named == Some(member.as_str()) {
            present(schema)
        } else {
            null_only()
        };
        properties.insert(member.clone(), allowed);
    }
    let mut table = Map::new();
    table.insert("type".to_string(), json!("object"));
    table.insert("properties".to_string(), Value::Object(properties));
    table.insert("additionalProperties".to_string(), Value::Bool(false));
    if let Some(name) = named {
        table.insert("required".to_string(), json!([name]));
    }
    Value::Object(table)
}

/// A member's schema without the `null` case the derive gives each `Option`.
/// The call an ask names must send its own table, not `null`.
fn present(member: &Value) -> Value {
    let mut kept = member.as_object().cloned().unwrap_or_default();
    if let Some(Value::Array(cases)) = kept.remove("anyOf") {
        for case in cases {
            if let Value::Object(fields) = case
                && fields.get("type").and_then(Value::as_str) != Some("null")
            {
                kept.extend(fields);
            }
        }
    }
    Value::Object(kept)
}
