// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Dispatch-time validation of a tool call's input against the tool's
//! advertised `input_schema` (#3144).
//!
//! [`ToolCall::input`]'s doc comment has always promised "validate against
//! `ToolSchema::input_schema` rather than trusting the shape" — this module
//! is the one seam that performs it, in `ToolRegistry::execute`, before the
//! tool runs. Without it a mistyped argument was misreported as a missing
//! one (`{"query": 42}` → "query is required") or silently defaulted
//! (`"trace": "yes"` → traced nothing), so the model retried against a
//! refusal naming the wrong defect.
//!
//! # The enforced subset
//!
//! The catalog's schemas use a small JSON-Schema subset, and this checker
//! deliberately implements exactly that subset rather than pulling in a
//! `jsonschema` dependency:
//!
//! - **`required`** — every listed field must be present. The refusal is
//!   [`InputError::Missing`], byte-identical to the wording every tool has
//!   always used.
//! - **`properties.*.type`** — a present field must carry a declared type.
//!   Both spellings are honored: a single string (`"type": "string"`) and an
//!   array of alternatives (`"type": ["string", "null"]` — any listed type
//!   passes). An absent `type` checks nothing. `"integer"` rejects a number
//!   with a fractional part; an integer against `"number"` passes. A type
//!   word outside the known seven checks nothing (fail-open: an exotic
//!   schema must not brick its tool).
//! - **`properties.*.enum`** — a present field must equal one member.
//! - **`properties.*.items.type`** — each element of a present array must
//!   carry the declared item type ([`InputError::WrongElementType`], which
//!   names the index).
//! - **`additionalProperties: false`** — unknown keys are rejected, but
//!   **only** for schemas that advertise this (the `task` tool,
//!   foundry-authored tools, custom manifests that opt in).
//!
//! # Unknown keys are otherwise ignored — deliberately (#3144)
//!
//! For every schema that does not declare `additionalProperties: false`,
//! unknown keys pass exactly as they always have. Models pass stray extra
//! keys constantly, and a new refusal class for them would fail calls that
//! previously succeeded — a solve-rate regression bought for pedantry. The
//! conservative decision, made in #3144: enforce strictness only where the
//! tool already advertises it.
//!
//! # `null` reads as absence
//!
//! [`crate::input`]'s `present` treats an explicit `null` as an absent
//! field, because every JSON encoder in the wild emits `null` for "no
//! value", and every tool reads its input through that convention. This
//! validator applies the same rule rather than inventing a stricter one the
//! tools would then contradict: a required field sent as `null` is refused
//! as [`InputError::Missing`] — unless its declared `type` lists `"null"`,
//! in which case it is a legitimate value — and an optional field sent as
//! `null` is skipped, exactly as the tool itself will skip it.
//!
//! # MCP and custom tools
//!
//! MCP-supplied tools (`mcp__`-prefixed) and custom script tools pass
//! through the same seam and are validated against whatever schema they
//! advertise. A schema with no `properties`/`required` (common for external
//! servers) naturally checks nothing — the validator enforces only what a
//! schema declares, so a permissive tool stays permissive.
//!
//! # Determinism
//!
//! Refusal text is model-visible and must be byte-deterministic (the loop
//! detector compares outputs). One call
//! yields at most one refusal, chosen in a fixed order: `required` fields in
//! schema order, then per-property checks in the property map's iteration
//! order (stable for a given build), then unknown keys sorted
//! lexicographically. No timings, no unstable map orders.
//!
//! [`ToolCall::input`]: stella_protocol::tool::ToolCall

use serde_json::Value;
use stella_protocol::tool::ToolOutput;

use super::Tool;
use crate::input::{InputError, present, type_name};

/// The dispatch gate: `Some(refusal)` when `input` contradicts the tool's
/// advertised schema, `None` to proceed (including for an unknown tool —
/// the unknown-name arm downstream owns that refusal).
pub(super) fn refusal(tool: Option<&dyn Tool>, input: &Value) -> Option<ToolOutput> {
    let schema = tool?.schema();
    check(&schema.input_schema, input).map(ToolOutput::from)
}

