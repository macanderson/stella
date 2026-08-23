// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! One verb, many targets — the shared shape behind the file tools' batch
//! forms (#4151).
//!
//! # The defect this closes
//!
//! Telemetry from one execution: **24 `sed -n 'A,Bp'` range reads against 2
//! `read_file` calls**, with `read_file` recording zero errors and the very
//! first tool call of the turn already a `sed`. The same turn made 18
//! `edit_file` calls and **zero** `sed -i` — so the model was not avoiding the
//! file tools, and it was not falling back after a failure. It was reaching
//! for the only surface through which it could ask **one question about
//! several files at once**: eleven of its `bash` calls chained two or three
//! reads with `; echo ===;`.
//!
//! The engine already runs consecutive read-only calls concurrently and the
//! system prompt already asks the model to send independent calls together
//! (#2985). That path was not taken: of 71 tool calls in that execution, 69
//! carried distinct timestamps, and the two collisions each pair a real tool
//! with a coordination call. Parallel tool calls are a **provider capability**
//! this workspace declares on no axis and sends on no adapter, so a model that
//! does not volunteer them leaves `bash` as the only way to batch.
//!
//! So the batching lives in the schema, where it holds for every model
//! regardless of what the wire negotiates. That is the whole argument for this
//! module: a working surface must not be built on a provider feature nobody
//! declared (invariant #8).
//!
//! # The shape, and why it is not a second tool
//!
//! Every file tool keeps its singular form exactly as it was and gains one
//! optional array — the plural of the same target. [`targets`] parses the
//! array **with the tool's own single-item parser**, so there is one definition
//! of what a target is and two spellings of how many.
//!
//! This is invariant #9-clean: the array *scopes* the operation to N targets,
//! it never *selects* a different one. Same verb, same policy gate, same audit
//! events, same `read_only` claim. Contrast `update_task(delete=true)`, which
//! is two verbs wearing one schema — nothing here branches on the plural key
//! except the arity of the loop under it.

use serde_json::Value;

use crate::input::{InputError, present, type_name};

/// Ceiling on the targets one call may name.
///
/// Not a performance bound — the read budget and the mutation gates already
/// bound cost. It bounds **blast radius**: a batch is all-or-nothing for the
/// mutating tools, so a malformed 10,000-element array should be refused as a
/// mistake rather than attempted as an instruction. 32 is past anything an
/// honest turn asks for (the observed `bash` chains batch two or three) and far
/// under the point where a single refusal stops being readable.
pub const MAX_BATCH_ITEMS: usize = 32;

/// Why a batch could not be read.
///
/// Typed rather than a `String` because callers branch on these: a
/// [`BatchError::Item`] names an index the model can fix in place, while
/// [`BatchError::Both`] is a whole-call mistake that no per-item retry
/// resolves (invariant #5).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BatchError {
    /// The single form did not parse. Displays as the bare [`InputError`],
    /// byte-identical to what these tools said before a plural key existed —
    /// a caller passing no `path` must be told exactly that, not handed an
    /// array index it never wrote.
    #[error("{0}")]
    Single(#[from] InputError),

    /// The plural key was present but was not a JSON array.
    #[error("field `{field}` must be an array of objects, got {got}")]
    NotAnArray {
        /// The plural key.
        field: &'static str,
        /// The JSON type that arrived.
        got: &'static str,
    },

    /// The plural key was an empty array, which asks for no work at all.
    #[error("field `{field}` is empty — omit it and pass the single-target fields instead")]
    Empty {
        /// The plural key.
        field: &'static str,
    },

    /// More targets than [`MAX_BATCH_ITEMS`].
    #[error(
        "field `{field}` names {count} targets, past the {max}-target ceiling for one call — split it"
    )]
    TooMany {
        /// The plural key.
        field: &'static str,
        /// How many arrived.
        count: usize,
        /// The ceiling.
        max: usize,
    },

    /// One element of the array did not parse. The index is in the type, not
    /// only the prose: "one of these is wrong" is not something a caller can
    /// act on.
    #[error("`{field}`[{index}]: {source}")]
    Item {
        /// The plural key.
        field: &'static str,
        /// The offending element's position.
        index: usize,
        /// Why that element did not parse.
        source: InputError,
    },

    /// Both spellings at once. Refused rather than resolved by precedence:
    /// either choice silently drops work the caller asked for, and a model
    /// that sent both cannot tell which half ran.
    #[error(
        "pass either the single-target fields or `{field}`, not both — they are two spellings of the same call"
    )]
    Both {
        /// The plural key.
        field: &'static str,
    },
}

