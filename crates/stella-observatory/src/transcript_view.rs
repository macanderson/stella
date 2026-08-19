// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! One execution's journal, folded into the shared transcript model and
//! rendered by [`stella_transcript`].
//!
//! The journal is a flat event stream — `tool_start` and `tool_result` are
//! separate rows correlated by `call_id`, and the page's own renderer therefore
//! drew them as separate blocks with the tool's name and arguments repeated in
//! each. This module is where that stream becomes a tree, and it is the only
//! place the correlation happens: once a row pair has become a
//! [`stella_transcript::Call`], there is no longer an API that can render the
//! call without its result.
//!
//! # What the journal cannot tell us
//!
//! An `edit_file` result body is the tool's prose confirmation, not a diff, and
//! the store holds no pre-image of the file. What it does hold is the call's own
//! arguments — `path`, `old_string`, `new_string` — so the diff rendered here is
//! of **the replaced fragment**, not of the whole file, and its line numbers are
//! fragment-relative. That is a real limitation rather than something papered
//! over; #3577 tracks persisting the pre-image so the diff can be exact.

use serde_json::Value;
use stella_transcript::model::{
    Accounting, ArgRow, Call, FileChange, FileStatus, Output, Prose, Run, Status, Step, ToolKind,
    Turn,
};

/// Fold an execution's head row and journal rows into a renderable run.
///
/// Tolerant by construction: a row this binary does not understand is skipped
/// rather than fatal, and a `tool_result` with no matching `tool_start` is
/// dropped rather than rendered as an anonymous orphan. A transcript that
/// blanks because one row was written by a newer binary is worse than a
/// transcript missing one row.
#[must_use]
pub(crate) fn build_run(execution: &Value, journal: &[Value]) -> Run {
    let mut turn = Turn {
        name: execution["kind"].as_str().unwrap_or("turn").to_string(),
        prompt: execution["prompt"].as_str().unwrap_or_default().to_string(),
        prose: Vec::new(),
        notes: vec![],
        steps: Vec::new(),
        answer: None,
        status: outcome_status(execution),
        duration_ms: 0,
    };

    // `tool_start` is held until its `tool_result` arrives, because a Call is
    // only complete once both halves are known.
    let mut pending: Option<(String, Call)> = None;
    let base_ts = journal.first().and_then(|r| r["ts"].as_i64()).unwrap_or(0);

    for row in journal {
        let ty = row["type"].as_str().unwrap_or_default();
        let offset_ms = row["ts"]
            .as_i64()
            .map_or(0, |ts| u64::try_from(ts - base_ts).unwrap_or(0));
        match ty {
            "reasoning" => turn.prose.push(Prose {
                text: body_of(row),
                before_step: turn.steps.len(),
            }),
            "text" => turn.answer = Some(body_of(row)),
            "tool_start" => {
                if let Some(call_id) = row["call_id"].as_str() {
                    pending = Some((call_id.to_string(), call_from_start(row)));
                }
            }
            "tool_result" => {
                let Some((call_id, mut call)) = pending.take() else {
                    continue;
                };
                if row["call_id"].as_str() != Some(call_id.as_str()) {
                    continue;
                }
                finish_call(&mut call, row);
                turn.steps.push(Step {
                    call: Some(call),
                    accounting: Accounting::default(),
                    offset_ms,
                });
            }
            _ => {}
        }
    }

    // A call whose result never arrived is a call still running — rendering it
    // as absent would hide the very thing a reader opened a live transcript to
    // watch.
    if let Some((_, mut call)) = pending.take() {
        call.status = Status::Running;
        turn.steps.push(Step {
            call: Some(call),
            accounting: Accounting::default(),
            offset_ms: 0,
        });
    }

    turn.duration_ms = turn.steps.last().map_or(0, |s| s.offset_ms);
    if let Some(cost) = execution["cost_usd"].as_f64()
        && let Some(last) = turn.steps.last_mut()
    {
        // The store records one cost for the execution, not per step. It is
        // attached to the last step rather than spread evenly, because an
        // invented per-step split would read as measured data.
        last.accounting.micros = micros_from_usd(cost);
    }

    Run {
        name: execution["kind"].as_str().unwrap_or("run").to_string(),
        model: format!(
            "{}/{}",
            execution["provider"].as_str().unwrap_or("?"),
            execution["model"].as_str().unwrap_or("?")
        ),
        started_at: execution["started_at"].as_str().unwrap_or("").to_string(),
        turns: vec![turn],
    }
}

