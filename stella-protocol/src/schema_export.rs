// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A machine-checked description of the `AgentEvent` wire format.
//!
//! [`AgentEvent`] is the wire format for three surfaces at once — the TUI
//! folds it, `--output-format stream-json` prints it, and `stella-serve`
//! streams it over SSE — and until this module existed nothing proved that a
//! change to it was additive. `docs/design/serve-surface.md` says so about
//! itself: it opens with a hand-maintained table of routes and calls that
//! prose "the single most dangerous drift in this document". A hand-written
//! schema would be a second copy of the same problem. These artifacts are
//! *derived* from the types, so they cannot describe a shape the code does not
//! have.
//!
//! Two artifacts, both committed under `docs/wire/` so drift is reviewable as
//! a PR diff rather than discovered by a consumer:
//!
//! - `agentevent.schema.json` — JSON Schema 2020-12 for the event and its
//!   whole payload graph, straight from `schemars`.
//! - `agentevent.d.ts` — the same contract as TypeScript declarations,
//!   printed by [`typescript_declarations`] below.
//!
//! # Why a hand-rolled TypeScript printer
//!
//! The obvious alternative is `json-schema-to-typescript`, which would put a
//! Node toolchain — and a `package.json`, and a lockfile, and a CI install
//! step — into a Rust workspace for the sake of one generated file. The
//! printer here is a few hundred lines over the *exact* subset of JSON Schema
//! `schemars` emits for these types, and it fails loudly
//! ([`UnsupportedSchema`]) rather than guessing when it meets a construct it
//! does not model. If a future type introduces one, the exporter stops instead
//! of quietly writing a `.d.ts` that lies.
//!
//! # What is deliberately NOT described
//!
//! [`AgentEvent::Unknown`] carries no wire tag of its own (`serde(skip)`), so
//! it is absent from the schema — correctly. An `Unknown` value re-serializes
//! as the foreign object it wrapped, whose `"type"` is by definition not one
//! this build knows, so it validates against no variant here. The schema
//! describes *this build's* vocabulary; a stream carrying events from a newer
//! stella is expected to contain objects it rejects, and a consumer must treat
//! an unmatched `"type"` as forward-compat news rather than corruption. See
//! the `event` module docs.
//!
//! # Determinism
//!
//! [`artifacts`] is a pure function of the types: `serde_json::Map` is a
//! `BTreeMap` here (no `preserve_order`), so object keys sort, and every array
//! `schemars` emits is in declaration order. Running the exporter twice
//! produces byte-identical files, which is what lets
//! `scripts/check-wire-schema.sh` treat any diff as real drift.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde_json::{Map, Value};

use crate::AgentEvent;

/// A JSON Schema construct the TypeScript printer does not model.
///
/// Loud on purpose. The printer covers the subset `schemars` emits for the
/// event graph as it stands; a new type that reaches for something outside it
/// must extend the printer rather than get a silently wrong declaration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unsupported JSON Schema construct at {path}: {detail}")]
pub struct UnsupportedSchema {
    /// JSON-pointer-ish path to the offending subschema.
    pub path: String,
    /// What about it could not be printed.
    pub detail: String,
}

/// The JSON Schema (2020-12) for [`AgentEvent`] and its whole payload graph.
#[must_use]
pub fn agent_event_schema() -> Value {
    schemars::schema_for!(AgentEvent).to_value()
}

/// The schema, pretty-printed with a trailing newline.
#[must_use]
pub fn schema_json() -> String {
    let mut json = serde_json::to_string_pretty(&agent_event_schema())
        .expect("a JSON Schema is always serializable");
    json.push('\n');
    json
}

/// Every committed artifact, as `(filename, contents)`.
///
/// # Errors
///
/// [`UnsupportedSchema`] when the generated schema uses a construct the
/// TypeScript printer does not model.
pub fn artifacts() -> Result<Vec<(&'static str, String)>, UnsupportedSchema> {
    let schema = agent_event_schema();
    let ts = typescript_declarations(&schema)?;
    let mut json =
        serde_json::to_string_pretty(&schema).expect("a JSON Schema is always serializable");
    json.push('\n');
    Ok(vec![
        ("agentevent.schema.json", json),
        ("agentevent.d.ts", ts),
    ])
}

