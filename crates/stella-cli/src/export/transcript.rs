//! The session transcript — the replayable half of the `/export` archive.
//!
//! [`super::export_session`] bundles telemetry *tables*: cost, tools called,
//! files touched, what was sent to the model. None of that is the session
//! itself. The session is the ordered stream of what the model said, what it
//! ran, and what came back. That stream lives in `events`, not in the tables.
//!
//! This module folds [`Store::session_events`](stella_store::Store::session_events)
//! into a [`stella_transcript::model::Run`] and renders it with
//! [`stella_transcript::html::render_page`] — the same renderer `stella
//! observe` uses. Both surfaces draw one row grammar for one session.
//! Everything below this line builds and redacts that run; the call to the
//! renderer is [`render`]'s last line.
//!
//! # Three rules this fold has to keep
//!
//! **1. Redact everything, once, at the end.** The table dumps go through
//! `redact_dump` before the dashboard embeds them. This event journal never
//! passes through that path, so every string this fold takes off an event
//! must be masked before [`render`] hands the run to the renderer. Rather
//! than clean each string as it is read, [`redact_run`] does it once: it
//! serializes the finished [`stella_transcript::model::Run`] to JSON, masks
//! every string value in the tree, and reads the run back. Every model type
//! round-trips through `serde_json` byte for byte, so this one pass cannot
//! miss a field the way a scattered set of `clean()` calls could.
//!
//! **2. Never invent a clock reading finer than what was measured.**
//! `events.ts` is a plain SQLite timestamp with no sub-second part
//! ([`SessionEventRecord::ts`](stella_store::SessionEventRecord::ts)), so
//! this fold never reads it. [`Step::offset_ms`] is summed instead from the
//! real, millisecond-precise durations on `ToolResult` and `StepUsage` — the
//! same durations the shared renderer already draws for `stella observe`,
//! and the same sum [`stella_tui::transcript_build::RunBuilder`] already
//! keeps for the live surface.
//!
//! **3. Drop nothing.** The shared run model has no slot for most of the
//! wire's event kinds: a stage boundary, a provider fallback, a verdict, and
//! more. Each of those becomes a [`Note`] — its wire tag as the summary, its
//! whole payload as the fold-out detail ([`push_note`]). A reader can read
//! what happened even for a kind this crate has never named, and can grep
//! `raw/` for the tag. A transcript that quietly skipped what it did not
//! recognize would be a summary pretending to be a replay.

use std::collections::HashMap;

use serde::Serialize;
use stella_protocol::{AgentEvent, ToolOutput};
use stella_store::{SessionEventRecord, SessionJournal};
use stella_transcript::fold::FoldState;
use stella_transcript::html;
use stella_transcript::model::{
    Accounting, ArgRow, Call, Extent, FileChange, FileStatus, Note, NoteKind, Output, Patch, Prose,
    Run, Status, Step, ToolKind, Turn,
};

/// Cap on a single embedded tool result, file diff, or structured payload.
///
/// The archive is a file someone opens in a browser; one `bash` call that cats
/// a build log can carry tens of megabytes, and a handful of them make the
/// dashboard slower to open than the run took to execute. The cut is stated
/// inline (`… N bytes truncated`) rather than silent, and the untruncated
/// rows remain in `raw/` beside it.
const MAX_EMBEDDED_BYTES: usize = 64 * 1024;

/// Cap on the number of steps and notes committed to the run.
///
/// A long-running session can hold six figures of events, and past a point
/// the page stops being readable at all. Overflow is reported in the
/// transcript's [`Transcript::provenance`] line, never dropped quietly. Turn
/// openings (the prompt row) are never capped — a prompt is cheap and the
/// reader needs to see which turns exist even once their content is elided.
const MAX_ROWS: usize = 5_000;

/// The rendered transcript document, plus what the render had to leave out.
pub(crate) struct Transcript {
    /// A complete, self-contained HTML page — [`stella_transcript::html::render_page`]'s
    /// output, written to the archive as its own `transcript.html` member.
    pub(crate) html: String,
    /// How many steps and notes were committed to the run.
    pub(crate) rendered: usize,
    /// Events the store could not parse back into an `AgentEvent`
    /// ([`SessionJournal::skipped`]) — a stream recorded before a variant
    /// existed. Stated so a reader can tell a quiet session from a lossy
    /// read.
    pub(crate) unparseable: usize,
    /// Steps/notes dropped by [`MAX_ROWS`].
    pub(crate) overflow: usize,
    /// Whether any string in the transcript had a credential masked out of
    /// it.
    pub(crate) redacted: bool,
}

