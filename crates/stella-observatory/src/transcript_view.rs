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
    Accounting, ArgRow, Call, CallAnchor, FileChange, FileStatus, Note, NoteKind, Output, Prose,
    Run, Status, Step, ToolKind, Turn,
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
    // A `step_usage` row precedes the tool calls its model call requested, so
    // its accounting is held and attached to the next completed call — that is
    // where the reader's eye lands, and the turn rollup sums the same figures
    // either way.
    let mut pending_acc: Option<Accounting> = None;
    let mut metered = false;
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
                    accounting: pending_acc.take().unwrap_or_default(),
                    offset_ms,
                });
            }
            "step_usage" => {
                metered = true;
                if let Some(acc) = pending_acc.take()
                    && let Some(last) = turn.steps.last_mut()
                {
                    // Two model calls with no tool call between them: the
                    // earlier call's figures still have to land somewhere the
                    // rollup can see.
                    last.accounting = last.accounting.merged(acc);
                }
                pending_acc = Some(usage_accounting(row));
                turn.notes.push(meter_note(row, turn.steps.len()));
            }
            // A `Handoff` note per bracket edge (#4627) — the kind
            // `stella_transcript` defines as "work handed elsewhere:
            // sub-agents, commits, pull requests". Placed by `before_step` so
            // the two edges bracket the steps that ran between them, which is
            // what makes a delegated turn read as a delegated turn instead of
            // a gap.
            //
            // Not a `Step`: a step is a call with a result, and the child's
            // own calls already arrive on this stream in their own right
            // (forwarded across the child/parent boundary). Folding the
            // bracket as a step too would count the fan-out twice.
            "sub_agent" => turn.notes.push(subagent_note(row, turn.steps.len())),
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

    // Usage from a final model call that requested no tool (the answer call)
    // still has to reach the rollup.
    if let Some(acc) = pending_acc.take()
        && let Some(last) = turn.steps.last_mut()
    {
        last.accounting = last.accounting.merged(acc);
    }

    turn.duration_ms = turn.steps.last().map_or(0, |s| s.offset_ms);
    if !metered
        && let Some(cost) = execution["cost_usd"].as_f64()
        && let Some(last) = turn.steps.last_mut()
    {
        // The store records one cost for the execution, not per step. It is
        // attached to the last step rather than spread evenly, because an
        // invented per-step split would read as measured data — and only when
        // no `step_usage` rows metered the turn call by call, which is the
        // measured version of the same figure.
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

/// The [`Accounting`] a `step_usage` journal row settles.
fn usage_accounting(row: &Value) -> Accounting {
    Accounting {
        tokens_in: row["input_tokens"].as_u64().unwrap_or(0),
        tokens_out: row["output_tokens"].as_u64().unwrap_or(0),
        cached_in: row["cached_input_tokens"].as_u64().unwrap_or(0),
        micros: micros_from_usd(row["cost_usd"].as_f64().unwrap_or(0.0)),
    }
}

/// One metering row: which model was called, through what, and what it cost.
///
/// The summary is the findable line — role, route (gateway→upstream when a
/// gateway names one), model, tokens and wall clock. Everything slower to
/// read folds into the detail. The anchor carries the call's engine
/// coordinates so a host page can open its prompt inspector on exactly this
/// call.
fn meter_note(row: &Value, before_step: usize) -> Note {
    let role = row["role"].as_str().unwrap_or("call").to_string();
    let provider = row["provider"].as_str().unwrap_or("?");
    let route = match row["upstream_provider"].as_str() {
        Some(upstream) if !upstream.is_empty() => format!("{provider}→{upstream}"),
        _ => provider.to_string(),
    };
    let model = row["model"].as_str().unwrap_or("?");
    let step = row["step"].as_u64().unwrap_or(0);
    let billed = row["input_tokens"].as_u64().unwrap_or(0);
    let cached = row["cached_input_tokens"].as_u64().unwrap_or(0);
    let written = row["cache_write_tokens"].as_u64().unwrap_or(0);
    let summary = format!(
        "step {step} · {role} · {route} · {model} · {} in · {} out · {}",
        fmt_tok(billed + cached),
        fmt_tok(row["output_tokens"].as_u64().unwrap_or(0)),
        fmt_ms(row["duration_ms"].as_u64().unwrap_or(0)),
    );
    let mut detail = vec![format!(
        "input: {} uncached · {} from prompt cache · {} written to cache",
        fmt_tok(billed),
        fmt_tok(cached),
        fmt_tok(written)
    )];
    if let Some(reasoning) = row["reasoning_tokens"].as_u64() {
        detail.push(format!("reasoning share of output: {}", fmt_tok(reasoning)));
    }
    if let Some(est) = row["estimated_input_tokens"].as_u64().filter(|n| *n > 0) {
        detail.push(format!("engine estimate before the call: {}", fmt_tok(est)));
    }
    detail.push(format!(
        "cost ${:.4} · {} retries",
        row["cost_usd"].as_f64().unwrap_or(0.0),
        row["retries"].as_u64().unwrap_or(0)
    ));
    if let Some(finish) = row["finish_reason"].as_str() {
        detail.push(if finish == "length" {
            "stopped: length — the call hit its output ceiling".to_string()
        } else {
            format!("stopped: {finish}")
        });
    }
    if row["complete"].as_bool() == Some(false) {
        detail.push("provider did not supply a complete usage envelope".to_string());
    }
    if let Some(agent) = row["sub_agent_id"].as_str() {
        detail.push(format!("spent by sub-agent {agent}"));
    }
    if let Some(body) = row["body"].as_str().filter(|b| !b.trim().is_empty()) {
        detail.push("output (this call emits no transcript text of its own):".to_string());
        detail.extend(body.lines().map(str::to_string));
    }
    Note {
        kind: NoteKind::Meter,
        summary,
        detail,
        before_step,
        inspect: Some(CallAnchor { step, role }),
    }
}

/// One edge of a sub-agent bracket: the child starting, or the child ending.
///
/// The summary is the findable line — which child, and either what it was
/// asked or how it ended. The detail carries the numbers that answer "was
/// delegating this worth it": what the child cost, how many model calls it
/// made, and how many messages of its own transcript the parent never had to
/// carry, which is the primitive's whole value proposition.
///
/// No [`CallAnchor`]: a bracket is not a model call, so there is no
/// `(step, role)` for a host's prompt inspector to open on.
fn subagent_note(row: &Value, before_step: usize) -> Note {
    let agent = row["agent_id"].as_str().unwrap_or("?");
    let mut detail = Vec::new();
    let summary = match row["phase"].as_str() {
        Some("started") => {
            if let Some(budget) = row["budget_usd"].as_f64() {
                detail.push(format!("budget carved: ${budget:.4}"));
            }
            detail.push(format!(
                "write access: {} · depth {}",
                row["write_access"].as_bool().unwrap_or(false),
                row["depth"].as_u64().unwrap_or(1)
            ));
            if let Some(effort) = row["effort"].as_str() {
                detail.push(format!("reasoning effort: {effort}"));
            }
            let task = row["instruction_preview"].as_str().unwrap_or("").trim();
            if task.is_empty() {
                format!("sub-agent {agent} started")
            } else {
                format!("sub-agent {agent} started · {task}")
            }
        }
        Some("finished") => {
            detail.push(format!(
                "cost ${:.4} · {} model calls · {} messages absorbed",
                row["cost_usd"].as_f64().unwrap_or(0.0),
                row["steps"].as_u64().unwrap_or(0),
                row["absorbed_messages"].as_u64().unwrap_or(0)
            ));
            if let Some(reason) = row["reason"].as_str() {
                detail.push(format!("reason: {reason}"));
            }
            if row["report_truncated"].as_bool().unwrap_or(false) {
                detail.push("the child's report was clipped to its cap".to_string());
            }
            if let Some(report) = row["body"].as_str().filter(|b| !b.trim().is_empty()) {
                detail.push("report:".to_string());
                detail.extend(report.lines().map(str::to_string));
            }
            let status = row["status"].as_str().unwrap_or("ended");
            format!("sub-agent {agent} {status}")
        }
        // A phase this build has never heard of still says a child was
        // bracketed here, rather than drawing nothing where one ran.
        _ => format!("sub-agent {agent}"),
    };
    Note {
        kind: NoteKind::Handoff,
        summary,
        detail,
        before_step,
        inspect: None,
    }
}

/// Humanize a token count: `981`, `32.4k`.
fn fmt_tok(n: u64) -> String {
    if n < 1_000 {
        n.to_string()
    } else {
        #[allow(clippy::cast_precision_loss)] // Display only; ±1 token is invisible at 0.1k.
        let thousands = n as f64 / 1_000.0;
        format!("{thousands:.1}k")
    }
}

/// `842ms` under a second, `8.4s` above.
fn fmt_ms(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms}ms")
    } else {
        #[allow(clippy::cast_precision_loss)] // Display only.
        let secs = ms as f64 / 1_000.0;
        format!("{secs:.1}s")
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

    /// **The witness for #4627.** Both bracket edges fold into `Handoff`
    /// notes, positioned so they enclose the steps the child ran.
    ///
    /// Fails before this change: `build_run` had no `sub_agent` arm at all —
    /// the row fell through `_ => {}` — so a delegated turn rendered as a
    /// parent that had somehow paused, and the reader had no way to tell a
    /// fan-out from a stall.
    #[test]
    fn both_edges_of_a_sub_agent_bracket_become_handoff_notes() {
        let journal = vec![
            json!({
                "type": "sub_agent", "ts": 0, "phase": "started",
                "agent_id": "search-1", "instruction_preview": "find the retry policy",
                "budget_usd": 0.25, "write_access": false, "depth": 1, "effort": "high",
            }),
            json!({
                "type": "tool_start", "ts": 10, "call_id": "c1", "name": "search",
                "body": "{\"query\": \"retry\"}",
            }),
            json!({
                "type": "tool_result", "ts": 40, "call_id": "c1", "ok": true, "body": "retry.rs",
            }),
            json!({
                "type": "sub_agent", "ts": 50, "phase": "finished",
                "agent_id": "search-1", "status": "completed", "cost_usd": 0.004,
                "steps": 3, "absorbed_messages": 9, "report_truncated": false,
                "body": "retry policy lives in retry.rs",
            }),
        ];
        let run = build_run(&execution(), &journal);
        let turn = &run.turns[0];
        assert_eq!(turn.notes.len(), 2, "one note per bracket edge");
        assert!(
            turn.notes
                .iter()
                .all(|n| n.kind == NoteKind::Handoff && n.inspect.is_none()),
            "a bracket is work handed elsewhere, and is not a model call"
        );
        // The edges bracket the step between them — which is the whole point
        // of putting them on the timeline rather than only in a side panel.
        assert_eq!(
            (turn.notes[0].before_step, turn.notes[1].before_step),
            (0, 1)
        );

        assert_eq!(
            turn.notes[0].summary,
            "sub-agent search-1 started · find the retry policy"
        );
        assert!(
            turn.notes[0].detail.iter().any(|d| d.contains("$0.2500")),
            "{:?}",
            turn.notes[0].detail
        );
        assert_eq!(turn.notes[1].summary, "sub-agent search-1 completed");
        assert!(
            turn.notes[1]
                .detail
                .iter()
                .any(|d| d.contains("9 messages absorbed")),
            "the value proposition is reported, not asserted: {:?}",
            turn.notes[1].detail
        );
        assert!(
            turn.notes[1]
                .detail
                .contains(&"retry policy lives in retry.rs".to_string()),
            "{:?}",
            turn.notes[1].detail
        );
        // The child's own forwarded call is still one step, not two: folding
        // the bracket as a step as well would count the fan-out twice.
        assert_eq!(turn.steps.len(), 1);
    }

    #[test]
    fn a_step_usage_row_becomes_a_metering_note_with_an_anchor() {
        let journal = vec![
            json!({
                "type": "step_usage", "ts": 0, "step": 3, "role": "worker",
                "provider": "openrouter", "upstream_provider": "anthropic",
                "model": "claude-fable-5", "input_tokens": 3_200,
                "output_tokens": 410, "cached_input_tokens": 29_100,
                "cache_write_tokens": 1_200, "duration_ms": 8_400,
                "cost_usd": 0.0134, "retries": 0,
            }),
            json!({
                "type": "tool_start", "ts": 1, "call_id": "c1", "name": "bash",
                "body": "{\"command\": \"true\"}",
            }),
            json!({
                "type": "tool_result", "ts": 2, "call_id": "c1", "ok": true, "body": "",
            }),
        ];
        let run = build_run(&execution(), &journal);
        let turn = &run.turns[0];
        assert_eq!(turn.notes.len(), 1);
        let note = &turn.notes[0];
        // The gateway names its upstream: the arrow is what lets a trace say
        // which vendor's silicon served the call, not just which API was
        // dialled.
        assert!(
            note.summary.contains("openrouter→anthropic"),
            "{}",
            note.summary
        );
        assert!(note.summary.contains("claude-fable-5"));
        let anchor = note
            .inspect
            .as_ref()
            .expect("a metered call is inspectable");
        assert_eq!(anchor.step, 3);
        assert_eq!(anchor.role, "worker");
        // The call's figures land on the tool step it requested, so the turn
        // rollup sums them exactly once.
        assert_eq!(turn.steps[0].accounting.tokens_in, 3_200);
        assert_eq!(turn.steps[0].accounting.cached_in, 29_100);
        assert_eq!(run.rollup().micros, 13_400);
    }

    #[test]
    fn metered_turns_do_not_double_count_the_execution_cost() {
        // `execution()` carries cost_usd 0.0061; the metered figure must win,
        // because it is the per-call measurement of the same money.
        let journal = vec![
            json!({
                "type": "step_usage", "ts": 0, "step": 1, "role": "worker",
                "provider": "zai", "model": "glm-5.2", "input_tokens": 10,
                "output_tokens": 5, "cached_input_tokens": 0,
                "cost_usd": 0.002, "duration_ms": 100, "retries": 0,
            }),
            json!({
                "type": "tool_start", "ts": 1, "call_id": "c1", "name": "bash",
                "body": "{}",
            }),
            json!({"type": "tool_result", "ts": 2, "call_id": "c1", "ok": true, "body": ""}),
        ];
        let run = build_run(&execution(), &journal);
        assert_eq!(run.rollup().micros, 2_000);
    }

    #[test]
    fn a_final_answer_calls_usage_reaches_the_rollup_without_a_tool_step() {
        let journal = vec![
            json!({
                "type": "tool_start", "ts": 0, "call_id": "c1", "name": "bash",
                "body": "{}",
            }),
            json!({"type": "tool_result", "ts": 1, "call_id": "c1", "ok": true, "body": ""}),
            json!({
                "type": "step_usage", "ts": 2, "step": 2, "role": "worker",
                "provider": "zai", "model": "glm-5.2", "input_tokens": 700,
                "output_tokens": 90, "cached_input_tokens": 0,
                "cost_usd": 0.001, "duration_ms": 900, "retries": 0,
            }),
        ];
        let run = build_run(&execution(), &journal);
        assert_eq!(run.rollup().tokens_in, 700);
        assert_eq!(run.rollup().tokens_out, 90);
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