const HEADER: &str = "\
// GENERATED FILE — DO NOT EDIT.
//
// Regenerate with:  bash scripts/export-agentevent-schema.sh
// Source of truth:  stella-protocol/src/event.rs (`AgentEvent`)
// Guarded by:       scripts/check-wire-schema.sh (`make wire-schema`)
//
// The event stream emitted by `stella --output-format stream-json` (one JSON
// object per line), by `stella-serve` over SSE, and folded by the TUI.
//
// A `\"type\"` value not listed here is NOT an error: it is an event from a
// newer stella. Forward-compatible consumers keep such a line intact and move
// on, exactly as `AgentEvent::Unknown` does on the Rust side.
";

/// Print TypeScript declarations for a schema produced by
/// [`agent_event_schema`].
///
/// # Errors
///
/// [`UnsupportedSchema`] when the schema uses a construct outside the subset
/// `schemars` emits for these types.
pub fn typescript_declarations(schema: &Value) -> Result<String, UnsupportedSchema> {
    let root = schema.as_object().ok_or_else(|| UnsupportedSchema {
        path: "#".to_string(),
        detail: "root schema is not an object".to_string(),
    })?;

    let mut out = String::from(HEADER);

    let title = root
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("AgentEvent");

    // Named payload types first, alphabetically (the `$defs` map is a BTreeMap,
    // so this order is the schema's own and is stable across runs).
    if let Some(defs) = root.get("$defs") {
        let defs = defs.as_object().ok_or_else(|| UnsupportedSchema {
            path: "#/$defs".to_string(),
            detail: "$defs is not an object".to_string(),
        })?;
        for (name, def) in defs {
            let path = format!("#/$defs/{name}");
            out.push('\n');
            push_doc(&mut out, def, "");
            let object = def
                .as_object()
                .filter(|o| o.contains_key("properties"))
                .filter(|o| o.get("type").and_then(Value::as_str) == Some("object"));
            if let Some(object) = object {
                // A plain struct prints as an interface: it is the shape a
                // consumer most often wants to extend or implement.
                let body = object_body(object, &path, 1)?;
                let _ = writeln!(out, "export interface {name} {body}");
            } else {
                let ty = type_expr(def, &path, 0)?;
                let _ = writeln!(out, "export type {name} = {ty};");
            }
        }
    }

    // Then the root union.
    out.push('\n');
    push_doc(&mut out, schema, "");
    let ty = type_expr_ignoring(root, &["$defs", "$schema", "title"], "#", 0)?;
    let _ = writeln!(out, "export type {title} = {ty};");

    // Finally the tag vocabulary as a standalone union — the one thing a
    // consumer needs that the discriminated union above does not hand them
    // directly: the set of `"type"` values this build knows, so an
    // unrecognized one routes to forward-compat handling instead of an error
    // branch.
    //
    // A *type*, not a `const`: this is a `.d.ts`, and an initializer in an
    // ambient context is both a TypeScript error and a claim that some JS
    // module exports the array at runtime. Nothing does.
    let tags = tag_literals(root)?;
    out.push('\n');
    out.push_str(
        "/**\n * Every `\"type\"` tag this build emits. A value outside this union is an\n \
         * event from a newer stella — keep it, do not fail on it.\n */\n",
    );
    let _ = writeln!(
        out,
        "export type KnownTypeTag =\n{};",
        tags.iter()
            .map(|t| format!("  | {}", json_literal(&Value::String(t.clone()))))
            .collect::<Vec<_>>()
            .join("\n")
    );

    Ok(out)
}

/// The `"type"` const of every root variant, in wire order.
fn tag_literals(root: &Map<String, Value>) -> Result<Vec<String>, UnsupportedSchema> {
    let variants =
        root.get("oneOf")
            .and_then(Value::as_array)
            .ok_or_else(|| UnsupportedSchema {
                path: "#".to_string(),
                detail: "root schema has no `oneOf` of tagged variants".to_string(),
            })?;
    variants
        .iter()
        .enumerate()
        .map(|(i, variant)| {
            variant
                .pointer("/properties/type/const")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| UnsupportedSchema {
                    path: format!("#/oneOf/{i}"),
                    detail: "variant carries no `type` const".to_string(),
                })
        })
        .collect()
}

/// Keys that carry no type information and are skipped when deciding what a
/// subschema *is*.
const ANNOTATIONS: &[&str] = &[
    "description",
    "title",
    "default",
    "format",
    "minimum",
    "maximum",
    "examples",
    "deprecated",
    "$comment",
];

/// Render one subschema as a TypeScript type expression.
fn type_expr(schema: &Value, path: &str, indent: usize) -> Result<String, UnsupportedSchema> {
    match schema {
        // A boolean schema: `true` accepts anything, `false` accepts nothing.
        Value::Bool(true) => return Ok("unknown".to_string()),
        Value::Bool(false) => return Ok("never".to_string()),
        Value::Object(map) => return type_expr_ignoring(map, &[], path, indent),
        _ => {}
    }
    Err(UnsupportedSchema {
        path: path.to_string(),
        detail: format!("subschema is neither an object nor a boolean: {schema}"),
    })
}