/// Validate `input` against `schema` over the enforced subset. Returns the
/// first violation in the module doc's fixed order, or `None` when the input
/// is acceptable (or the schema declares nothing checkable).
fn check(schema: &Value, input: &Value) -> Option<InputError> {
    let properties = schema.get("properties").and_then(Value::as_object);

    // `required`, in the schema's declared order.
    for field in schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if present(input, field).is_none() {
            // An explicit `null` normally reads as absence, but a type that
            // lists "null" declares it a legitimate value.
            let null_allowed = matches!(input.get(field), Some(Value::Null))
                && properties
                    .and_then(|props| props.get(field))
                    .and_then(|prop| prop.get("type"))
                    .is_some_and(type_lists_null);
            if !null_allowed {
                return Some(InputError::Missing {
                    field: field.to_string(),
                });
            }
        }
    }

    // Per-property checks, in the property map's iteration order.
    for (field, prop) in properties.into_iter().flatten() {
        let Some(value) = present(input, field) else {
            continue;
        };
        if let Some(declared) = prop.get("type")
            && !type_allows(declared, value)
        {
            return Some(InputError::WrongType {
                field: field.clone(),
                want: want_phrase(declared),
                got: type_name(value),
            });
        }
        if let Some(members) = prop.get("enum").and_then(Value::as_array)
            && !members.contains(value)
        {
            return Some(InputError::NotOneOf {
                field: field.clone(),
                allowed: join_rendered(members),
                got: render_capped(value),
            });
        }
        if let (Some(elements), Some(item_type)) = (
            value.as_array(),
            prop.get("items").and_then(|items| items.get("type")),
        ) {
            for (index, element) in elements.iter().enumerate() {
                if !type_allows(item_type, element) {
                    return Some(InputError::WrongElementType {
                        field: field.clone(),
                        index,
                        want: want_phrase(item_type),
                        got: type_name(element),
                    });
                }
            }
        }
    }

    // Unknown keys, only where the schema advertises strictness.
    if schema.get("additionalProperties") == Some(&Value::Bool(false))
        && let Some(map) = input.as_object()
    {
        let mut unknown: Vec<&String> = map
            .keys()
            .filter(|key| !properties.is_some_and(|props| props.contains_key(*key)))
            .collect();
        unknown.sort();
        if let Some(field) = unknown.first() {
            let mut allowed: Vec<&String> =
                properties.into_iter().flatten().map(|(k, _)| k).collect();
            allowed.sort();
            let allowed = allowed
                .iter()
                .map(|key| format!("`{key}`"))
                .collect::<Vec<_>>()
                .join(", ");
            return Some(InputError::UnknownField {
                field: (*field).clone(),
                allowed,
            });
        }
    }

    None
}

/// Whether a `"type"` declaration (string or array of strings) admits
/// `value`. A malformed declaration or an unknown type word admits
/// everything — the validator fails open outside the enforced subset.
fn type_allows(declared: &Value, value: &Value) -> bool {
    match declared {
        Value::String(word) => word_allows(word, value),
        Value::Array(words) => words.iter().any(|word| type_allows(word, value)),
        _ => true,
    }
}

/// One JSON-Schema type word against one value. `"integer"` is the number
/// with no fractional part; an integer passes `"number"` but a float does
/// not pass `"integer"`. An unknown word admits everything.
fn word_allows(word: &str, value: &Value) -> bool {
    match word {
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true,
    }
}

/// Whether a `"type"` declaration lists `"null"`.
fn type_lists_null(declared: &Value) -> bool {
    match declared {
        Value::String(word) => word == "null",
        Value::Array(words) => words.iter().any(type_lists_null),
        _ => false,
    }
}

/// The "must be …" phrase for a `"type"` declaration, matching the fixed
/// phrases [`crate::input`]'s readers use ("a string", "an integer") so the
/// two vocabularies cannot drift. Alternatives join with " or "; `"null"`
/// alternatives are elided from the phrase (a present, non-null value is
/// being described).
fn want_phrase(declared: &Value) -> String {
    fn word_phrase(word: &str) -> Option<&'static str> {
        Some(match word {
            "string" => "a string",
            "integer" => "an integer",
            "number" => "a number",
            "boolean" => "a boolean",
            "array" => "an array",
            "object" => "an object",
            "null" => return None,
            _ => return None,
        })
    }
    let phrases: Vec<&'static str> = match declared {
        Value::String(word) => word_phrase(word).into_iter().collect(),
        Value::Array(words) => words
            .iter()
            .filter_map(Value::as_str)
            .filter_map(word_phrase)
            .collect(),
        _ => Vec::new(),
    };
    if phrases.is_empty() {
        // Unreachable when `type_allows` refused: a declaration made only of
        // unknown words (or only "null") admits everything. Kept total.
        "the declared type".to_string()
    } else {
        phrases.join(" or ")
    }
}

