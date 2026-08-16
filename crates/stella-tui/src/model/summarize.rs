//! Text and JSON shaping for a transcript line: the pure formatting the fold
//! calls and holds no state for.
//!
//! # The seam
//!
//! Split out of `model.rs` when that file reached 1500 of the guard's 1500
//! lines with no baseline entry (#2958), so the next addition to it would have
//! failed the gate outright. The line count is the trigger, not the reason.
//!
//! The reason is that these two halves grow on different clocks.
//! [`super::SessionModel::apply`] matches `AgentEvent` exhaustively and is one
//! of the four compile-enforced stops when a variant is added
//! (`stella_protocol::event::tags`), so **every** new event costs `model.rs` at
//! least a line — the arm cannot be declined. Nothing here has a reason to grow
//! when the vocabulary does: these are functions over borrowed strings and
//! `serde_json::Value`, with no knowledge of events at all. Leaving them in one
//! file meant the vocabulary's growth was paying rent on the formatter's
//! length, and the ratchet was about to make an ordinary protocol change
//! impossible.
//!
//! The precedents in this crate are the same move for the same reason:
//! `deck/classify.rs` (extracted when #2007 pushed `deck.rs` against the
//! ceiling) and `wire_contract/samples.rs` over in `stella-protocol`. As in
//! both, this is a **pure move** — every function is byte-identical to the one
//! it replaced apart from the `pub(super)` a sibling module needs, and
//! [`super`] re-imports them so no call site changed.
//!
//! Splitting buys structure, not slack: `make file-size-update` retightens
//! every ceiling to its file's current size, so the room this makes is room for
//! the fold to keep absorbing variants, not a budget to spend on anything else.

use super::SUMMARY_BUDGET;

/// Compact a tool-call input `Value` to a single-line JSON string. Falls back
/// to the empty string on the (impossible for `Value`) serialization error so
/// the model never panics on a tool card.
pub(super) fn compact_json(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// Char budget for a tool call's retained compact-JSON arguments.
pub(crate) const INPUT_BUDGET: usize = 4_096;
/// Char budget for a tool result's retained output (outputs are already
/// capped upstream by the tools; this bounds transcript memory).
pub(crate) const OUTPUT_BUDGET: usize = 16_384;

/// Middle-out char cap preserving head and tail (first error + final summary
/// both matter), on char boundaries.
pub(super) fn cap_middle(text: &str, budget: usize) -> String {
    cap_middle_with(text, budget, "\n[… truncated …]\n")
}

/// [`cap_middle`] with a caller-chosen elision marker. Slices at
/// `char_indices` boundaries instead of materializing a `Vec<char>`, so a
/// multi-megabyte payload costs no allocation beyond the capped result.
pub(super) fn cap_middle_with(text: &str, budget: usize, marker: &str) -> String {
    // Byte length bounds char count, so an in-budget payload returns without
    // scanning; an over-budget one probes just past the boundary instead.
    if text.len() <= budget || text.char_indices().nth(budget).is_none() {
        return text.to_string();
    }
    let keep = budget.saturating_sub(marker.chars().count());
    let head = keep / 2;
    let tail = keep - head;
    let head_end = text.char_indices().nth(head).map_or(text.len(), |(i, _)| i);
    let tail_start = if tail == 0 {
        text.len()
    } else {
        text.char_indices().nth_back(tail - 1).map_or(0, |(i, _)| i)
    };
    format!("{}{marker}{}", &text[..head_end], &text[tail_start..])
}

/// Per-leaf caps for [`cap_input_json`]: generous enough to keep any one
/// argument readable, small enough that leaf capping alone usually lands the
/// whole object under [`INPUT_BUDGET`].
pub(super) const INPUT_STR_CAP: usize = 512;
pub(super) const INPUT_ARR_CAP: usize = 32;

/// Cap a tool call's retained arguments **inside** the JSON: long string
/// leaves are middle-capped and oversized arrays elided, so the compact form
/// stays *valid* JSON and ctrl+o can still pretty-print it. Only a
/// pathological object that remains oversized after leaf capping falls back
/// to the raw char cap (which the renderer shows as wrapped plain text).
pub(super) fn cap_input_json(value: &serde_json::Value, budget: usize) -> String {
    let compact = compact_json(value);
    if compact.len() <= budget {
        return compact;
    }
    let mut capped = value.clone();
    cap_json_leaves(&mut capped);
    cap_middle(&compact_json(&capped), budget)
}

/// Recursively shrink the leaves of `value` in place (strings middle-capped
/// on one line, arrays truncated with a `+N more` marker) without disturbing
/// the object structure.
pub(super) fn cap_json_leaves(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) => {
            if s.len() > INPUT_STR_CAP {
                *s = cap_middle_with(s, INPUT_STR_CAP, " […] ");
            }
        }
        serde_json::Value::Array(items) => {
            if items.len() > INPUT_ARR_CAP {
                let dropped = items.len() - INPUT_ARR_CAP;
                items.truncate(INPUT_ARR_CAP);
                items.push(serde_json::Value::String(format!("[… +{dropped} more …]")));
            }
            for item in items.iter_mut() {
                cap_json_leaves(item);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values_mut() {
                cap_json_leaves(item);
            }
        }
        _ => {}
    }
}