fn type_expr_ignoring(
    map: &Map<String, Value>,
    ignore: &[&str],
    path: &str,
    indent: usize,
) -> Result<String, UnsupportedSchema> {
    let keys: BTreeSet<&str> = map
        .keys()
        .map(String::as_str)
        .filter(|k| !ANNOTATIONS.contains(k) && !ignore.contains(k))
        .collect();

    // An empty subschema (`{}`) accepts any JSON — `serde_json::Value` fields.
    if keys.is_empty() {
        return Ok("unknown".to_string());
    }

    if let Some(reference) = map.get("$ref") {
        let reference = reference.as_str().ok_or_else(|| UnsupportedSchema {
            path: path.to_string(),
            detail: "$ref is not a string".to_string(),
        })?;
        return reference
            .strip_prefix("#/$defs/")
            .map(str::to_owned)
            .ok_or_else(|| UnsupportedSchema {
                path: path.to_string(),
                detail: format!("only local `#/$defs/…` references are supported, got {reference}"),
            });
    }

    for combinator in ["oneOf", "anyOf"] {
        if let Some(branches) = map.get(combinator) {
            let branches = branches.as_array().ok_or_else(|| UnsupportedSchema {
                path: path.to_string(),
                detail: format!("`{combinator}` is not an array"),
            })?;
            let mut parts = Vec::with_capacity(branches.len());
            for (i, branch) in branches.iter().enumerate() {
                let part = type_expr(branch, &format!("{path}/{combinator}/{i}"), indent)?;
                if !parts.contains(&part) {
                    parts.push(part);
                }
            }
            return Ok(parts.join(" | "));
        }
    }

    if let Some(constant) = map.get("const") {
        return Ok(json_literal(constant));
    }

    if let Some(values) = map.get("enum") {
        let values = values.as_array().ok_or_else(|| UnsupportedSchema {
            path: path.to_string(),
            detail: "`enum` is not an array".to_string(),
        })?;
        return Ok(values
            .iter()
            .map(json_literal)
            .collect::<Vec<_>>()
            .join(" | "));
    }

    let Some(ty) = map.get("type") else {
        return Err(UnsupportedSchema {
            path: path.to_string(),
            detail: format!("no `$ref`, combinator, `const`, `enum` or `type` (keys: {keys:?})"),
        });
    };

    // `"type": ["string", "null"]` — how schemars writes `Option<primitive>`.
    let names: Vec<&str> = match ty {
        Value::String(one) => vec![one.as_str()],
        Value::Array(many) => many
            .iter()
            .map(|v| {
                v.as_str().ok_or_else(|| UnsupportedSchema {
                    path: path.to_string(),
                    detail: "`type` array holds a non-string".to_string(),
                })
            })
            .collect::<Result<_, _>>()?,
        other => {
            return Err(UnsupportedSchema {
                path: path.to_string(),
                detail: format!("`type` is neither a string nor an array: {other}"),
            });
        }
    };

    let mut parts = Vec::with_capacity(names.len());
    for name in names {
        let part = match name {
            "string" => "string".to_string(),
            "integer" | "number" => "number".to_string(),
            "boolean" => "boolean".to_string(),
            "null" => "null".to_string(),
            "array" => {
                let items = map.get("items").ok_or_else(|| UnsupportedSchema {
                    path: path.to_string(),
                    detail: "array schema has no `items` (tuple schemas are not modelled)"
                        .to_string(),
                })?;
                let inner = type_expr(items, &format!("{path}/items"), indent)?;
                // `A | B` must not become `A | B[]`.
                if inner.contains(' ') {
                    format!("Array<{inner}>")
                } else {
                    format!("{inner}[]")
                }
            }
            "object" => {
                if map.contains_key("properties") {
                    object_body(map, path, indent + 1)?
                } else {
                    "Record<string, unknown>".to_string()
                }
            }
            other => {
                return Err(UnsupportedSchema {
                    path: path.to_string(),
                    detail: format!("unmodelled JSON type `{other}`"),
                });
            }
        };
        if !parts.contains(&part) {
            parts.push(part);
        }
    }
    Ok(parts.join(" | "))
}

