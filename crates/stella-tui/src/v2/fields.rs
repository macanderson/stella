// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A JSON object, read as fields rather than shown as syntax.
//!
//! A tool's arguments and a tool's structured result both travel as JSON, and
//! the transcript used to print them as JSON: a `task_create` head read
//! `{"tasks":[{"subject":"wire the digest","description":"..."}]}` on one
//! line, and a `get_state` result opened on a brace. A person does not read
//! braces. They read *what the call said* — the subject, the path, the count —
//! and the punctuation is what the wire needed, not what they do.
//!
//! Two projections, one vocabulary:
//!
//! * [`headline`] — the one line a call's head states beside the verb: the
//!   field the reader most wants, or a `key value · key value` run when no
//!   field stands out.
//! * [`rows`] — the whole object as a table: one field per row, `key` muted
//!   and `value` in the text tone, nested objects indented, arrays as one row
//!   per item. No brace, quote or comma survives.
//!
//! Pure over a `serde_json::Value`; nothing here knows which tool produced
//! the object. The *shape* keys (`tasks`, `edits`, `subject`, `path`) are
//! preferences for what to lead with, never a requirement — an object with
//! none of them still renders every field it has.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde_json::Value;
use stella_tui_theme::token;

/// The keys a head leads with, most wanted first.
///
/// A subject or a title is what a person named the thing; a path or a command
/// is what it acts on; a query is what it looks for. `description` and
/// `content` come last because they are long, and a head is one line.
const LEAD_KEYS: [&str; 22] = [
    "subject",
    "title",
    "name",
    "path",
    "file_path",
    "command",
    "cmd",
    "query",
    "pattern",
    "symbol",
    "key",
    "id",
    "task_id",
    "question",
    "prompt",
    "briefing",
    "message",
    "text",
    "reason",
    "url",
    "description",
    "content",
];

/// The keys a table lists after the lead keys, in reading order: the two
/// halves of an edit in the order a diff reads them, then a task's state.
/// Everything else follows alphabetically — `serde_json` hands the object
/// over sorted, so wire order is not available to keep.
const FOLLOW_KEYS: [&str; 6] = [
    "old_string",
    "new_string",
    "status",
    "owner",
    "edits",
    "tasks",
];

/// The reading order of an object's fields: lead keys, follow keys, then the
/// rest alphabetically.
fn ordered(map: &serde_json::Map<String, Value>) -> Vec<(&String, &Value)> {
    let rank = |k: &str| {
        LEAD_KEYS
            .iter()
            .position(|lead| *lead == k)
            .or_else(|| {
                FOLLOW_KEYS
                    .iter()
                    .position(|f| *f == k)
                    .map(|i| LEAD_KEYS.len() + i)
            })
            .unwrap_or(usize::MAX)
    };
    let mut fields: Vec<(&String, &Value)> = map.iter().collect();
    fields.sort_by(|(a, _), (b, _)| rank(a).cmp(&rank(b)).then_with(|| a.cmp(b)));
    fields
}

/// Most characters a headline spends before it is cut.
const HEADLINE_CAP: usize = 96;
/// Most characters one table value spends on its row before folding to the
/// next.
const VALUE_WRAP: usize = 120;
/// Cells one nesting level indents a table row by.
const INDENT: usize = 2;

/// The one line a head states for `value`.
///
/// An object leads with its most wanted field ([`LEAD_KEYS`]); an array, or
/// an object whose only interesting member is an array, states the count and
/// the first item's headline (`3 tasks · wire the digest, …`); a scalar is
/// itself. An empty object is the empty string, so a caller can elide the
/// column — `get_environment {}` read as an empty *result* (#4157's sibling).
#[must_use]
pub fn headline(value: &Value) -> String {
    let text = match value {
        Value::Null => String::new(),
        Value::Object(map) if map.is_empty() => String::new(),
        Value::Object(map) => {
            if let Some(lead) = LEAD_KEYS.iter().find_map(|k| map.get(*k).map(|v| (k, v))) {
                let (key, v) = lead;
                match v {
                    Value::String(s) => one_line(s),
                    other => format!("{key} {}", scalar_or_count(other)),
                }
            } else if let Some((key, items)) = map
                .iter()
                .find_map(|(k, v)| v.as_array().map(|items| (k, items)))
            {
                array_headline(key, items)
            } else {
                ordered(map)
                    .into_iter()
                    .map(|(k, v)| format!("{k} {}", scalar_or_count(v)))
                    .collect::<Vec<_>>()
                    .join(" · ")
            }
        }
        Value::Array(items) => array_headline("items", items),
        scalar => scalar_text(scalar),
    };
    cap(&text, HEADLINE_CAP)
}