impl Transcript {
    /// The one-line provenance note the dashboard prints beside the link to
    /// `transcript.html` — what this rendering does and does not contain.
    pub(crate) fn provenance(&self) -> String {
        let mut parts = vec![format!("{} entries", super::comma(self.rendered as i64))];
        if self.overflow > 0 {
            parts.push(format!(
                "{} further entries not rendered (the archive's raw/ dumps are complete)",
                super::comma(self.overflow as i64)
            ));
        }
        if self.unparseable > 0 {
            parts.push(format!(
                "{} event(s) could not be replayed (recorded by an older build)",
                super::comma(self.unparseable as i64)
            ));
        }
        if self.redacted {
            parts.push("credentials masked".to_string());
        }
        parts.join(" · ")
    }
}

/// Fold a session's journal into a transcript document.
///
/// `prompts` maps `execution_id` to that execution's prompt, so the
/// transcript opens each turn with what was actually asked. It comes from
/// the already redacted `executions` dump rather than from the journal,
/// because the prompt is a column, not an event. `session_id` names the
/// document — the shared renderer's identity bar has nowhere else to read a
/// title from, since this fold performs no I/O of its own.
pub(crate) fn render(
    journal: &SessionJournal,
    prompts: &HashMap<i64, String>,
    session_id: &str,
) -> Transcript {
    let mut fold = Fold::new(prompts, session_id);
    for record in &journal.events {
        fold.push(record);
    }
    fold.finish(journal.skipped)
}

/// Accumulator for [`render`]. Builds a [`Run`] much the way
/// [`stella_tui::transcript_build::RunBuilder`] builds one from a live
/// `AgentEvent` stream, with three differences. A turn opens on an
/// `execution_id` change, not an explicit call. An orphaned
/// [`AgentEvent::ToolResult`] — no matching `ToolStart` in this journal —
/// still renders instead of being dropped. And the fallback for an event
/// kind the backbone does not name carries the whole payload, not a
/// narrated line.
struct Fold<'a> {
    run: Run,
    /// The turn currently accumulating, opened by [`Fold::open_execution`].
    turn: Option<Turn>,
    /// Calls dispatched but not yet resolved, oldest first.
    pending: Vec<(String, Call)>,
    /// `call_id` → (tool name, delegate id), kept for every call this fold
    /// has ever seen started — the fallback name for a `ToolResult` whose
    /// own `ToolStart` this journal never recorded, or recorded and already
    /// closed (a duplicate or late result).
    tool_names: HashMap<String, (String, Option<String>)>,
    /// Wall time attributed to the open turn so far, summed from the
    /// measured durations the events themselves report (module doc,
    /// property 2).
    elapsed_ms: u64,
    /// The execution whose turn is currently open.
    current_execution: Option<i64>,
    prompts: &'a HashMap<i64, String>,
    /// Steps and notes committed so far, against [`MAX_ROWS`].
    emitted: usize,
    overflow: usize,
}

impl<'a> Fold<'a> {
    fn new(prompts: &'a HashMap<i64, String>, session_id: &str) -> Self {
        Self {
            run: Run {
                name: format!("session {session_id}"),
                model: String::new(),
                started_at: String::new(),
                turns: Vec::new(),
            },
            turn: None,
            pending: Vec::new(),
            tool_names: HashMap::new(),
            elapsed_ms: 0,
            current_execution: None,
            prompts,
            emitted: 0,
            overflow: 0,
        }
    }

    /// Absorb one journal record.
    fn push(&mut self, record: &SessionEventRecord) {
        self.open_execution(record.execution_id);
        self.push_event(&record.event);
    }

    /// Open a new turn when the execution changes, carrying its prompt.
    fn open_execution(&mut self, execution_id: i64) {
        if self.current_execution == Some(execution_id) {
            return;
        }
        self.current_execution = Some(execution_id);
        let prompt = self.prompts.get(&execution_id).cloned().unwrap_or_default();
        self.start_turn(prompt);
    }