impl From<BatchError> for stella_protocol::ToolOutput {
    /// A malformed batch is the model's mistake, classified the same way
    /// [`InputError`] is (#3145) so a per-tool error rate can exclude misuse
    /// without matching on prose.
    fn from(err: BatchError) -> Self {
        stella_protocol::ToolOutput::classified_error(
            stella_protocol::ErrorClass::InvalidInput,
            err.to_string(),
        )
    }
}

/// Read a tool's targets, in whichever of the two spellings the caller used.
///
/// `plural` is the array key; `singular_probe` is a field that only the single
/// form carries, used solely to catch a call that sent both. `item` is the
/// tool's own parser for one target — the same function the single form uses,
/// which is what keeps the two spellings from drifting into two meanings.
///
/// Returns targets in the caller's order. Order is required for
/// `edit_file`, where two edits to one file compose, so this never sorts or
/// deduplicates.
pub fn targets<T>(
    input: &Value,
    plural: &'static str,
    singular_probe: &str,
    item: impl Fn(&Value) -> Result<T, InputError>,
) -> Result<Vec<T>, BatchError> {
    let Some(value) = present(input, plural) else {
        // No plural key: the single form, parsed by the very same function.
        return item(input).map(|one| vec![one]).map_err(BatchError::Single);
    };

    if present(input, singular_probe).is_some() {
        return Err(BatchError::Both { field: plural });
    }

    let items = value.as_array().ok_or(BatchError::NotAnArray {
        field: plural,
        got: type_name(value),
    })?;
    if items.is_empty() {
        return Err(BatchError::Empty { field: plural });
    }
    if items.len() > MAX_BATCH_ITEMS {
        return Err(BatchError::TooMany {
            field: plural,
            count: items.len(),
            max: MAX_BATCH_ITEMS,
        });
    }

    items
        .iter()
        .enumerate()
        .map(|(index, raw)| {
            item(raw).map_err(|source| BatchError::Item {
                field: plural,
                index,
                source,
            })
        })
        .collect()
}

/// Whether this call used the plural spelling.
///
/// The tools render a per-target header for the plural form and keep the
/// singular form's output byte-identical to what it has always been. That is
/// deliberate: the footer of a single-file read is compared by the loop
/// detector and asserted byte-for-byte by existing tests, and a header that
/// appeared only above two-or-more targets would make the output shape depend
/// on a count rather than on the call.
pub fn is_plural(input: &Value, plural: &'static str) -> bool {
    present(input, plural).is_some()
}

