// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Typed reads of a tool's JSON input (#1267).
//!
//! # The defect this replaces
//!
//! Every tool used to destructure its own input, and the idiom they all
//! reached for cannot tell "absent" from "present but wrong type":
//!
//! ```ignore
//! let pattern = match input.get("pattern").and_then(|v| v.as_str()) {
//!     Some(p) => p,
//!     None => return ToolOutput::Error {
//!         message: "missing required field `pattern`".into(),
//!     },
//! };
//! ```
//!
//! `and_then` collapses both failures into one `None`, so a call passing
//! `{"pattern": 42}` is told the field is **missing**. It is not missing — it
//! is a number. The model then re-sends the same wrong-typed value, because
//! the error described a different mistake than the one it made, and the turn
//! spends a step per repetition.
//!
//! # What replaces it
//!
//! One reader per (shape, optionality), each returning a message that names
//! the expected type *and the one that arrived*:
//!
//! - absent → `missing required field \`pattern\`` — deliberately byte-identical
//!   to the old wording, so the migration changes only the case that was wrong;
//! - wrong type → ``field `pattern` must be a string, got number``.
//!
//! The optional readers are the interesting half. `input.get(f).and_then(as_str)
//! .unwrap_or(default)` silently swallows a wrong type into the default, which
//! is the same defect wearing a hat: the caller believes it set the field. These
//! return `Result<Option<T>>` so a wrong type is an error while a genuine
//! absence is `None`.
//!
//! JSON's number tower is not a type ladder, so `u64` accepts any non-negative
//! integer and rejects a float or a negative rather than truncating one — a
//! silently truncated `2.7` is how an off-by-one reaches production.

use serde_json::Value;

/// The JSON type name of `value`, for an error a model can act on. These are
/// the JSON spelling (`number`, `boolean`) rather than Rust's, because the
/// caller wrote JSON.
fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// `null` is treated as absence, not as a wrong type: every JSON encoder in
/// the wild emits `null` for "no value", and failing a call over the
/// difference would be pedantry the caller cannot act on.
fn present<'a>(input: &'a Value, field: &str) -> Option<&'a Value> {
    match input.get(field) {
        None | Some(Value::Null) => None,
        Some(v) => Some(v),
    }
}

fn missing(field: &str) -> String {
    format!("missing required field `{field}`")
}

fn wrong_type(field: &str, want: &str, got: &Value) -> String {
    format!("field `{field}` must be {want}, got {}", type_name(got))
}

/// A required string field.
pub fn required_str<'a>(input: &'a Value, field: &str) -> Result<&'a str, String> {
    let value = present(input, field).ok_or_else(|| missing(field))?;
    value
        .as_str()
        .ok_or_else(|| wrong_type(field, "a string", value))
}

/// An optional string field. `Ok(None)` means absent; a wrong type is an
/// error rather than a silent fallback to the caller's default.
pub fn optional_str<'a>(input: &'a Value, field: &str) -> Result<Option<&'a str>, String> {
    match present(input, field) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| wrong_type(field, "a string", value)),
    }
}

/// A required non-negative integer field.
pub fn required_u64(input: &Value, field: &str) -> Result<u64, String> {
    let value = present(input, field).ok_or_else(|| missing(field))?;
    value
        .as_u64()
        .ok_or_else(|| wrong_type(field, "a non-negative integer", value))
}

/// An optional non-negative integer field.
pub fn optional_u64(input: &Value, field: &str) -> Result<Option<u64>, String> {
    match present(input, field) {
        None => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| wrong_type(field, "a non-negative integer", value)),
    }
}

/// An optional boolean field.
pub fn optional_bool(input: &Value, field: &str) -> Result<Option<bool>, String> {
    match present(input, field) {
        None => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| wrong_type(field, "a boolean", value)),
    }
}

/// An optional array of strings — the common "list of paths / names" shape.
/// A non-string element is named by index, because "one of these is wrong" is
/// not something a caller can act on.
pub fn optional_str_array<'a>(
    input: &'a Value,
    field: &str,
) -> Result<Option<Vec<&'a str>>, String> {
    let Some(value) = present(input, field) else {
        return Ok(None);
    };
    let items = value
        .as_array()
        .ok_or_else(|| wrong_type(field, "an array of strings", value))?;
    let mut out = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let text = item.as_str().ok_or_else(|| {
            format!(
                "field `{field}`[{i}] must be a string, got {}",
                type_name(item)
            )
        })?;
        out.push(text);
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of #1267: the two failures must not report as one.
    #[test]
    fn a_wrong_type_is_not_reported_as_missing() {
        let input = serde_json::json!({"pattern": 42});
        let err = required_str(&input, "pattern").unwrap_err();
        assert!(
            err.contains("must be a string") && err.contains("got number"),
            "must name expected AND actual type: {err}"
        );
        assert!(
            !err.contains("missing"),
            "a present-but-wrong-typed field is not missing: {err}"
        );
    }

    /// The absent wording is byte-identical to what every tool said before, so
    /// the migration changes only the case that was reporting wrongly.
    #[test]
    fn an_absent_field_keeps_the_original_wording() {
        let input = serde_json::json!({});
        assert_eq!(
            required_str(&input, "pattern").unwrap_err(),
            "missing required field `pattern`"
        );
    }

    /// `unwrap_or(default)` on a wrong type is the same defect wearing a hat —
    /// the caller believes it set the field.
    #[test]
    fn an_optional_field_rejects_a_wrong_type_instead_of_defaulting() {
        let input = serde_json::json!({"path": 7});
        assert!(optional_str(&input, "path").is_err());
        assert_eq!(optional_str(&serde_json::json!({}), "path").unwrap(), None);
    }

    /// Every JSON encoder emits `null` for "no value"; failing over it would
    /// be pedantry the caller cannot act on.
    #[test]
    fn an_explicit_null_reads_as_absent() {
        let input = serde_json::json!({"path": null});
        assert_eq!(optional_str(&input, "path").unwrap(), None);
        assert_eq!(
            required_str(&input, "path").unwrap_err(),
            "missing required field `path`"
        );
    }

    /// A silently truncated `2.7` is how an off-by-one reaches production.
    #[test]
    fn a_float_or_negative_is_refused_rather_than_truncated() {
        assert!(required_u64(&serde_json::json!({"n": 2.7}), "n").is_err());
        assert!(required_u64(&serde_json::json!({"n": -1}), "n").is_err());
        assert_eq!(required_u64(&serde_json::json!({"n": 3}), "n").unwrap(), 3);
    }

    #[test]
    fn a_bad_array_element_is_named_by_index() {
        let input = serde_json::json!({"paths": ["a", 2, "c"]});
        let err = optional_str_array(&input, "paths").unwrap_err();
        assert!(err.contains("`paths`[1]"), "got {err}");

        let good = serde_json::json!({"paths": ["a"]});
        assert_eq!(optional_str_array(&good, "paths").unwrap(), Some(vec!["a"]));
        let scalar = serde_json::json!({"paths": "a"});
        assert!(optional_str_array(&scalar, "paths").is_err());
    }
}
