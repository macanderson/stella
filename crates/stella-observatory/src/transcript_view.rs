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

/// What one pass of [`fold_rows`] learned beyond the model it built — the
/// facts the incremental tail protocol (#4566) mints its cursor from.
pub(crate) struct FoldFacts {
    /// Whether any `step_usage` row metered this span.
    pub(crate) metered: bool,
    /// The seq of the last `text` row, so a later suffix that carries no text
    /// of its own can still recover the answer it must keep rendering.
    pub(crate) last_text_seq: Option<i64>,
    /// Per pushed step: the seq of the `tool_result` that completed it —
    /// `None` for the synthetic still-running step. A cursor's resume point is
    /// one of these seqs, because right after a completing `tool_result` the
    /// fold holds no half-built call and no unattached usage, so a fold
    /// resumed there sees exactly what the full fold saw.
    pub(crate) result_seqs: Vec<Option<i64>>,
}

/// The step-loop core shared by [`build_run`] and [`build_run_tail`]: fold
/// journal rows into `turn`, appending steps, notes and prose with indices
/// local to this pass.
fn fold_rows(turn: &mut Turn, journal: &[Value], base_ts: i64) -> FoldFacts {
    // `tool_start` is held until its `tool_result` arrives, because a Call is
    // only complete once both halves are known.
    let mut pending: Option<(String, Call)> = None;
    // A `step_usage` row precedes the tool calls its model call requested, so
    // its accounting is held and attached to the next completed call — that is
    // where the reader's eye lands, and the turn rollup sums the same figures
    // either way.
    let mut pending_acc: Option<Accounting> = None;
    let mut facts = FoldFacts {
        metered: false,
        last_text_seq: None,
        result_seqs: Vec::new(),
    };

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
            "text" => {
                turn.answer = Some(body_of(row));
                facts.last_text_seq = row["seq"].as_i64();
            }
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
                facts.result_seqs.push(row["seq"].as_i64());
            }
            "step_usage" => {
                facts.metered = true;
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
        facts.result_seqs.push(None);
    }

    // Usage from a final model call that requested no tool (the answer call)
    // still has to reach the rollup.
    if let Some(acc) = pending_acc.take()
        && let Some(last) = turn.steps.last_mut()
    {
        last.accounting = last.accounting.merged(acc);
    }
    facts
}

/// The empty turn every fold starts from — everything knowable from the
/// execution's head row alone.
fn base_turn(execution: &Value) -> Turn {
    Turn {
        name: execution["kind"].as_str().unwrap_or("turn").to_string(),
        prompt: execution["prompt"].as_str().unwrap_or_default().to_string(),
        prose: Vec::new(),
        notes: vec![],
        steps: Vec::new(),
        answer: None,
        status: outcome_status(execution),
        duration_ms: 0,
    }
}

fn run_of(execution: &Value, turn: Turn) -> Run {
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

/// Fold an execution's head row and journal rows into a renderable run.
///
/// Tolerant by construction: a row this binary does not understand is skipped
/// rather than fatal, and a `tool_result` with no matching `tool_start` is
/// dropped rather than rendered as an anonymous orphan. A transcript that
/// blanks because one row was written by a newer binary is worse than a
/// transcript missing one row.
#[must_use]
pub(crate) fn build_run(execution: &Value, journal: &[Value]) -> Run {
    let mut turn = base_turn(execution);
    let base_ts = journal.first().and_then(|r| r["ts"].as_i64()).unwrap_or(0);
    let facts = fold_rows(&mut turn, journal, base_ts);
    turn.duration_ms = turn.steps.last().map_or(0, |s| s.offset_ms);
    if !facts.metered
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
    run_of(execution, turn)
}

/// The client-echoed resume point of the incremental transcript protocol
/// (#4566): everything a fold over only the journal's new rows needs in order
/// to agree, byte for byte, with a fold over the whole journal.
///
/// Minted by the server, echoed back verbatim by the page on its next poll.
/// Content-free by construction — counts, seqs and token totals, never a body
/// — and derivable at any time by re-folding from scratch, so a client that
/// drops it merely pays one full-size tick.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct TailCursor {
    /// The seq of the `tool_result` that completed the last settled step.
    /// The next fold reads only rows after this. `-1` means fold everything.
    pub(crate) seq: i64,
    /// Settled steps, notes and prose blocks — the whole-turn index each of
    /// the tail's local indices continues from.
    pub(crate) steps: usize,
    pub(crate) notes: usize,
    pub(crate) prose: usize,
    /// The whole journal's first-row timestamp, so tail offsets stay measured
    /// from the run's start rather than the suffix's.
    pub(crate) base_ts: i64,
    /// The last settled step's time offset — the tail's time-column dedup
    /// seed, and the turn's wall time while the tail has no steps.
    pub(crate) prev_offset_ms: Option<u64>,
    /// The settled steps' accounting, summed into the receipt's rollup.
    pub(crate) carried: Accounting,
    /// Whether any settled row was a `step_usage` — keeps the unmetered
    /// execution-cost fallback from firing on a metered run's tail.
    pub(crate) metered: bool,
    /// The seq of the last `text` row seen, settled epochs included: the
    /// answer must keep rendering even on a tick whose suffix carries no
    /// text row of its own.
    pub(crate) answer_seq: Option<i64>,
}