    /// Open a new turn around a prompt. Any turn still open is closed first.
    fn start_turn(&mut self, prompt: String) {
        self.finish_turn(Status::Ok);
        self.elapsed_ms = 0;
        self.turn = Some(Turn {
            name: slug(&prompt),
            prompt,
            prose: Vec::new(),
            notes: Vec::new(),
            steps: Vec::new(),
            answer: None,
            status: Status::Running,
            duration_ms: 0,
        });
    }

    /// Close the open turn, if any.
    fn finish_turn(&mut self, status: Status) {
        let Some(mut turn) = self.turn.take() else {
            return;
        };
        // A call still in flight when the turn ends never resolved. Rendered
        // as `Running` rather than dropped: a tool that hung is exactly what
        // a reader is looking for.
        //
        // Taken rather than drained in place: `admit` charges the row budget
        // once per call, and a live `drain` would still hold the `&mut self`
        // that charge needs.
        let elapsed = self.elapsed_ms;
        for (_, call) in std::mem::take(&mut self.pending) {
            if self.admit() {
                turn.steps.push(Step {
                    call: Some(call),
                    accounting: Accounting::default(),
                    offset_ms: elapsed,
                });
            }
        }
        turn.duration_ms = self.elapsed_ms;
        self.elapsed_ms = 0;
        if turn.status == Status::Running {
            turn.status = if turn.steps.iter().any(|s| s.status() == Status::Error) {
                Status::Error
            } else {
                status
            };
        }
        self.run.turns.push(turn);
    }