/// `3 tasks · first, second, third` — a count, then the items' own headlines.
fn array_headline(key: &str, items: &[Value]) -> String {
    let n = items.len();
    let noun = if n == 1 {
        key.strip_suffix('s').unwrap_or(key).to_string()
    } else {
        key.to_string()
    };
    let names: Vec<String> = items
        .iter()
        .take(3)
        .map(headline)
        .filter(|s| !s.is_empty())
        .collect();
    if names.is_empty() {
        format!("{n} {noun}")
    } else {
        format!("{n} {noun} · {}", names.join(", "))
    }
}

/// A scalar as the reader would say it; a container as its size.
fn scalar_or_count(value: &Value) -> String {
    match value {
        Value::Array(items) => format!("{} items", items.len()),
        Value::Object(map) => format!("{} fields", map.len()),
        scalar => scalar_text(scalar),
    }
}

/// The plain word for a scalar: strings unquoted, `true`/`false`, `null` as
/// `none`, numbers as written.
fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(s) => one_line(s),
        Value::Bool(b) => b.to_string(),
        Value::Null => "none".into(),
        Value::Number(n) => n.to_string(),
        // Containers are never asked for their scalar text — the callers
        // above branch on them first — so this is the honest degradation
        // rather than a reachable arm.
        Value::Array(_) | Value::Object(_) => scalar_or_count(value),
    }
}

/// Newlines become spaces, runs of blanks collapse.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Cut to `max` characters with an ellipsis.
fn cap(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

/// The whole object as a field table.
///
/// One field per row: `key` in the muted tone, a space, the value in the text
/// tone. A nested object indents its fields one level under the key's row; an
/// array renders `key · n items`, then one indented row per item, objects
/// among them as their own indented tables. A long string value folds onto
/// continuation rows rather than running off the pane.
///
/// Rows carry no rail: the caller owns the margin, exactly as it does for a
/// painted body line.
#[must_use]
pub fn rows(value: &Value) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    push_value(value, 0, None, &mut out);
    out
}

/// One field (`key`, or none for a bare array item) at `depth`.
fn push_value(value: &Value, depth: usize, key: Option<&str>, out: &mut Vec<Line<'static>>) {
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);
    let dim = Style::new().fg(token::DIM);
    let pad = " ".repeat(depth * INDENT);
    match value {
        Value::Object(map) => {
            if let Some(key) = key {
                out.push(Line::from(vec![
                    Span::raw(pad.clone()),
                    Span::styled(key.to_string(), muted),
                    Span::styled(
                        if map.is_empty() { " · no fields" } else { "" }.to_string(),
                        dim,
                    ),
                ]));
            }
            let inner = if key.is_some() { depth + 1 } else { depth };
            for (k, v) in ordered(map) {
                push_value(v, inner, Some(k), out);
            }
        }
        Value::Array(items) => {
            if let Some(key) = key {
                out.push(Line::from(vec![
                    Span::raw(pad.clone()),
                    Span::styled(key.to_string(), muted),
                    Span::styled(
                        format!(
                            " · {} {}",
                            items.len(),
                            if items.len() == 1 { "item" } else { "items" }
                        ),
                        dim,
                    ),
                ]));
            }
            let inner = if key.is_some() { depth + 1 } else { depth };
            for item in items {
                match item {
                    Value::Object(_) | Value::Array(_) => {
                        out.push(Line::from(vec![
                            Span::raw(" ".repeat(inner * INDENT)),
                            Span::styled("-", dim),
                        ]));
                        push_value(item, inner + 1, None, out);
                    }
                    scalar => out.push(Line::from(vec![
                        Span::raw(" ".repeat(inner * INDENT)),
                        Span::styled("- ", dim),
                        Span::styled(scalar_text(scalar), text),
                    ])),
                }
            }
        }
        scalar => {
            let word = scalar_text(scalar);
            let mut spans = vec![Span::raw(pad.clone())];
            if let Some(key) = key {
                spans.push(Span::styled(key.to_string(), muted));
                spans.push(Span::raw(" "));
            }
            // A string with its own line breaks keeps them: a briefing or a
            // commit message is written in paragraphs, and flattening it to
            // one row would be the JSON wire's habit, not the reader's.
            let raw = match scalar {
                Value::String(s) => s.clone(),
                _ => word,
            };
            let mut first = true;
            for line in raw.lines().flat_map(|l| wrap(l, VALUE_WRAP)) {
                if first {
                    spans.push(Span::styled(line, text));
                    out.push(Line::from(std::mem::take(&mut spans)));
                    first = false;
                } else {
                    out.push(Line::from(vec![
                        Span::raw(pad.clone()),
                        Span::raw(" ".repeat(key.map_or(0, |k| k.chars().count() + 1))),
                        Span::styled(line, text),
                    ]));
                }
            }
            if first {
                // An empty string still owns its row, or the field vanishes.
                out.push(Line::from(spans));
            }
        }
    }
}