impl TailCursor {
    /// The first tick's cursor: fold everything. The route stamps `base_ts`
    /// from the journal's first row before folding, so every later suffix
    /// measures its offsets from the run's start.
    pub(crate) fn start(first_row_ts: i64) -> Self {
        Self {
            seq: -1,
            base_ts: first_row_ts,
            ..Self::default()
        }
    }

    /// The cursor as the wire carries it. Hand-rolled through `Value` like
    /// every other JSON shape in this crate — the crate deliberately links
    /// `serde_json` alone.
    pub(crate) fn to_value(self) -> Value {
        serde_json::json!({
            "seq": self.seq,
            "steps": self.steps,
            "notes": self.notes,
            "prose": self.prose,
            "base_ts": self.base_ts,
            "prev_offset_ms": self.prev_offset_ms,
            "carried": self.carried,
            "metered": self.metered,
            "answer_seq": self.answer_seq,
        })
    }

    /// Parse an echoed cursor. `None` for anything malformed — the route
    /// treats that as "no cursor" and serves a full-size tick, because a
    /// stale or truncated echo must degrade to more data, never to an error.
    pub(crate) fn from_value(v: &Value) -> Option<Self> {
        Some(Self {
            seq: v["seq"].as_i64()?,
            steps: usize::try_from(v["steps"].as_u64()?).ok()?,
            notes: usize::try_from(v["notes"].as_u64()?).ok()?,
            prose: usize::try_from(v["prose"].as_u64()?).ok()?,
            base_ts: v["base_ts"].as_i64()?,
            prev_offset_ms: v["prev_offset_ms"].as_u64(),
            carried: serde_json::from_value(v["carried"].clone()).ok()?,
            metered: v["metered"].as_bool()?,
            answer_seq: v["answer_seq"].as_i64(),
        })
    }
}

/// Fold only the journal rows after a cursor's resume point into the turn's
/// tail — the model [`stella_transcript::html::render_turn_tail`] renders.
///
/// The rows must be exactly the journal filtered to `seq > cursor.seq`, and
/// the cursor must have been minted by [`advance_cursor`] over this same
/// execution (or be [`TailCursor::default`] with `seq: -1` for a first tick).
/// Under that contract the returned turn is the byte-for-byte suffix of what
/// [`build_run`] over the whole journal would hold: local indices continue
/// the settled counts, the wall time covers the whole turn, and the
/// unmetered cost fallback lands on the same final step. The tests hold the
/// two folds to each other tick by tick.
pub(crate) fn build_run_tail(
    execution: &Value,
    rows: &[Value],
    cursor: &TailCursor,
) -> (Run, FoldFacts) {
    let mut turn = base_turn(execution);
    // The head owns the prompt; a tail never renders it.
    turn.prompt = String::new();
    let facts = fold_rows(&mut turn, rows, cursor.base_ts);
    turn.duration_ms = turn
        .steps
        .last()
        .map_or(cursor.prev_offset_ms.unwrap_or(0), |s| s.offset_ms);
    if !(cursor.metered || facts.metered)
        && let Some(cost) = execution["cost_usd"].as_f64()
        && let Some(last) = turn.steps.last_mut()
    {
        // The unmetered fallback, exactly as `build_run` applies it. Its
        // target — the fold's final step — never settles while the run is
        // live, so restating it every tick amends nothing already painted.
        last.accounting.micros = micros_from_usd(cost);
    }
    (run_of(execution, turn), facts)
}

/// How much of this tick's tail settled, and the cursor the next tick resumes
/// from.
///
/// A step settles once a later completed step exists — until then a trailing
/// `step_usage` row may still merge figures into it, so its rendered bytes
/// are not yet final. With `completed` completed steps in the tail, the first
/// `completed - 1` of them settle; fewer than two completed steps settle
/// nothing, and the echoed cursor comes back unchanged.
pub(crate) fn advance_cursor(
    cursor: &TailCursor,
    tail: &Turn,
    facts: &FoldFacts,
) -> (usize, TailCursor) {
    let completed = facts.result_seqs.iter().flatten().count();
    if completed < 2 {
        return (0, *cursor);
    }
    let newly = completed - 1;
    let carried = tail.steps[..newly]
        .iter()
        .fold(cursor.carried, |acc, step| acc.merged(step.accounting));
    let next = TailCursor {
        seq: facts.result_seqs[newly - 1].unwrap_or(cursor.seq),
        steps: cursor.steps + newly,
        notes: cursor.notes + tail.notes.iter().filter(|n| n.before_step < newly).count(),
        prose: cursor.prose + tail.prose.iter().filter(|p| p.before_step < newly).count(),
        base_ts: cursor.base_ts,
        prev_offset_ms: Some(tail.steps[newly - 1].offset_ms),
        carried,
        metered: cursor.metered || facts.metered,
        answer_seq: facts.last_text_seq.or(cursor.answer_seq),
    };
    (newly, next)
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