    /// Render one event onto the open turn.
    fn push_event(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::TextDelta { delta } => self.append_answer(delta),
            AgentEvent::Text { text } => self.set_answer(text),
            AgentEvent::Reasoning { delta } => self.append_prose(delta),
            AgentEvent::ToolStart {
                call, sub_agent_id, ..
            } => self.open_call(call, sub_agent_id.clone()),
            AgentEvent::ToolResult {
                call_id,
                output,
                duration_ms,
                speculated,
                sub_agent_id,
                ..
            } => self.close_call(
                call_id,
                output,
                *duration_ms,
                *speculated,
                sub_agent_id.as_deref(),
            ),
            AgentEvent::StepUsage {
                input_tokens,
                output_tokens,
                cached_input_tokens,
                cost_usd,
                duration_ms,
                ..
            } => self.charge(
                *input_tokens,
                *output_tokens,
                *cached_input_tokens,
                *cost_usd,
                *duration_ms,
            ),
            AgentEvent::FileChange {
                path,
                added,
                removed,
                diff,
                minimal,
                ..
            } => self.attach_file(path, *added, *removed, diff.as_deref(), *minimal),
            // A terminator closes work that already exists; `push_note` would
            // have nothing to attribute it to once the turn it describes is
            // gone, and the cost/model it carries is already in the turn's
            // own accounting rollup.
            AgentEvent::TurnComplete { .. } | AgentEvent::RunComplete { .. } => {
                self.finish_turn(Status::Ok);
            }
            AgentEvent::Error { .. } => {
                self.push_note(event);
                if let Some(turn) = self.turn.as_mut() {
                    turn.status = Status::Error;
                }
            }
            other => self.push_note(other),
        }
    }

    fn set_answer(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(turn) = self.turn.as_mut() {
            turn.answer = Some(text.to_string());
        }
    }

    fn append_answer(&mut self, delta: &str) {
        if let Some(turn) = self.turn.as_mut() {
            turn.answer.get_or_insert_with(String::new).push_str(delta);
        }
    }

    fn append_prose(&mut self, delta: &str) {
        let step = self.turn.as_ref().map_or(0, |t| t.steps.len());
        let Some(turn) = self.turn.as_mut() else {
            return;
        };
        match turn.prose.last_mut() {
            Some(last) if last.before_step == step => last.text.push_str(delta),
            _ => turn.prose.push(Prose {
                text: delta.to_string(),
                before_step: step,
            }),
        }
    }

    fn open_call(&mut self, call: &stella_protocol::ToolCall, sub_agent_id: Option<String>) {
        let tool = ToolKind::from_name(&call.name);
        self.tool_names.insert(
            call.call_id.clone(),
            (call.name.clone(), sub_agent_id.clone()),
        );
        let entry = Call {
            header_object: header_object(&tool, &call.input),
            args: arg_rows(&call.input),
            tool,
            output: Output::default(),
            files: Vec::new(),
            status: Status::Running,
            duration_ms: 0,
            speculated: false,
            sub_agent_id,
        };
        self.pending.push((call.call_id.clone(), entry));
    }

    /// Attach a result to the call its `call_id` opened — or, when this
    /// journal never recorded that call's start (module doc, property 3),
    /// render the result on its own rather than dropping it.
    fn close_call(
        &mut self,
        call_id: &str,
        output: &ToolOutput,
        duration_ms: u64,
        spec: bool,
        sub_agent_id: Option<&str>,
    ) {
        let (status, text) = match output {
            ToolOutput::Ok { content, .. } => (Status::Ok, content.as_str()),
            ToolOutput::Error { message, .. } => (Status::Error, message.as_str()),
        };
        let offset_ms = self.elapsed_ms;
        self.elapsed_ms += duration_ms;

        if let Some(index) = self.pending.iter().position(|(id, _)| id == call_id) {
            let (_, mut call) = self.pending.remove(index);
            call.status = status;
            call.duration_ms = duration_ms;
            call.speculated = spec;
            call.output = Output::from_text(&clip(text));
            self.commit_step(Step {
                call: Some(call),
                accounting: Accounting::default(),
                offset_ms,
            });
            return;
        }

        let (name, started_agent) = self
            .tool_names
            .get(call_id)
            .cloned()
            .unwrap_or_else(|| ("tool".to_string(), None));
        let call = Call {
            header_object: String::new(),
            args: Vec::new(),
            tool: ToolKind::from_name(&name),
            output: Output::from_text(&clip(text)),
            files: Vec::new(),
            status,
            duration_ms,
            speculated: spec,
            sub_agent_id: sub_agent_id.map(str::to_string).or(started_agent),
        };
        self.commit_step(Step {
            call: Some(call),
            accounting: Accounting::default(),
            offset_ms,
        });
    }

    /// Bill a model call to the step it paid for — the last completed step,
    /// or a call-less step of its own when the turn has none yet (a plain
    /// answer with no tool calls still costs tokens).
    fn charge(&mut self, input: u64, output: u64, cached: u64, cost_usd: f64, duration_ms: u64) {
        let accounting = Accounting {
            tokens_in: input,
            tokens_out: output,
            cached_in: cached,
            micros: (cost_usd * 1_000_000.0).round().max(0.0) as u64,
        };
        let offset_ms = self.elapsed_ms;
        self.elapsed_ms += duration_ms;
        let has_step = matches!(&self.turn, Some(t) if !t.steps.is_empty());
        if has_step {
            if let Some(step) = self.turn.as_mut().and_then(|t| t.steps.last_mut()) {
                step.accounting = step.accounting.merged(accounting);
            }
        } else {
            self.commit_step(Step {
                call: None,
                accounting,
                offset_ms,
            });
        }
    }

    /// Hang a file change on the step that produced it — the engine emits it
    /// as its own event, correlated only by coming right after the call that
    /// made it.
    fn attach_file(
        &mut self,
        path: &str,
        added: u32,
        removed: u32,
        diff: Option<&str>,
        minimal: bool,
    ) {
        let status = if removed > 0 && added == 0 {
            FileStatus::Deleted
        } else if removed == 0 && added > 0 {
            FileStatus::New
        } else {
            FileStatus::Modified
        };
        let change = FileChange {
            path: path.to_string(),
            before: String::new(),
            after: String::new(),
            status,
            extent: Extent::delta(added as usize, removed as usize),
            patch: diff.map(|text| Patch {
                text: clip(text),
                minimal,
            }),
        };
        if let Some(turn) = self.turn.as_mut()
            && let Some(call) = turn.steps.last_mut().and_then(|s| s.call.as_mut())
        {
            call.files.push(change);
        }
    }

    /// Record an event the backbone has no slot for as a [`Note`] carrying
    /// its wire tag and its whole payload (module doc, property 3).
    fn push_note(&mut self, event: &AgentEvent) {
        let tag = event_tag(event);
        let kind = note_kind(event);
        let detail = detail_lines(event);
        let step = self.turn.as_ref().map_or(0, |t| t.steps.len());
        self.commit_note(Note {
            kind,
            summary: tag,
            detail,
            before_step: step,
            inspect: None,
        });
    }

    /// Increment the row budget, or count the overflow. Never mutates a turn
    /// itself — callers commit only after this returns `true`.
    fn admit(&mut self) -> bool {
        if self.emitted >= MAX_ROWS {
            self.overflow += 1;
            false
        } else {
            self.emitted += 1;
            true
        }
    }

    fn commit_step(&mut self, step: Step) {
        if !self.admit() {
            return;
        }
        if let Some(turn) = self.turn.as_mut() {
            turn.steps.push(step);
        }
    }

    fn commit_note(&mut self, note: Note) {
        if !self.admit() {
            return;
        }
        if let Some(turn) = self.turn.as_mut() {
            turn.notes.push(note);
        }
    }

    fn finish(mut self, unparseable: usize) -> Transcript {
        self.finish_turn(Status::Ok);
        let mut run = self.run;
        let redacted = redact_run(&mut run);
        let page = html::render_page(&run, &FoldState::new());
        Transcript {
            html: page,
            rendered: self.emitted,
            unparseable,
            overflow: self.overflow,
            redacted,
        }
    }
}