/// Dollars to whole micro-dollars, saturating rather than wrapping.
fn micros_from_usd(usd: f64) -> u64 {
    if !usd.is_finite() || usd <= 0.0 {
        return 0;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Guarded above; saturates.
    let micros = (usd * 1_000_000.0).round() as i128;
    u64::try_from(micros.clamp(0, i128::from(u64::MAX))).unwrap_or(u64::MAX)
}

fn outcome_status(execution: &Value) -> Status {
    match execution["outcome"].as_str() {
        Some("ok" | "success" | "completed") => Status::Ok,
        Some("error" | "failed") => Status::Error,
        None => Status::Running,
        Some(_) => Status::Warn,
    }
}

fn body_of(row: &Value) -> String {
    row["body"].as_str().unwrap_or_default().to_string()
}

/// Build the call half from a `tool_start` row.
///
/// The row's `body` is the pretty-printed argument JSON. It is parsed back into
/// values rather than rendered as a blob — the blob is the defect this whole
/// change exists to remove, and a header plus key/value rows needs the fields.
fn call_from_start(row: &Value) -> Call {
    let name = row["name"].as_str().unwrap_or("tool");
    let tool = ToolKind::from_name(name);
    let input: Value = serde_json::from_str(&body_of(row)).unwrap_or(Value::Null);

    let header_object = header_object(&tool, &input);
    let args = arg_rows(&input);
    let files = files_from_input(&tool, &input);

    Call {
        tool,
        header_object,
        args,
        output: Output::default(),
        files,
        status: Status::Running,
        duration_ms: 0,
        speculated: false,
    }
}

/// The object of the verb: which argument the header prints.
///
/// Falls back to the first string argument for a tool this binary has never
/// seen — an MCP tool must render as a useful one-liner, not as a bare name.
fn header_object(tool: &ToolKind, input: &Value) -> String {
    let key = match tool {
        ToolKind::Bash => "command",
        ToolKind::ReadFile | ToolKind::WriteFile | ToolKind::EditFile | ToolKind::DeleteFile => {
            "path"
        }
        ToolKind::Search => "query",
        ToolKind::Other(_) => "",
    };
    if let Some(found) = input.get(key).and_then(Value::as_str) {
        return found.to_string();
    }
    input
        .as_object()
        .and_then(|map| map.values().find_map(Value::as_str))
        .unwrap_or_default()
        .to_string()
}

/// Every argument as a display row. Whichever one the header printed is dropped
/// later by [`Call::extra_args`], so this does not have to know.
fn arg_rows(input: &Value) -> Vec<ArgRow> {
    let Some(map) = input.as_object() else {
        return Vec::new();
    };
    map.iter()
        .map(|(key, value)| ArgRow {
            key: key.clone(),
            value: match value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            },
        })
        .collect()
}

/// The file change a mutation call's own arguments describe.
///
/// `write_file` carries the whole new file, so its diff is exact and all-green.
/// `edit_file` carries only the replaced fragment (see the module header).
/// `delete_file` carries neither side, so it renders a header with no contents
/// rather than inventing any.
fn files_from_input(tool: &ToolKind, input: &Value) -> Vec<FileChange> {
    let path = input
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if path.is_empty() {
        return Vec::new();
    }
    let text = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    match tool {
        ToolKind::WriteFile => vec![FileChange {
            path,
            before: String::new(),
            after: text("content"),
            status: FileStatus::New,
        }],
        ToolKind::EditFile => vec![FileChange {
            path,
            before: text("old_string"),
            after: text("new_string"),
            status: FileStatus::Modified,
        }],
        ToolKind::DeleteFile => vec![FileChange {
            path,
            before: String::new(),
            after: String::new(),
            status: FileStatus::Deleted,
        }],
        _ => Vec::new(),
    }
}

