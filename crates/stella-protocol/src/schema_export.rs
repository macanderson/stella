// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A machine-checked description of the `AgentEvent` wire format.
//!
//! [`AgentEvent`] is the wire format for three surfaces at once — the TUI
//! folds it, `--output-format stream-json` prints it, and `stella-serve`
//! streams it over SSE — and until this module existed nothing proved that a
//! change to it was additive. `docs/spec/serve-surface.md` says so about
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

// The crate denies `expect` under invariant #5, and the two below are the one
// place the lint is wrong. Both serialize a `serde_json::Value`, and the only
// two ways `serde_json` can fail to serialize — a map key that is not a string,
// a non-finite float — are unrepresentable in `Value`. The failure is excluded
// by the type, not by convention, so plumbing a `Result` no caller could match
// on would trade a proof for ceremony.
#![allow(clippy::expect_used)]

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

/// How a tagged union spells its discriminant, and how the union of that
/// discriminant's values is published.
///
/// The printer used to assume `"type"`, which is what `AgentEvent` and
/// `ServerFrame` both use and is *not* what serde produces for every tagged
/// enum. The wrapper socket's envelope is `#[serde(tag = "point", content =
/// "body")]`, so its arms pin no `type` const and the printer refused it with
/// `variant carries no "type" const` — which left the one contract whose
/// audience writes TypeScript as the one contract with no `.d.ts` (#4535).
///
/// Three fields rather than one, because the union's *name* and its prose are
/// per-contract facts a key cannot be run backwards into: `"point"` does not
/// spell `WrapperPoint`, and the sentence a consumer needs about an
/// unrecognized `AgentEvent` tag ("an event from a newer stella") is not the
/// sentence they need about an unrecognized socket point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Discriminant {
    /// The property every variant of a tagged union pins with a `const`.
    pub key: &'static str,
    /// The exported name for the union of those `const` values.
    pub vocabulary: &'static str,
    /// The prose printed as a JSDoc block above that union. Blank lines and
    /// line breaks are preserved.
    pub doc: &'static str,
}

impl Discriminant {
    /// `"type"`, as `AgentEvent` and `ServerFrame` both tag it.
    pub const EVENT_TYPE: Self = Self {
        key: "type",
        vocabulary: "KnownTypeTag",
        doc: "Every `\"type\"` tag this build emits. A value outside this union is an\n\
              event from a newer stella — keep it, do not fail on it.",
    };
}

/// The JSON Schema (2020-12) for one line of the event stream: [`AgentEvent`]
/// and its whole payload graph, plus the optional `ts` its sink stamps on.
///
/// `ts` is not a member of any variant — it is added at the write boundary by
/// [`crate::journal::stamped_line`], because a stamp is a fact about a write and
/// the engine that produces events owns no clock (#2111). But this artifact
/// exists to describe *the bytes a consumer reads*, and those bytes carry the
/// key. Omitting it would leave the one generated document a downstream
/// implementer trusts silently short of the wire, which is the failure this
/// module was built to prevent.
///
/// So it is attached here rather than derived, in one uniform pass over every
/// variant, with its prose taken from [`crate::journal::TS_DESCRIPTION`] so the
/// published contract and the Rust doc cannot drift. Attaching, not deriving,
/// is the honest shape: `schemars` would only reproduce it by flattening an
/// envelope into the root, which would cost the discriminated union — and with
/// it `KnownTypeTag`, the one thing a forward-compatible consumer most needs.
#[must_use]
pub fn agent_event_schema() -> Value {
    let mut schema = schemars::schema_for!(AgentEvent).to_value();
    attach_journal_stamp(&mut schema);
    schema
}

