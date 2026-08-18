//! Fold an [`AgentEvent`] stream into a [`stella_transcript::Run`].
//!
//! This is the seam that lets a *live* surface draw through the shared
//! transcript model. `stella-transcript` is a near-leaf — it deliberately does
//! not depend on `stella-protocol`, so it cannot fold events itself — and both
//! live surfaces already speak `AgentEvent`. So the fold lives here, one layer
//! up, and `stella-cli`'s plain surface reaches it through this crate the same
//! way it already reaches [`crate::textline`] and [`crate::markdown`].
//!
//! # Why not fold from `SessionModel`
//!
//! The obvious shortcut is to convert the deck's own [`crate::model::SessionModel`],
//! which has already folded the same stream. It loses the accounting: nothing
//! in `SessionModel` retains `StepUsage`, so every token and cost chip on every
//! step row would render empty — and those chips are most of what the digest
//! rows exist to carry. Folding the events directly keeps them.
//!
//! # Wording is not re-invented here
//!
//! Every non-backbone event becomes a [`Note`], worded by
//! [`crate::textline::event_line`] — the same function the plain surface and
//! the deck already print. A note therefore reads identically to the line it
//! replaces, and a new event kind is worded once, in the one place that has
//! always worded it.

use stella_protocol::{AgentEvent, ToolOutput};
use stella_transcript::model::{
    Accounting, Call, FileChange, FileStatus, Note, NoteKind, Output, Prose, Run, Status, Step,
    ToolKind, Turn,
};

use crate::textline;

/// Accumulates events into a [`Run`].
///
/// A pure fold: `push` is the only mutator and it reads nothing but the event,
/// which is what makes the whole thing testable without a terminal or a model
/// (invariant #2, and the same L-T1 discipline the deck renders under).
#[derive(Debug)]
pub struct RunBuilder {
    run: Run,
    /// The turn currently accumulating, if a prompt has opened one.
    turn: Option<Turn>,
    /// Calls dispatched but not yet resolved, oldest first. A tool result is
    /// matched by `call_id` rather than by position, because speculative
    /// execution lets results arrive out of order.
    pending: Vec<(String, Call)>,
    /// Wall time attributed to the open turn so far. The stream carries no
    /// clock — a renderer that read one would stop being a pure fold — so the
    /// turn's elapsed time is summed from the durations the events themselves
    /// report, which is also what makes it replay identically.
    elapsed_ms: u64,
}