/// Mask any credential anywhere in the built run (module doc, property 1).
///
/// Every [`stella_transcript::model`] type round-trips through `serde_json`
/// byte for byte (invariant #4), so this serializes the whole tree, redacts
/// every string value in it, and deserializes back — the same shape
/// [`super::redact_json_strings`] already applies to a table dump, over a
/// [`Run`] instead of a raw JSON array. A round trip that fails (this struct
/// carries no float, map, or other value `serde_json` could refuse) must not
/// ship an unverified document, so the run is replaced with a note stating
/// the withholding rather than guessed at.
fn redact_run(run: &mut Run) -> bool {
    let Ok(mut value) = serde_json::to_value(&*run) else {
        *run = withheld_run(run.name.clone());
        return true;
    };
    let redacted = super::redact_json_strings(&mut value);
    if redacted {
        match serde_json::from_value::<Run>(value) {
            Ok(cleaned) => *run = cleaned,
            Err(_) => *run = withheld_run(run.name.clone()),
        }
    }
    redacted
}

/// The run substituted when [`redact_run`] cannot verify its own redaction —
/// safe because it carries none of the session's own content.
fn withheld_run(name: String) -> Run {
    Run {
        name,
        model: String::new(),
        started_at: String::new(),
        turns: vec![Turn {
            name: "redaction-failed".into(),
            prompt: String::new(),
            prose: Vec::new(),
            notes: vec![Note {
                kind: NoteKind::Other,
                summary: "the transcript could not be verified as redacted, so it was withheld"
                    .into(),
                detail: Vec::new(),
                before_step: 0,
                inspect: None,
            }],
            steps: Vec::new(),
            answer: None,
            status: Status::Error,
            duration_ms: 0,
        }],
    }
}

/// Which class of thing a [`Note`] records.
///
/// [`stella_tui::transcript_build`] maps events the same way for the live
/// surface, but its fallback narrates through
/// [`stella_tui::textline::event_line`] and drops what that returns `None`
/// for. This one keeps every kind. `NoteKind` only picks a glyph and a
/// colour here — the two fallbacks are kept apart on purpose.
fn note_kind(event: &AgentEvent) -> NoteKind {
    match event {
        AgentEvent::Stage { .. } => NoteKind::Stage,
        AgentEvent::ContextRecall { .. }
        | AgentEvent::ContextWrite { .. }
        | AgentEvent::MemoryLogged { .. }
        | AgentEvent::MemoryPromoted { .. }
        | AgentEvent::SkillInjected { .. }
        | AgentEvent::Compaction { .. } => NoteKind::Context,
        AgentEvent::BudgetTick { .. }
        | AgentEvent::ProviderFallback { .. }
        | AgentEvent::Retry { .. } => NoteKind::Meter,
        AgentEvent::TurnParked { .. }
        | AgentEvent::TurnWoken { .. }
        | AgentEvent::AskUser { .. }
        | AgentEvent::Steered { .. } => NoteKind::Wait,
        AgentEvent::Verdict { .. }
        | AgentEvent::GoalVerdict { .. }
        | AgentEvent::ScopeReview { .. }
        | AgentEvent::HunkReview { .. }
        | AgentEvent::TaskUpdate { .. }
        | AgentEvent::GateBoard { .. }
        | AgentEvent::Proof { .. } => NoteKind::Verdict,
        AgentEvent::SubAgent { .. }
        | AgentEvent::Commit { .. }
        | AgentEvent::Pr { .. }
        | AgentEvent::MediaProgress { .. }
        | AgentEvent::MediaComplete { .. } => NoteKind::Handoff,
        _ => NoteKind::Other,
    }
}