/// Declare `ts` as an optional property of every root variant.
///
/// Optional on purpose, and on every surface: a line recorded before the field
/// existed has none, and `stella-serve` frames the event in an envelope that
/// stamps its own. "May be present" is the only claim true of all three
/// surfaces this schema covers.
///
/// Silent about a root that is not a `oneOf` of objects. Such a root cannot
/// come from `AgentEvent` — the sole caller — and the exporter's own
/// [`tag_literals`] already fails loudly on that shape, so a second bespoke
/// error here would only duplicate a check that is better placed.
fn attach_journal_stamp(schema: &mut Value) {
    let property = serde_json::json!({
        "description": crate::journal::TS_DESCRIPTION,
        "format": "uint64",
        "minimum": 0,
        "type": "integer",
    });
    let Some(variants) = schema.get_mut("oneOf").and_then(Value::as_array_mut) else {
        return;
    };
    for variant in variants {
        if let Some(properties) = variant
            .get_mut("properties")
            .and_then(Value::as_object_mut)
            .filter(|properties| properties.contains_key("type"))
        {
            properties.insert("ts".to_string(), property.clone());
        }
    }
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
//
// Every variant carries an optional `ts`: the wall-clock instant its sink wrote
// the line, in milliseconds since the Unix epoch. It is stamped by the sink, not
// by the event, so it is absent on any line recorded before the field existed —
// treat it as optional forever, and clamp a negative delta, because a system
// clock is not monotonic.
";

/// Print TypeScript declarations for a schema produced by
/// [`agent_event_schema`].
///
/// # Errors
///
/// [`UnsupportedSchema`] when the schema uses a construct outside the subset
/// `schemars` emits for these types.
pub fn typescript_declarations(schema: &Value) -> Result<String, UnsupportedSchema> {
    typescript_declarations_with_header(schema, HEADER, Discriminant::EVENT_TYPE)
}

/// [`typescript_declarations`] under a caller-supplied banner and
/// [`Discriminant`].
///
/// The printer is generic over the schema but neither the banner nor the
/// discriminant is: the banner names the regeneration command and the source of
/// truth, and the discriminant names the key a tagged union pins. Both differ
/// per artifact. `stella-serve` and `stella-plugin` print their own contracts
/// through this same function rather than growing a second printer, because two
/// printers is two subsets of JSON Schema to keep in step and the whole point of
/// this module is not having two of something.
///
/// # The three root shapes
///
/// - a **tagged union** (`AgentEvent`, `ServerFrame`, `WrapperRequest`) — a
///   `oneOf` of variants each pinning `discriminant.key`, which prints as a
///   discriminated union followed by the vocabulary of its tags;
/// - a **plain struct** (`ToolResultIn`, `ProviderResultIn`) — one object,
///   which prints as an interface exactly as a `$defs` entry would;
/// - a **definitions container** (`StellaWrapperSocket`) — `$defs` and no root
///   type of its own, which prints its definitions and no root declaration.
///
/// The second was added when `stella-serve` needed to print its two inbound
/// request bodies: they are the bodies of two *different* endpoints and carry no
/// discriminant, so wrapping them in a union to satisfy a union-only printer
/// would have published a false claim — that a client may send either shape to
/// either route. The third arrived with the wrapper socket, whose request and
/// response directions are likewise not alternatives (`point` is a legal tag on
/// both), and whose payload types must be declared once rather than twice.
/// Teaching the printer the shape that actually exists is the honest fix in
/// both cases, and the reason it refuses rather than guesses is that it caught
/// them.
///
/// A container prints no root declaration and no root JSDoc: there is no type
/// to attach either to, and the document-level prose belongs in the caller's
/// banner where a reader meets it first. Its tag vocabulary is collected from
/// whichever `$defs` entries *are* tagged unions on `discriminant.key`; an
/// entry whose `oneOf` is something else (a nullable, an untagged alternation)
/// contributes nothing and is not an error.
///
/// # Errors
///
/// [`UnsupportedSchema`], as [`typescript_declarations`].
pub fn typescript_declarations_with_header(
    schema: &Value,
    header: &str,
    discriminant: Discriminant,
) -> Result<String, UnsupportedSchema> {
    let root = schema.as_object().ok_or_else(|| UnsupportedSchema {
        path: "#".to_string(),
        detail: "root schema is not an object".to_string(),
    })?;

    let mut out = String::from(header);

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

    // Then the root itself — one of the three shapes the doc comment above
    // names. A container has no root type at all, so it prints neither a
    // declaration nor the JSDoc that would have to attach to one.
    let root_object = root
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| t == "object")
        && root.contains_key("properties");
    let root_container = !root_object && !root.contains_key("oneOf") && root.contains_key("$defs");
    if !root_container {
        out.push('\n');
        push_doc(&mut out, schema, "");
        if root_object {
            let body = object_body(root, "#", 1)?;
            let _ = writeln!(out, "export interface {title} {{\n{body}}}");
        } else {
            let ty = type_expr_ignoring(root, &["$defs", "$schema", "title"], "#", 0)?;
            let _ = writeln!(out, "export type {title} = {ty};");
        }
    }

    // Finally the tag vocabulary as a standalone union — the one thing a
    // consumer needs that the discriminated union above does not hand them
    // directly: the set of tag values this build knows, so an unrecognized one
    // routes to forward-compat handling instead of an error branch. Meaningless
    // for an object root, which has no tags, so it is emitted only where there
    // are tags to name.
    //
    // A *type*, not a `const`: this is a `.d.ts`, and an initializer in an
    // ambient context is both a TypeScript error and a claim that some JS
    // module exports the array at runtime. Nothing does.
    let tags = if root_container {
        container_tag_literals(root, discriminant.key)
    } else if root_object {
        Vec::new()
    } else {
        tag_literals(root, "#", discriminant.key)?
    };
    if !tags.is_empty() {
        out.push('\n');
        push_jsdoc(&mut out, discriminant.doc, "");
        let _ = writeln!(
            out,
            "export type {} =\n{};",
            discriminant.vocabulary,
            tags.iter()
                .map(|t| format!("  | {}", json_literal(&Value::String(t.clone()))))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    Ok(out)
}

/// The tag const of every variant of a `oneOf`, in wire order.
fn tag_literals(
    owner: &Map<String, Value>,
    path: &str,
    key: &str,
) -> Result<Vec<String>, UnsupportedSchema> {
    let variants = owner
        .get("oneOf")
        .and_then(Value::as_array)
        .ok_or_else(|| UnsupportedSchema {
            path: path.to_string(),
            detail: "no `oneOf` of tagged variants".to_string(),
        })?;
    variants
        .iter()
        .enumerate()
        .map(|(i, variant)| {
            variant
                .pointer(&format!("/properties/{key}/const"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| UnsupportedSchema {
                    path: format!("{path}/oneOf/{i}"),
                    detail: format!("variant carries no `{key}` const"),
                })
        })
        .collect()
}

/// The tag vocabulary of a definitions container: every value pinned by any
/// `$defs` entry that is a union tagged on `key`, first-seen order, deduped.
///
/// A `$defs` entry whose `oneOf` is *not* tagged on `key` — a nullable, an
/// untagged alternation — contributes nothing and is not an error. A container
/// is a bag of definitions and is not obliged to be uniform; refusing one that
/// holds an ordinary union beside a tagged one would make the printer stricter
/// than the shape it is describing.
fn container_tag_literals(root: &Map<String, Value>, key: &str) -> Vec<String> {
    let Some(defs) = root.get("$defs").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut vocabulary: Vec<String> = Vec::new();
    for (name, def) in defs {
        let Some(def) = def.as_object().filter(|d| d.contains_key("oneOf")) else {
            continue;
        };
        let Ok(tags) = tag_literals(def, &format!("#/$defs/{name}"), key) else {
            continue;
        };
        for tag in tags {
            if !vocabulary.contains(&tag) {
                vocabulary.push(tag);
            }
        }
    }
    vocabulary
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
    push_jsdoc(out, description, pad);
}

/// Emit `text` as a JSDoc block, one source line per line.
fn push_jsdoc(out: &mut String, text: &str, pad: &str) {
    let _ = writeln!(out, "{pad}/**");
    for line in text.lines() {
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
        let tags = tag_literals(
            schema.as_object().unwrap(),
            "#",
            Discriminant::EVENT_TYPE.key,
        )
        .unwrap();
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

    /// A discriminant other than `"type"`, as `#[serde(tag = "point")]`
    /// produces. The union prints, and so does the vocabulary of its tags under
    /// the name the caller gave it.
    const POINT: Discriminant = Discriminant {
        key: "point",
        vocabulary: "WrapperPoint",
        doc: "Every point this build speaks.",
    };

    /// **The witness for #4535.** A union tagged on something other than
    /// `"type"` printed nothing at all before this: `tag_literals` looked up
    /// `/properties/type/const` on every arm and refused the whole document,
    /// which is why the wrapper socket shipped a schema and no `.d.ts`.
    #[test]
    fn a_union_tagged_on_another_key_prints_its_own_vocabulary() {
        let schema = serde_json::json!({
            "title": "WrapperRequest",
            "oneOf": [
                { "type": "object", "properties": { "point": { "const": "before_turn" } },
                  "required": ["point"] },
                { "type": "object", "properties": { "point": { "const": "after_turn" } },
                  "required": ["point"] },
            ],
        });

        // The hardcoded key refuses it, naming the arm and the key it wanted.
        let refused =
            typescript_declarations_with_header(&schema, "", Discriminant::EVENT_TYPE).unwrap_err();
        assert_eq!(refused.path, "#/oneOf/0");
        assert_eq!(refused.detail, "variant carries no `type` const");

        let ts = typescript_declarations_with_header(&schema, "", POINT).unwrap();
        assert!(ts.contains("export type WrapperRequest ="), "{ts}");
        assert!(
            ts.contains("export type WrapperPoint =\n  | \"before_turn\"\n  | \"after_turn\";"),
            "{ts}"
        );
        assert!(
            !ts.contains("KnownTypeTag"),
            "the vocabulary is named by the caller, not by this module: {ts}"
        );
    }

    /// A definitions container has no root type, so it prints none — and its
    /// vocabulary is the union of whatever its `$defs` entries pin.
    ///
    /// Deduped and in first-seen order: the wrapper socket's request and
    /// response directions are two tagged unions over the *same* two points, and
    /// a `WrapperPoint` listing each of them twice would not compile.
    #[test]
    fn a_definitions_container_prints_its_definitions_and_no_root() {
        let arm = |point: &str| {
            serde_json::json!({
                "type": "object",
                "properties": { "point": { "const": point } },
                "required": ["point"],
            })
        };
        let schema = serde_json::json!({
            "title": "StellaWrapperSocket",
            "description": "A container, not a union.",
            "$defs": {
                "Untagged": { "oneOf": [{ "type": "string" }, { "type": "null" }] },
                "WrapperRequest": { "oneOf": [arm("before_turn"), arm("after_turn")] },
                "WrapperResponse": { "oneOf": [arm("before_turn"), arm("after_turn")] },
            },
        });

        let ts = typescript_declarations_with_header(&schema, "", POINT).unwrap();
        assert!(ts.contains("export type WrapperRequest ="), "{ts}");
        assert!(ts.contains("export type WrapperResponse ="), "{ts}");
        assert!(
            !ts.contains("export type StellaWrapperSocket"),
            "a container is not a type: {ts}"
        );
        assert!(
            ts.contains("export type WrapperPoint =\n  | \"before_turn\"\n  | \"after_turn\";"),
            "{ts}"
        );
        // An entry whose `oneOf` is not tagged on the key is an ordinary union,
        // not a defect: it prints and contributes no tag.
        assert!(ts.contains("export type Untagged = string | null;"), "{ts}");
    }
}