impl RunBuilder {
    /// A builder for a run with the given display name and model.
    #[must_use]
    pub fn new(name: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            run: Run {
                name: name.into(),
                model: model.into(),
                started_at: String::new(),
                turns: Vec::new(),
            },
            turn: None,
            pending: Vec::new(),
            elapsed_ms: 0,
        }
    }

    /// Open a new turn around a user prompt.
    ///
    /// Separate from [`Self::push`] because a prompt is not an `AgentEvent` —
    /// the engine emits events *about* a turn, never the turn's own opening,
    /// so the caller that has the prompt text supplies it. Any turn still open
    /// is closed first.
    pub fn start_turn(&mut self, prompt: impl Into<String>) {
        self.finish_turn(Status::Ok);
        let prompt = prompt.into();
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

    /// Fold one event in.
    pub fn push(&mut self, event: &AgentEvent) {
        // Every arm below needs a turn to write into. A stream that starts
        // mid-turn (an attach, a replay from an offset) still has to render,
        // so open an anonymous one rather than dropping the event.
        if self.turn.is_none() {
            self.start_turn(String::new());
        }

        match event {
            AgentEvent::Text { text, .. } => self.set_answer(text),
            AgentEvent::TextDelta { delta, .. } => self.append_answer(delta),
            AgentEvent::Reasoning { delta } => self.append_prose(delta),
            AgentEvent::ToolStart { call } => self.open_call(call),
            AgentEvent::ToolResult {
                call_id,
                output,
                duration_ms,
                speculated,
                ..
            } => self.close_call(call_id, output, *duration_ms, *speculated),
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
                ..
            } => self.attach_file(path, *added, *removed, diff.as_deref()),
            AgentEvent::TurnComplete { .. } => self.finish_turn(Status::Ok),
            AgentEvent::Error { .. } => {
                self.note(event);
                if let Some(turn) = self.turn.as_mut() {
                    turn.status = Status::Error;
                }
            }
            other => self.note(other),
        }
    }

    /// The run so far, with any open turn included as `Running`.
    ///
    /// Non-consuming on purpose: a live surface re-renders on every event, and
    /// a builder that had to be finished to be read could not be watched.
    #[must_use]
    pub fn snapshot(&self) -> Run {
        let mut run = self.run.clone();
        if let Some(turn) = &self.turn {
            let mut turn = turn.clone();
            turn.steps.extend(self.pending.iter().map(|(_, call)| Step {
                call: Some(call.clone()),
                accounting: Accounting::default(),
                offset_ms: 0,
            }));
            run.turns.push(turn);
        }
        run
    }

    /// Close the open turn, if any.
    pub fn finish_turn(&mut self, status: Status) {
        let Some(mut turn) = self.turn.take() else {
            return;
        };
        // A call still in flight when the turn ends never resolved. It is
        // rendered as `Running` rather than dropped: a tool that hung is
        // exactly what a reader is looking for.
        for (_, call) in self.pending.drain(..) {
            turn.steps.push(Step {
                call: Some(call),
                accounting: Accounting::default(),
                offset_ms: 0,
            });
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

    // ---------------------------------------------------------------- arms

    fn set_answer(&mut self, text: &str) {
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
        // Reasoning arrives as deltas; append to the block still being written
        // for this step rather than making one block per token.
        match turn.prose.last_mut() {
            Some(last) if last.before_step == step => last.text.push_str(delta),
            _ => turn.prose.push(Prose {
                text: delta.to_string(),
                before_step: step,
            }),
        }
    }

    fn open_call(&mut self, call: &stella_protocol::ToolCall) {
        let tool = ToolKind::from_name(&call.name);
        self.pending.push((
            call.call_id.clone(),
            Call {
                header_object: header_object(&tool, &call.input),
                tool,
                args: Vec::new(),
                output: Output::from_text(""),
                files: Vec::new(),
                status: Status::Running,
                duration_ms: 0,
                speculated: false,
            },
        ));
    }

    fn close_call(&mut self, call_id: &str, output: &ToolOutput, duration_ms: u64, spec: bool) {
        let Some(index) = self.pending.iter().position(|(id, _)| id == call_id) else {
            return;
        };
        let (_, mut call) = self.pending.remove(index);
        let (status, text) = match output {
            ToolOutput::Ok { content, .. } => (Status::Ok, content.as_str()),
            ToolOutput::Error { message, .. } => (Status::Error, message.as_str()),
        };
        call.status = status;
        call.duration_ms = duration_ms;
        call.speculated = spec;
        call.output = Output::from_text(text);

        let offset_ms = self.elapsed_ms;
        self.elapsed_ms += duration_ms;
        if let Some(turn) = self.turn.as_mut() {
            turn.steps.push(Step {
                call: Some(call),
                accounting: Accounting::default(),
                offset_ms,
            });
        }
    }

    /// Bill a model call to the step it paid for.
    ///
    /// `StepUsage` lands *after* the tool results of the step it describes, so
    /// the charge goes on the last completed step. A step-less turn (a plain
    /// answer with no tool calls) has nowhere to put it and would otherwise
    /// lose the turn's whole cost, so it opens a call-less step — which is
    /// exactly what `Step::call: Option<Call>` is for.
    fn charge(&mut self, input: u64, output: u64, cached: u64, cost_usd: f64, duration_ms: u64) {
        self.elapsed_ms += duration_ms;
        let accounting = Accounting {
            tokens_in: input,
            tokens_out: output,
            cached_in: cached,
            micros: (cost_usd * 1_000_000.0).round().max(0.0) as u64,
        };
        let Some(turn) = self.turn.as_mut() else {
            return;
        };
        match turn.steps.last_mut() {
            Some(step) => step.accounting = step.accounting.merged(accounting),
            None => turn.steps.push(Step {
                call: None,
                accounting,
                offset_ms: 0,
            }),
        }
    }

    /// Hang a file change on the step that produced it.
    fn attach_file(&mut self, path: &str, added: u32, removed: u32, diff: Option<&str>) {
        let status = if removed > 0 && added == 0 {
            FileStatus::Deleted
        } else if removed == 0 && added > 0 {
            FileStatus::New
        } else {
            FileStatus::Modified
        };
        let (before, after) = diff.map_or_else(Default::default, reconstruct);
        let change = FileChange {
            path: path.to_string(),
            before,
            after,
            status,
        };
        if let Some(turn) = self.turn.as_mut()
            && let Some(call) = turn.steps.last_mut().and_then(|s| s.call.as_mut())
        {
            call.files.push(change);
        }
    }

    /// Record any event the backbone has no slot for.
    ///
    /// The wildcard is deliberate and is the degradation [`NoteKind::Other`]
    /// documents: an event this crate has never heard of still reaches the
    /// reader as a muted line, rather than being dropped because nobody
    /// remembered to add an arm.
    fn note(&mut self, event: &AgentEvent) {
        let Some(line) = textline::event_line(event) else {
            return;
        };
        let summary = match &line.detail {
            Some(detail) => format!("{} {}", line.body, detail),
            None => line.body.clone(),
        };
        let step = self.turn.as_ref().map_or(0, |t| t.steps.len());
        if let Some(turn) = self.turn.as_mut() {
            turn.notes.push(Note {
                kind: note_kind(event),
                summary,
                detail: Vec::new(),
                before_step: step,
            });
        }
    }
}

/// A short turn name from its prompt — the `fix-overfull` in the reference
/// frames. The header rail is one line shared with the status word and the
/// chips, so this is deliberately terse; the prompt itself is rendered in full
/// on the `you` row directly below and is never what this replaces.
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

/// Which visual class an event's note belongs to.
fn note_kind(event: &AgentEvent) -> NoteKind {
    match event {
        AgentEvent::Stage { .. } => NoteKind::Stage,
        AgentEvent::ContextRecall { .. }
        | AgentEvent::ContextWrite { .. }
        | AgentEvent::Compaction { .. } => NoteKind::Context,
        AgentEvent::BudgetTick { .. }
        | AgentEvent::BudgetDenied { .. }
        | AgentEvent::ProviderFallback { .. }
        | AgentEvent::Retry { .. }
        | AgentEvent::RetriesExhausted { .. } => NoteKind::Meter,
        AgentEvent::TurnParked { .. } | AgentEvent::TurnWoken { .. } | AgentEvent::AskUser { .. } => {
            NoteKind::Wait
        }
        AgentEvent::Verdict { .. }
        | AgentEvent::GoalVerdict { .. }
        | AgentEvent::ScopeReview { .. }
        | AgentEvent::HunkReview { .. }
        | AgentEvent::TaskUpdate { .. }
        | AgentEvent::Proof { .. } => NoteKind::Verdict,
        AgentEvent::SubAgent { .. }
        | AgentEvent::Commit { .. }
        | AgentEvent::Pr { .. }
        | AgentEvent::MediaProgress { .. }
        | AgentEvent::MediaComplete { .. } => NoteKind::Handoff,
        _ => NoteKind::Other,
    }
}

/// The one-line object a call's header states — the path for a file tool, the
/// command for a shell, and the raw argument blob for anything else.
fn header_object(tool: &ToolKind, input: &serde_json::Value) -> String {
    for key in ["path", "command", "query", "pattern"] {
        if let Some(value) = input.get(key).and_then(serde_json::Value::as_str) {
            return value.to_string();
        }
    }
    let _ = tool;
    input
        .as_str()
        .map(ToString::to_string)
        .unwrap_or_else(|| input.to_string())
}

/// Recover `(before, after)` text from a unified diff.
///
/// [`FileChange`] stores content rather than a diff because the transcript
/// re-derives the diff itself — that is where its word-level highlighting comes
/// from — but the wire carries a rendered unified diff. Each hunk is replayed
/// into the two sides, padded with blank lines up to its `@@` start line so the
/// re-derived hunk headers land on the original line numbers. The padding
/// regions are identical on both sides, so they can never themselves become a
/// hunk.
fn reconstruct(diff: &str) -> (String, String) {
    let mut before: Vec<String> = Vec::new();
    let mut after: Vec<String> = Vec::new();
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("@@") {
            let (old_start, new_start) = hunk_starts(rest);
            pad_to(&mut before, old_start);
            pad_to(&mut after, new_start);
            continue;
        }
        match line.as_bytes().first() {
            Some(b'-') => before.push(line[1..].to_string()),
            Some(b'+') => after.push(line[1..].to_string()),
            Some(b' ') => {
                before.push(line[1..].to_string());
                after.push(line[1..].to_string());
            }
            // `\ No newline at end of file`, `diff --git`, `---`/`+++` headers
            // and blank separators carry no content for either side.
            _ => {}
        }
    }
    (join(before), join(after))
}

/// The `-a,b +c,d` starting line numbers of a hunk header, 1-based.
fn hunk_starts(rest: &str) -> (usize, usize) {
    let mut old = 1;
    let mut new = 1;
    for token in rest.split_whitespace() {
        let number = |t: &str| -> usize {
            t[1..]
                .split(',')
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(1)
        };
        match token.as_bytes().first() {
            Some(b'-') => old = number(token),
            Some(b'+') => new = number(token),
            _ => {}
        }
    }
    (old, new)
}

fn pad_to(side: &mut Vec<String>, start_line: usize) {
    while side.len() + 1 < start_line {
        side.push(String::new());
    }
}

fn join(mut lines: Vec<String>) -> String {
    if lines.is_empty() {
        return String::new();
    }
    lines.push(String::new());
    lines.join("\n")
}

#[cfg(test)]
mod tests;