/// `enum` members rendered as JSON in declared order, comma-joined.
fn join_rendered(members: &[Value]) -> String {
    members
        .iter()
        .map(render_capped)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A value rendered as compact JSON, capped so a pathological input cannot
/// balloon the refusal. The cap is a fixed constant, so the text stays
/// byte-deterministic for a given input.
fn render_capped(value: &Value) -> String {
    const CAP: usize = 120;
    let rendered = value.to_string();
    if rendered.len() <= CAP {
        return rendered;
    }
    let mut end = CAP;
    while !rendered.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &rendered[..end])
}

#[cfg(test)]
mod tests {
    use super::super::ToolRegistry;
    use super::*;

    fn registry() -> (tempfile::TempDir, ToolRegistry) {
        let root = tempfile::tempdir().unwrap();
        let reg = ToolRegistry::new(root.path().to_path_buf());
        (root, reg)
    }

    fn error_message(output: &ToolOutput) -> &str {
        match output {
            ToolOutput::Error { message, .. } => message,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// The seam witness (#3144): a schema-declared type mismatch is refused
    /// at dispatch with the `WrongType` wording, and the tool never runs —
    /// before the seam, a mistyped field silently defaulted and the tool
    /// executed anyway.
    #[tokio::test]
    async fn a_mistyped_field_is_refused_at_dispatch_and_the_tool_does_not_run() {
        let (_root, reg) = registry();
        let out = reg
            .execute(
                "save_state",
                &serde_json::json!({ "key": "seam_probe", "content": 42 }),
            )
            .await;
        assert_eq!(
            error_message(&out),
            "field `content` must be a string, got number"
        );
        let listed = reg.execute("list_state", &serde_json::json!({})).await;
        match listed {
            ToolOutput::Ok { content } => assert!(
                !content.contains("seam_probe"),
                "the tool must not run on refused input — the entry was saved: {content}"
            ),
            other => panic!("list_state must succeed: {other:?}"),
        }
    }

    /// A present-but-mistyped required field is a type mismatch, never
    /// "missing" — the misreport the whole issue is about.
    #[tokio::test]
    async fn a_mistyped_required_field_is_not_reported_as_missing() {
        let (_root, reg) = registry();
        let out = reg
            .execute(
                "save_state",
                &serde_json::json!({ "key": 42, "content": "x" }),
            )
            .await;
        assert_eq!(
            error_message(&out),
            "field `key` must be a string, got number"
        );
    }

    /// An absent required field keeps the exact wording every tool has
    /// always used, so the seam is invisible where behavior was correct.
    #[tokio::test]
    async fn an_absent_required_field_keeps_the_original_wording() {
        let (_root, reg) = registry();
        let out = reg.execute("get_state", &serde_json::json!({})).await;
        assert_eq!(error_message(&out), "missing required field `key`");
    }

    /// `"type": "integer"` rejects a fractional number; the refusal names
    /// the field, the expectation, and what arrived.
    #[tokio::test]
    async fn a_fractional_number_fails_an_integer_declaration() {
        let (_root, reg) = registry();
        let out = reg
            .execute(
                "get_state",
                &serde_json::json!({ "key": "a", "offset": 2.5 }),
            )
            .await;
        assert_eq!(
            error_message(&out),
            "field `offset` must be an integer, got number"
        );
    }

    /// `additionalProperties: false` — advertised by the `task` tool — now
    /// rejects unknown keys with a deterministic, sorted refusal. On main
    /// the key was silently ignored and the sub-agent spawned anyway.
    #[tokio::test]
    async fn an_unknown_key_is_rejected_only_where_the_schema_forbids_it() {
        let (_root, reg) = registry();
        let out = reg
            .execute(
                "task",
                &serde_json::json!({
                    "description": "probe",
                    "prompt": "p",
                    "zubagent": 1,
                    "agent": "x"
                }),
            )
            .await;
        assert_eq!(
            error_message(&out),
            "unknown field `agent` — this tool accepts only: `description`, `prompt`"
        );
    }

    /// A tool without `additionalProperties: false` keeps ignoring unknown
    /// keys — the documented conservative decision (#3144).
    #[tokio::test]
    async fn an_unknown_key_passes_where_the_schema_is_open() {
        let (_root, reg) = registry();
        let out = reg
            .execute("list_state", &serde_json::json!({ "stray_key": 1 }))
            .await;
        assert!(!out.is_error(), "{out:?}");
    }

    /// Direct subset checks the catalog cannot exercise: type alternatives,
    /// null-as-absence, enum membership, item types, absent declarations.
    #[test]
    fn the_subset_checker_honors_type_arrays_null_and_enums() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "mode": { "type": "string", "enum": ["fast", "full"] },
                "count": { "type": ["integer", "null"] },
                "files": { "type": "array", "items": { "type": "string" } },
                "anything": {}
            },
            "required": ["mode"]
        });

        // Absent type declaration: no check.
        assert_eq!(
            check(&schema, &serde_json::json!({"mode": "fast", "anything": 5})),
            None
        );
        // A type array admits any listed type; a present integer passes.
        assert_eq!(
            check(&schema, &serde_json::json!({"mode": "full", "count": 3})),
            None
        );
        // An optional field sent as null reads as absent — no refusal.
        assert_eq!(
            check(&schema, &serde_json::json!({"mode": "fast", "count": null})),
            None
        );
        // An integer satisfies "number", but 2.5 does not satisfy "integer".
        let err = check(&schema, &serde_json::json!({"mode": "fast", "count": 2.5})).unwrap();
        assert_eq!(
            err.to_string(),
            "field `count` must be an integer, got number"
        );
        // Enum membership, with the members in declared order.
        let err = check(&schema, &serde_json::json!({"mode": "slow"})).unwrap();
        assert_eq!(
            err.to_string(),
            "field `mode` must be one of \"fast\", \"full\", got \"slow\""
        );
        // A wrong-typed enum field is a type mismatch, not an enum miss.
        let err = check(&schema, &serde_json::json!({"mode": 7})).unwrap();
        assert_eq!(err.to_string(), "field `mode` must be a string, got number");
        // items.type names the offending index.
        let err = check(
            &schema,
            &serde_json::json!({"mode": "fast", "files": ["a", 2]}),
        )
        .unwrap();
        assert_eq!(
            err.to_string(),
            "field `files`[1] must be a string, got number"
        );
    }

    /// A required field whose type lists "null" accepts an explicit null;
    /// one whose type does not is refused as missing (the `crate::input`
    /// convention: null reads as absence).
    #[test]
    fn required_null_follows_the_declared_type() {
        let strict = serde_json::json!({
            "properties": { "q": { "type": "string" } },
            "required": ["q"]
        });
        assert_eq!(
            check(&strict, &serde_json::json!({"q": null}))
                .unwrap()
                .to_string(),
            "missing required field `q`"
        );
        let nullable = serde_json::json!({
            "properties": { "q": { "type": ["string", "null"] } },
            "required": ["q"]
        });
        assert_eq!(check(&nullable, &serde_json::json!({"q": null})), None);
    }

    /// A schema with no `properties`/`required` — the common shape for an
    /// external MCP server — checks nothing, so permissive tools stay
    /// permissive through the same seam.
    #[test]
    fn a_bare_schema_checks_nothing() {
        let bare = serde_json::json!({ "type": "object" });
        assert_eq!(check(&bare, &serde_json::json!({"whatever": 1})), None);
        assert_eq!(check(&bare, &serde_json::json!(null)), None);
    }

    /// The refusal must be byte-deterministic: same input, same bytes, and
    /// oversized values are capped rather than echoed whole.
    #[test]
    fn refusals_are_deterministic_and_capped() {
        let schema = serde_json::json!({
            "properties": { "mode": { "type": "string", "enum": ["a"] } }
        });
        let huge = serde_json::json!({ "mode": "x".repeat(500) });
        let first = check(&schema, &huge).unwrap().to_string();
        let second = check(&schema, &huge).unwrap().to_string();
        assert_eq!(first, second);
        assert!(first.len() < 300, "capped, not echoed whole: {first}");
        assert!(first.ends_with('…'));
    }
}