/// Complete a call from its `tool_result` row.
fn finish_call(call: &mut Call, row: &Value) {
    call.status = if row["ok"].as_bool().unwrap_or(true) {
        Status::Ok
    } else {
        Status::Error
    };
    call.duration_ms = row["duration_ms"].as_u64().unwrap_or(0);
    call.speculated = row["speculated"].as_bool().unwrap_or(false);
    call.output = Output::from_text(row["body"].as_str().unwrap_or_default());
    // The journal clips a long body at the transport. Say so, rather than
    // letting the fold control imply the output simply ended there.
    if row["truncated"].as_bool().unwrap_or(false) {
        call.output.clipped = 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn execution() -> Value {
        json!({
            "id": 1,
            "kind": "run",
            "prompt": "fix the overfull hbox warnings",
            "provider": "zai",
            "model": "glm-5.2",
            "outcome": "ok",
            "cost_usd": 0.0061,
            "started_at": "14:02:11",
        })
    }

    #[test]
    fn a_call_and_its_result_fold_into_one_node() {
        let journal = vec![
            json!({
                "type": "tool_start", "ts": 0, "call_id": "c1", "name": "bash",
                "body": "{\"command\": \"pdflatex main.tex\"}",
            }),
            json!({
                "type": "tool_result", "ts": 1_400, "call_id": "c1", "name": "bash",
                "ok": true, "duration_ms": 1_400, "speculated": false,
                "body": "Overfull \\hbox",
            }),
        ];
        let run = build_run(&execution(), &journal);
        assert_eq!(run.turns[0].steps.len(), 1, "one step, not two");
        let call = run.turns[0].steps[0].call.as_ref().unwrap();
        assert_eq!(call.header_object, "pdflatex main.tex");
        assert_eq!(call.duration_ms, 1_400);
        assert_eq!(call.output.lines, vec!["Overfull \\hbox".to_string()]);
        // The command is in the header, and the args toggle no longer repeats it.
        assert!(call.extra_args().is_empty());
    }

    #[test]
    fn an_edit_call_carries_a_diffable_fragment() {
        let journal = vec![
            json!({
                "type": "tool_start", "ts": 0, "call_id": "c1", "name": "edit_file",
                "body": "{\"path\":\"main.tex\",\"old_string\":\"{15pt}\",\"new_string\":\"{12pt}\"}",
            }),
            json!({
                "type": "tool_result", "ts": 30, "call_id": "c1",
                "ok": true, "duration_ms": 30, "body": "edited main.tex",
            }),
        ];
        let run = build_run(&execution(), &journal);
        let call = run.turns[0].steps[0].call.as_ref().unwrap();
        assert_eq!(call.files.len(), 1);
        assert_eq!(call.files[0].before, "{15pt}");
        assert_eq!(call.files[0].after, "{12pt}");
        assert_eq!(call.files[0].status, FileStatus::Modified);
    }

    #[test]
    fn a_call_whose_result_never_arrived_renders_as_running() {
        let journal = vec![json!({
            "type": "tool_start", "ts": 0, "call_id": "c1", "name": "bash",
            "body": "{\"command\": \"sleep 600\"}",
        })];
        let run = build_run(&execution(), &journal);
        assert_eq!(run.turns[0].steps.len(), 1);
        assert_eq!(run.turns[0].steps[0].status(), Status::Running);
    }

    #[test]
    fn an_orphan_result_is_dropped_rather_than_rendered_anonymously() {
        let journal = vec![json!({
            "type": "tool_result", "ts": 0, "call_id": "gone", "ok": true, "body": "x",
        })];
        let run = build_run(&execution(), &journal);
        assert!(run.turns[0].steps.is_empty());
    }

    #[test]
    fn an_unknown_row_type_does_not_blank_the_transcript() {
        let journal = vec![
            json!({"type": "some_future_event", "ts": 0, "body": "?"}),
            json!({"type": "text", "ts": 1, "body": "done"}),
        ];
        let run = build_run(&execution(), &journal);
        assert_eq!(run.turns[0].answer.as_deref(), Some("done"));
    }

    #[test]
    fn a_failed_result_marks_the_call_as_an_error() {
        let journal = vec![
            json!({
                "type": "tool_start", "ts": 0, "call_id": "c1", "name": "bash",
                "body": "{\"command\": \"false\"}",
            }),
            json!({
                "type": "tool_result", "ts": 5, "call_id": "c1",
                "ok": false, "duration_ms": 5, "body": "exit status 1",
            }),
        ];
        let run = build_run(&execution(), &journal);
        assert_eq!(run.turns[0].steps[0].status(), Status::Error);
    }

    #[test]
    fn the_execution_cost_survives_the_conversion_to_micros() {
        let journal = vec![
            json!({"type":"tool_start","ts":0,"call_id":"c","name":"bash","body":"{}"}),
            json!({"type":"tool_result","ts":1,"call_id":"c","ok":true,"body":""}),
        ];
        let run = build_run(&execution(), &journal);
        assert_eq!(run.rollup().micros, 6_100);
    }

    #[test]
    fn a_clipped_body_is_admitted_rather_than_read_as_the_end_of_the_output() {
        let journal = vec![
            json!({"type":"tool_start","ts":0,"call_id":"c","name":"bash","body":"{}"}),
            json!({
                "type":"tool_result","ts":1,"call_id":"c","ok":true,
                "body":"first","truncated":true,
            }),
        ];
        let run = build_run(&execution(), &journal);
        let call = run.turns[0].steps[0].call.as_ref().unwrap();
        assert_eq!(call.output.clipped, 1);
    }
}