/// Format a tool-call input as a human-readable one-liner. Instead of raw
/// JSON, this extracts the most relevant field(s) by input shape so the
/// transcript reads naturally — `path` for file tools, `cmd` for shell, the
/// query for search tools, and so on — whatever the tool's name or origin.
pub(super) fn format_tool_input(input: &serde_json::Value) -> String {
    let str_field = |key: &str| -> Option<String> {
        input
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };

    // A batch write carries its paths inside an `edits` array (the
    // conventional `apply_edits` shape); surface them instead of the
    // raw-JSON fallback so the transcript row reads like other file tools.
    // Shape-keyed, not name-keyed, so any custom or MCP tool using the
    // batch convention renders the same way.
    if let Some(edits) = input.get("edits").and_then(|v| v.as_array()) {
        let mut paths: Vec<&str> = Vec::new();
        for edit in edits {
            if let Some(p) = edit.get("path").and_then(|v| v.as_str())
                && !paths.contains(&p)
            {
                paths.push(p);
            }
        }
        if !paths.is_empty() {
            let labels: Vec<String> = paths.iter().map(|p| truncate_field(p, 48)).collect();
            return if paths.len() == 1 {
                labels.into_iter().next().unwrap()
            } else {
                format!("{}  ({} files)", labels.join(", "), paths.len())
            };
        }
    }

    // Primary field first — the one the user cares about at a glance. A call
    // carrying a whole-file `content` beside its path (the conventional
    // write shape) also reports the payload's size; the path alone says
    // nothing about how much landed.
    if let Some(p) = str_field("path").or_else(|| str_field("file_path")) {
        return match str_field("content") {
            Some(content) => format!("{p}  ({} lines)", content.lines().count()),
            None => p,
        };
    }

    if let Some(cmd) = str_field("cmd").or_else(|| str_field("command")) {
        return truncate_field(&cmd, 120);
    }

    if let Some(query) = str_field("query")
        .or_else(|| str_field("pattern"))
        .or_else(|| str_field("symbol"))
    {
        return truncate_field(&query, 80);
    }

    if let Some(prompt) = str_field("question").or_else(|| str_field("prompt")) {
        return truncate_field(&prompt, 80);
    }

    // An argument-less call has nothing to summarize. `compact_json` renders
    // `{}` for it, which the transcript then printed beside the tool name —
    // and an empty object next to `get_environment` reads as an empty
    // *result*, not as "this tool takes no arguments". Both this and the
    // renderer treat the empty string as "no argument column"; saying it here
    // as well is what keeps every consumer of `input` (the trace tab, the
    // accessible renderer) agreeing about it.
    if input.as_object().is_some_and(serde_json::Map::is_empty) || input.is_null() {
        return String::new();
    }
    // Fallback: compact JSON, summarized.
    summarize(&compact_json(input))
}

/// Truncate a field value to `max` chars with an ellipsis. Cuts *before*
/// flattening: the newline→space replacement is one char in, one char out, so
/// slicing the raw text first yields the identical result without copying a
/// multi-megabyte `content`/`old_string` argument in full (the same reason
/// [`cap_middle_with`] walks `char_indices` instead of materializing chars).
pub(super) fn truncate_field(s: &str, max: usize) -> String {
    // `nth(max)` is `None` exactly when the text is `max` chars or shorter.
    if s.char_indices().nth(max).is_none() {
        return s.replace(['\n', '\r'], " ");
    }
    let head_end = s
        .char_indices()
        .nth(max.saturating_sub(1))
        .map_or(s.len(), |(i, _)| i);
    format!("{}…", s[..head_end].replace(['\n', '\r'], " "))
}

/// The workspace-relative path a file tool targets. Conventionally-shaped
/// file tools take their path under the `path` key, and a `FileChange` event
/// carries that same path — so this is the join key between a tool result
/// and its diff.
pub(super) fn tool_input_path(input: &serde_json::Value) -> Option<String> {
    if let Some(path) = input
        .get("path")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
    {
        return Some(path);
    }
    // A batch write carries its paths in an `edits` array, not at the top
    // level; the first edit's path stands in so a single-file batch still
    // renders an inline diff under its result row.
    input
        .get("edits")
        .and_then(serde_json::Value::as_array)
        .and_then(|edits| edits.first())
        .and_then(|e| e.get("path"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// Whether a tool name is one of the conventional file-*mutating* names —
/// the only tools whose result should carry an inline diff (reads must
/// not). No built-in carries these names; a custom tool that adopts one
/// gets its `FileChange` diff rendered inline by construction.
pub(super) fn is_file_mutation(name: &str) -> bool {
    matches!(
        name,
        "write_file" | "edit_file" | "apply_edits" | "delete_file"
    )
}

/// Truncate a summary to [`SUMMARY_BUDGET`] chars with a middle-out elision —
/// the head and tail both matter for a failing tool result (L-S3), so we keep
/// both rather than head-truncating away the error tail.
///
/// Caps *before* flattening. `text` here is a raw tool payload that can be
/// megabytes (the caller also passes it to [`cap_middle`]); replacing newlines
/// first would copy the whole thing just to throw all but 200 chars away, and
/// the replacement is one char in / one char out, so the two orders agree.
pub(super) fn summarize(text: &str) -> String {
    cap_middle_with(text, SUMMARY_BUDGET, "...").replace(['\n', '\r'], " ")
}