/// The JSON Schema fragment for one tool's plural key.
///
/// Generated rather than hand-written per tool so the four schemas cannot
/// drift in how they describe the same idea — `docs/tools/*.toml` is
/// regenerated from these and a reviewer reads them side by side.
///
/// # Why the enclosing tool declares `"required": []`
///
/// "Exactly one of the single fields or the plural key" is `oneOf` in JSON
/// Schema, and that is the honest encoding. It is not the one used, because
/// this schema is not consumed by a validator we control: it is sent to every
/// provider, and top-level `oneOf`/`anyOf` in a tool's parameter schema is
/// unevenly supported across them — the failure mode is a provider rejecting
/// the tool declaration outright, which costs the tool rather than the
/// mistake. So the arity requirement is enforced where it can be enforced
/// exactly, in [`targets`], and the schema states it in the description
/// instead. A call carrying neither spelling still fails, with the singular
/// form's original wording (see [`BatchError::Single`]) rather than anything
/// about arrays.
pub fn plural_schema(item_properties: Value, required: &[&str], what: &str) -> Value {
    serde_json::json!({
        "type": "array",
        "description": format!(
            "{what} Use this to act on several targets in ONE call instead of \
             several calls. Omit it and use the single-target fields above for \
             one target; passing both is refused. At most {MAX_BATCH_ITEMS}."
        ),
        "items": {
            "type": "object",
            "properties": item_properties,
            "required": required,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One target parser, used by both spellings — the property the whole
    /// module exists to hold.
    fn path_of(value: &Value) -> Result<String, InputError> {
        crate::input::required_str(value, "path").map(str::to_string)
    }

    #[test]
    fn the_single_form_is_parsed_by_the_same_item_parser() {
        let single = serde_json::json!({"path": "a.rs"});
        assert_eq!(
            targets(&single, "files", "path", path_of).unwrap(),
            vec!["a.rs".to_string()]
        );
    }

    /// The single form's wording is a contract — the model has been reading
    /// these exact strings, and `read_file`'s own tests assert them verbatim.
    /// Adding a plural key must not turn a missing `path` into an array index
    /// the caller never wrote.
    #[test]
    fn a_single_form_miss_keeps_its_original_wording() {
        let err = targets(&serde_json::json!({}), "files", "path", path_of).unwrap_err();
        assert_eq!(err.to_string(), "missing required field `path`");
        assert!(matches!(
            err,
            BatchError::Single(InputError::Missing { .. })
        ));
    }

    #[test]
    fn the_plural_form_keeps_the_callers_order() {
        // Order is required for edit_file, where two edits to one file
        // compose. Sorting or deduplicating here would silently change meaning.
        let many =
            serde_json::json!({"files": [{"path": "b.rs"}, {"path": "a.rs"}, {"path": "b.rs"}]});
        assert_eq!(
            targets(&many, "files", "path", path_of).unwrap(),
            vec!["b.rs".to_string(), "a.rs".to_string(), "b.rs".to_string()]
        );
    }

    /// Sending both spellings is refused, never resolved by precedence: either
    /// choice drops work the caller asked for, and the model cannot tell which
    /// half ran.
    #[test]
    fn both_spellings_at_once_is_refused() {
        let both = serde_json::json!({"path": "a.rs", "files": [{"path": "b.rs"}]});
        assert_eq!(
            targets(&both, "files", "path", path_of).unwrap_err(),
            BatchError::Both { field: "files" }
        );
    }

    #[test]
    fn a_bad_element_is_named_by_index_and_carries_the_inner_cause() {
        let bad = serde_json::json!({"files": [{"path": "a.rs"}, {"nope": 1}]});
        let err = targets(&bad, "files", "path", path_of).unwrap_err();
        assert!(
            matches!(
                err,
                BatchError::Item {
                    index: 1,
                    source: InputError::Missing { .. },
                    ..
                }
            ),
            "the index and the cause both belong in the type: {err:?}"
        );
        assert!(err.to_string().contains("`files`[1]"), "got {err}");
    }

    #[test]
    fn an_empty_or_oversized_or_mistyped_array_is_refused() {
        let empty = serde_json::json!({"files": []});
        assert_eq!(
            targets(&empty, "files", "path", path_of).unwrap_err(),
            BatchError::Empty { field: "files" }
        );

        let scalar = serde_json::json!({"files": "a.rs"});
        assert!(matches!(
            targets(&scalar, "files", "path", path_of).unwrap_err(),
            BatchError::NotAnArray { got: "string", .. }
        ));

        let huge: Vec<Value> = (0..MAX_BATCH_ITEMS + 1)
            .map(|i| serde_json::json!({"path": format!("f{i}.rs")}))
            .collect();
        assert!(matches!(
            targets(&serde_json::json!({"files": huge}), "files", "path", path_of).unwrap_err(),
            BatchError::TooMany { count, .. } if count == MAX_BATCH_ITEMS + 1
        ));

        // The ceiling is inclusive — exactly MAX_BATCH_ITEMS is allowed.
        let at_limit: Vec<Value> = (0..MAX_BATCH_ITEMS)
            .map(|i| serde_json::json!({"path": format!("f{i}.rs")}))
            .collect();
        assert_eq!(
            targets(
                &serde_json::json!({"files": at_limit}),
                "files",
                "path",
                path_of
            )
            .unwrap()
            .len(),
            MAX_BATCH_ITEMS
        );
    }

    /// A malformed batch is the model's mistake, not the tool's — the same
    /// class `InputError` carries, so per-tool error rates stay comparable.
    #[test]
    fn a_batch_error_is_classified_as_invalid_input() {
        let out: stella_protocol::ToolOutput = BatchError::Empty { field: "files" }.into();
        let stella_protocol::ToolOutput::Error { class, .. } = out else {
            panic!("expected an error");
        };
        assert_eq!(class, Some(stella_protocol::ErrorClass::InvalidInput));
    }
}