/// A short turn name from its prompt. Mirrors
/// [`stella_tui::transcript_build`]'s own `slug` (private to that crate, so
/// duplicated rather than shared — the same reasoning `diff_hunk_rows` used
/// to give for its own small duplication before this module's diff rendering
/// moved to the shared renderer entirely).
fn slug(prompt: &str) -> String {
    const MAX_WORDS: usize = 4;
    const MAX_CHARS: usize = 28;
    let mut out = String::new();
    for word in prompt.split_whitespace().take(MAX_WORDS) {
        let word: String = word
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if word.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(&word.to_lowercase());
        if out.chars().count() >= MAX_CHARS {
            break;
        }
    }
    out.chars().take(MAX_CHARS).collect()
}

/// The object of the verb: the command line for `bash`, the path for a file
/// tool, the query for a search. Falls back to the first string argument for
/// a tool this crate has never seen — an MCP tool must render as a useful
/// one-liner, not as a bare name. Mirrors
/// `crates/stella-observatory/src/transcript_view.rs::header_object`, the
/// worked example of a store-backed fold into this model.
fn header_object(tool: &ToolKind, input: &serde_json::Value) -> String {
    let key = match tool {
        ToolKind::Bash => "command",
        ToolKind::ReadFile | ToolKind::WriteFile | ToolKind::EditFile | ToolKind::DeleteFile => {
            "path"
        }
        ToolKind::Search => "query",
        ToolKind::Other(_) => "",
    };
    if let Some(found) = input.get(key).and_then(serde_json::Value::as_str) {
        return found.to_string();
    }
    input
        .as_object()
        .and_then(|map| map.values().find_map(serde_json::Value::as_str))
        .unwrap_or_default()
        .to_string()
}

/// Every argument as a display row. Whichever one [`header_object`] already
/// printed is dropped later by [`Call::extra_args`], so this does not have
/// to know which key that was.
fn arg_rows(input: &serde_json::Value) -> Vec<ArgRow> {
    let Some(map) = input.as_object() else {
        return Vec::new();
    };
    map.iter()
        .map(|(key, value)| ArgRow {
            key: key.clone(),
            value: match value {
                serde_json::Value::String(text) => text.clone(),
                other => other.to_string(),
            },
        })
        .collect()
}

/// The wire tag an event serializes under — the same token the raw stream
/// carries, so a reader can grep `raw/` for a note they saw on the page.
fn event_tag(event: &AgentEvent) -> String {
    match serde_json::to_value(event) {
        Ok(serde_json::Value::Object(map)) => map
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("event")
            .to_string(),
        _ => "event".to_string(),
    }
}

/// An event's whole payload, pretty-printed and capped, one line per
/// [`Note::detail`] row.
fn detail_lines<T: Serialize>(value: &T) -> Vec<String> {
    let json = serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".into());
    clip(&json).lines().map(str::to_string).collect()
}

/// Truncate to [`MAX_EMBEDDED_BYTES`], stating the cut.
///
/// Cuts on a character boundary — the byte budget is a size limit, and
/// slicing a UTF-8 sequence in half would panic on exactly the multi-byte
/// output (a tree-drawing tool, a non-English file) least likely to appear
/// in a test.
fn clip(text: &str) -> String {
    if text.len() <= MAX_EMBEDDED_BYTES {
        return text.to_string();
    }
    let mut end = MAX_EMBEDDED_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let dropped = text.len() - end;
    format!(
        "{}\n… {} bytes truncated — the complete row is in this archive's raw/ dumps",
        &text[..end],
        super::comma(dropped as i64)
    )
}

#[cfg(test)]
mod tests;