/// Word-wrap one line to `width` characters.
fn wrap(line: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let mut current = String::new();
    for word in line.split(' ') {
        if !current.is_empty() && current.chars().count() + 1 + word.chars().count() > width {
            rows.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    rows.push(current);
    rows
}

/// `value` if `text` is a JSON document — an object or an array, not a bare
/// scalar, which a tool result that happens to be the word `true` should not
/// be re-read as.
#[must_use]
pub fn parse_document(text: &str) -> Option<Value> {
    let trimmed = text.trim_start();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return None;
    }
    serde_json::from_str::<Value>(text)
        .ok()
        .filter(|v| v.is_object() || v.is_array())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text_of(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// **The witness.** A `task_create` call's head names the tasks, not the
    /// wire.
    #[test]
    fn a_task_create_head_names_its_tasks_not_its_braces() {
        let input = json!({"tasks": [
            {"subject": "read seen-set write path", "description": "long prose"},
            {"subject": "persist digest set", "description": "more prose"},
        ]});
        let head = headline(&input);
        assert_eq!(
            head,
            "2 tasks · read seen-set write path, persist digest set"
        );
        for forbidden in ['{', '}', '"', ':'] {
            assert!(!head.contains(forbidden), "{forbidden:?} leaked: {head}");
        }
    }

    /// An edit's arguments read as a diff does: the path, then the old text,
    /// then the new.
    #[test]
    fn an_edits_fields_read_in_diff_order() {
        let value = json!({"new_string": "bravo", "old_string": "alpha", "path": "src/lib.rs"});
        let text = text_of(&rows(&value));
        assert_eq!(text, "path src/lib.rs\nold_string alpha\nnew_string bravo");
    }

    #[test]
    fn a_head_leads_with_the_field_a_person_named() {
        assert_eq!(headline(&json!({"id": "3", "reason": "superseded"})), "3");
        assert_eq!(
            headline(&json!({"key": "verify", "value": {"flip": true}})),
            "verify"
        );
        assert_eq!(
            headline(&json!({"count": 3, "dry_run": true})),
            "count 3 · dry_run true"
        );
        assert_eq!(headline(&json!({})), "");
        assert_eq!(headline(&json!(null)), "");
    }

    /// The table: one field per row, nested objects indented, arrays itemised,
    /// and not one byte of JSON punctuation.
    #[test]
    fn a_result_renders_as_fields_not_as_json() {
        let value = json!({
            "flip": true,
            "witness": "tests/overfull.rs",
            "attempts": 3,
            "notes": null,
            "steps": ["read", "edit"],
            "cost": {"usd": 0.04, "tokens": 1200}
        });
        let text = text_of(&rows(&value));
        // Lead keys first, the rest alphabetically — `serde_json` hands the
        // object over sorted, so wire order is not a thing this can keep.
        assert_eq!(
            text,
            "attempts 3\n\
             cost\n\
             \x20 tokens 1200\n\
             \x20 usd 0.04\n\
             flip true\n\
             notes none\n\
             steps · 2 items\n\
             \x20 - read\n\
             \x20 - edit\n\
             witness tests/overfull.rs"
        );
        for forbidden in ['{', '}', '"', ','] {
            assert!(!text.contains(forbidden), "{forbidden:?} leaked:\n{text}");
        }
    }

    #[test]
    fn an_array_of_objects_itemises_each_as_its_own_table() {
        let value = json!({"tasks": [{"subject": "a", "status": "pending"}]});
        let text = text_of(&rows(&value));
        assert_eq!(
            text,
            "tasks · 1 item\n\
             \x20 -\n\
             \x20   subject a\n\
             \x20   status pending"
        );
    }

    #[test]
    fn a_multiline_string_keeps_its_paragraphs_under_its_key() {
        let value = json!({"briefing": "first line\nsecond line"});
        let text = text_of(&rows(&value));
        assert_eq!(text, "briefing first line\n         second line");
    }

    #[test]
    fn only_a_document_is_read_as_one() {
        assert!(parse_document("{\"a\": 1}").is_some());
        assert!(parse_document("  [1, 2]").is_some());
        assert!(parse_document("true").is_none());
        assert!(
            parse_document("1│{\n2│}").is_none(),
            "a gutter'd read is source, not a document"
        );
        assert!(parse_document("error: {unexpected}").is_none());
    }
}