/// Render `{ a: A; b?: B }` for an object subschema, one property per line.
fn object_body(
    map: &Map<String, Value>,
    path: &str,
    indent: usize,
) -> Result<String, UnsupportedSchema> {
    let properties = map
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| UnsupportedSchema {
            path: path.to_string(),
            detail: "object schema has no `properties` map".to_string(),
        })?;
    let required: BTreeSet<&str> = map
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    let pad = "  ".repeat(indent);
    let closing = "  ".repeat(indent - 1);
    let mut out = String::from("{\n");
    for (name, property) in properties {
        let property_path = format!("{path}/properties/{name}");
        push_doc(&mut out, property, &pad);
        let ty = type_expr(property, &property_path, indent)?;
        let optional = if required.contains(name.as_str()) {
            ""
        } else {
            "?"
        };
        let _ = writeln!(out, "{pad}{}{optional}: {ty};", property_key(name));
    }
    // `additionalProperties: false` is schemars' externally-tagged-enum shape;
    // TypeScript object literals are already exact, so nothing to add. Any
    // *schema* value there would be an index signature we do not model.
    if let Some(extra) = map.get("additionalProperties")
        && extra != &Value::Bool(false)
    {
        return Err(UnsupportedSchema {
            path: path.to_string(),
            detail: format!("`additionalProperties` is not `false`: {extra}"),
        });
    }
    out.push_str(&closing);
    out.push('}');
    Ok(out)
}

/// A property name, quoted only when it is not a bare TS identifier.
fn property_key(name: &str) -> String {
    let bare = !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if bare {
        name.to_string()
    } else {
        json_literal(&Value::String(name.to_string()))
    }
}

/// A JSON value as a TypeScript literal type.
fn json_literal(value: &Value) -> String {
    match value {
        Value::String(s) => serde_json::to_string(s).unwrap_or_else(|_| format!("{s:?}")),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

/// Emit the subschema's `description` as a JSDoc block, if it has one.
fn push_doc(out: &mut String, schema: &Value, pad: &str) {
    let Some(description) = schema.get("description").and_then(Value::as_str) else {
        return;
    };
    let _ = writeln!(out, "{pad}/**");
    for line in description.lines() {
        // A doc comment containing `*/` would close the block early.
        let line = line.replace("*/", "*\\/");
        if line.is_empty() {
            let _ = writeln!(out, "{pad} *");
        } else {
            let _ = writeln!(out, "{pad} * {line}");
        }
    }
    let _ = writeln!(out, "{pad} */");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_covers_exactly_the_known_tag_vocabulary() {
        let schema = agent_event_schema();
        let tags = tag_literals(schema.as_object().unwrap()).unwrap();
        assert_eq!(
            tags,
            crate::KNOWN_TYPE_TAGS,
            "the schema's variant tags must be KNOWN_TYPE_TAGS, in order"
        );
    }

    #[test]
    fn export_is_deterministic() {
        assert_eq!(artifacts().unwrap(), artifacts().unwrap());
    }

    #[test]
    fn the_declarations_name_every_payload_type() {
        let schema = agent_event_schema();
        let ts = typescript_declarations(&schema).unwrap();
        for name in schema["$defs"].as_object().unwrap().keys() {
            assert!(
                ts.contains(&format!("export interface {name} "))
                    || ts.contains(&format!("export type {name} =")),
                "no declaration emitted for {name}"
            );
        }
        assert!(ts.contains("export type AgentEvent ="));
    }

    #[test]
    fn optional_fields_print_as_optional() {
        let schema = agent_event_schema();
        let ts = typescript_declarations(&schema).unwrap();
        // `ManifestEntry::call_id` is `Option<String>` behind `serde(default,
        // skip_serializing_if)` — absent from `required`, so optional AND
        // nullable in TypeScript.
        assert!(ts.contains("call_id?: string | null;"), "{ts}");
        // A mandatory scalar keeps no `?`.
        assert!(ts.contains("block_id: string;"), "{ts}");
    }

    #[test]
    fn an_unmodelled_construct_fails_loudly_instead_of_guessing() {
        let schema = serde_json::json!({
            "title": "Weird",
            "$defs": { "Weird": { "not": { "type": "string" } } },
            "oneOf": [{ "properties": { "type": { "const": "weird" } } }],
        });
        let err = typescript_declarations(&schema).unwrap_err();
        assert_eq!(err.path, "#/$defs/Weird");
    }

    #[test]
    fn a_union_item_type_is_parenthesised_by_array_generic() {
        // `Array<A | B>`, never `A | B[]`.
        let schema = serde_json::json!({
            "type": "array",
            "items": { "type": ["string", "null"] }
        });
        assert_eq!(type_expr(&schema, "#", 0).unwrap(), "Array<string | null>");
    }
}
